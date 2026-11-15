#!/usr/bin/env python3
"""Compare the existing Kilo binaries using acknowledged PTY redraws.

This measures end-to-end input-to-frame latency over a PTY, not isolated render
CPU time, terminal-emulator painting, memory usage, or allocation counts.
"""
import argparse
import errno
import hashlib
import json
import os
import pty
import random
import re
import select
import statistics
import subprocess
import termios
import time
from pathlib import Path

FRAME_END = b"\x1b[?25h"  # Both local editors end each complete frame this way.
DOWN = b"\x1b[B"
PAGE_DOWN = b"\x1b[6~"


class Frames:
    def __init__(self, fd, process, timeout):
        self.fd, self.process, self.timeout = fd, process, timeout
        self.pending = b""
        self.frames = 0

    def next(self):
        deadline = time.perf_counter() + self.timeout
        while FRAME_END not in self.pending:
            remaining = deadline - time.perf_counter()
            if remaining <= 0:
                raise TimeoutError("No complete redraw before deadline")
            ready, _, _ = select.select([self.fd], [], [], remaining)
            if not ready:
                raise TimeoutError("No complete redraw before deadline")
            try:
                chunk = os.read(self.fd, 65536)
            except OSError as exc:
                if exc.errno != errno.EIO:
                    raise
                chunk = b""
            if not chunk:
                self.process.poll()
                raise RuntimeError(
                    f"Editor closed the PTY; status={self.process.returncode}; "
                    f"output={self.pending[-300:]!r}"
                )
            self.pending += chunk
        frame, _, self.pending = self.pending.partition(FRAME_END)
        self.frames += 1
        return frame + FRAME_END


def position(frame):
    # Status bars are after the file body, so the last n/total match is theirs.
    matches = re.findall(rb"(\d+)/(\d+)", frame)
    if not matches:
        raise RuntimeError("Could not verify the editor's logical line position")
    return tuple(map(int, matches[-1]))


def send(fd, data):
    if os.write(fd, data) != len(data):
        raise RuntimeError("Short PTY input write")


def trial(binary, target, args):
    master, slave = pty.openpty()
    process = None
    try:
        termios.tcsetwinsize(slave, (args.rows, args.cols))
        process = subprocess.Popen(
            [str(binary), str(target)], stdin=slave, stdout=slave, stderr=slave,
            close_fds=True, start_new_session=True,
        )
        os.close(slave)
        slave = -1
        frames = Frames(master, process, args.timeout)
        frames.next()  # Startup and file parsing are explicitly outside timing.
        send(master, PAGE_DOWN)
        last = frames.next()  # Position at the bottom of a viewport.
        for _ in range(args.warmup_keys):
            send(master, DOWN)
            last = frames.next()
        before = position(last)
        output_bytes = 0
        start = time.perf_counter_ns()
        for _ in range(args.keys):
            send(master, DOWN)
            last = frames.next()
            output_bytes += len(last)
        elapsed_ms = (time.perf_counter_ns() - start) / 1_000_000
        after = position(last)
        if after != (before[0] + args.keys, before[1]):
            raise RuntimeError(f"Wrong workload result: {before} -> {after}")
        cursor = re.search(rb"\x1b\[(\d+);(\d+)H\x1b\[\?25h$", last)
        if not cursor or int(cursor[1]) != args.rows - 2:
            raise RuntimeError("Final cursor was not at the scrolling boundary")
        send(master, b"\x11")  # No editing or saving; clean buffers quit once.
        deadline = time.perf_counter() + args.timeout
        while process.poll() is None:
            if time.perf_counter() > deadline:
                raise TimeoutError("Editor did not quit")
            ready, _, _ = select.select([master], [], [], 0.02)
            if ready:
                try:
                    os.read(master, 65536)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
        if process.returncode != 0:
            raise RuntimeError(f"Editor failed with status {process.returncode}")
        return {
            "milliseconds": elapsed_ms, "measured_frames": args.keys,
            "total_acknowledged_frames": frames.frames,
            "measured_output_bytes": output_bytes,
            "line_before": before[0], "line_after": after[0],
            "file_lines": after[1], "exit_code": process.returncode,
        }
    finally:
        if slave != -1:
            os.close(slave)
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
        os.close(master)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--c-bin", type=Path, default=Path("/Users/aahannayak/kilo/kilo_stripped"))
    parser.add_argument("--rust-bin", type=Path, default=Path("/Users/aahannayak/kilo-rs/target/release/kilo-rs"))
    parser.add_argument("--file", type=Path, default=Path("/Users/aahannayak/kilo/kilo.c"))
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--cols", type=int, default=80)
    parser.add_argument("--keys", type=int, default=200)
    parser.add_argument("--trials", type=int, default=7)
    parser.add_argument("--warmup-keys", type=int, default=10)
    parser.add_argument("--timeout", type=float, default=5.0)
    parser.add_argument("--seed", type=int, default=0)
    args = parser.parse_args()
    if min(args.rows, args.cols, args.keys, args.trials) <= 0 or args.rows < 4 or args.warmup_keys < 0 or args.timeout <= 0:
        parser.error("Use rows >= 4, positive dimensions/counts/timeout, and warmup-keys >= 0")
    target = args.file.resolve(strict=True)
    binaries = {"C": args.c_bin.resolve(strict=True), "Rust": args.rust_bin.resolve(strict=True)}
    rng = random.Random(args.seed)
    measurements = {name: [] for name in binaries}
    order = []
    for _ in range(args.trials):
        names = list(binaries)
        rng.shuffle(names)
        for name in names:
            order.append(name)
            measurements[name].append(trial(binaries[name], target, args))
    summary = {}
    for name, runs in measurements.items():
        values = [r["milliseconds"] for r in runs]
        summary[name] = {
            "median_ms": statistics.median(values),
            "min_ms": min(values), "max_ms": max(values),
            "stdev_ms": statistics.stdev(values) if len(values) > 1 else None,
            "binary_bytes": binaries[name].stat().st_size,
            "binary_sha256": hashlib.sha256(binaries[name].read_bytes()).hexdigest(),
            "runs": runs,
        }
    print(json.dumps({
        "metric": "PTY input-to-complete-frame wall time (startup excluded)",
        "warning": "Not isolated renderer CPU time; control system load and rebuild both binaries comparably before publishing results.",
        "terminal": {"rows": args.rows, "cols": args.cols},
        "measured_down_keys_per_trial": args.keys,
        "warmup_down_keys_per_trial": args.warmup_keys,
        "target": str(target),
        "target_sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
        "run_order": order, "results": summary,
    }, indent=2))


if __name__ == "__main__":
    main()
