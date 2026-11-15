"""Measure complete redraw latency for Kilo-compatible editors over a real PTY."""

import argparse
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import statistics
import struct
import subprocess
import termios
import time


class PtySession:
    """A bounded PTY session shared by benchmarks and integration tests."""

    FRAME_END = b"\x1b[?25h"

    def __init__(self, binary, target, rows=24, cols=80):
        self.master, self.slave = pty.openpty()
        self.pending = bytearray()
        self.process = None
        try:
            self.resize(rows, cols, notify=False)
            self.original_attributes = termios.tcgetattr(self.slave)
            self.process = subprocess.Popen(
                [str(Path(binary).resolve()), str(target)],
                stdin=self.slave, stdout=self.slave, stderr=self.slave,
                start_new_session=True,
            )
        except BaseException:
            self.close()
            raise

    def resize(self, rows, cols, notify=True):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        if notify and self.process is not None:
            self.process.send_signal(signal.SIGWINCH)

    def send(self, keys):
        while keys:
            written = os.write(self.master, keys)
            keys = keys[written:]

    def frame(self, timeout=5):
        """Read through cursor-show, which marks completion of a rendered frame."""
        deadline = time.monotonic() + timeout
        while True:
            end = self.pending.find(self.FRAME_END)
            if end >= 0:
                end += len(self.FRAME_END)
                result = bytes(self.pending[:end])
                del self.pending[:end]
                return result
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("Editor did not complete a frame within the timeout")
            readable, _, _ = select.select([self.master], [], [], min(remaining, 0.05))
            if readable:
                try:
                    chunk = os.read(self.master, 65536)
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
                    chunk = b""
                if not chunk:
                    raise RuntimeError("Editor closed its PTY before completing a frame")
                self.pending.extend(chunk)
            elif self.process.poll() is not None:
                raise RuntimeError(
                    f"Editor exited with status {self.process.returncode}: "
                    f"{bytes(self.pending)!r}"
                )

    def wait_exit(self, timeout=5):
        # tcsetattr(TCSAFLUSH) can wait for unread PTY output on macOS.
        # Keep draining while waiting, including on startup errors.
        deadline = time.monotonic() + timeout
        while self.process.poll() is None:
            if time.monotonic() >= deadline:
                raise TimeoutError("Editor did not exit within the timeout")
            readable, _, _ = select.select([self.master], [], [], 0.01)
            if readable:
                try:
                    self.pending.extend(os.read(self.master, 65536))
                except OSError as exc:
                    if exc.errno != errno.EIO:
                        raise
        return self.process.returncode

    def close(self):
        if self.process is not None and self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)
        for name in ("master", "slave"):
            fd = getattr(self, name, None)
            if fd is not None:
                os.close(fd)
                setattr(self, name, None)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def benchmark_binary(bin_path, target_file, num_scrolls=200):
    if num_scrolls <= 0:
        raise ValueError("num_scrolls must be positive")
    with PtySession(bin_path, target_file) as session:
        session.frame()  # Exclude startup, and verify that the editor actually launched.
        started = time.perf_counter()
        for _ in range(num_scrolls):
            session.send(b"\x1b[B")
            session.frame()  # Wait for the full redraw, not an arbitrary output chunk.
        elapsed_ms = (time.perf_counter() - started) * 1000
        session.send(b"\x11")
        session.wait_exit()
        if session.process.returncode != 0:
            raise RuntimeError(f"Editor exited with status {session.process.returncode}")
        return elapsed_ms


def positive_int(value):
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return parsed


def main():
    root = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binaries", nargs="+", type=Path, help="Kilo-compatible executable(s)")
    parser.add_argument("--target", type=Path, default=root / "src/editor.rs")
    parser.add_argument("--scrolls", type=positive_int, default=200)
    parser.add_argument("--runs", type=positive_int, default=3)
    args = parser.parse_args()
    if not args.target.is_file():
        parser.error(f"Target is not a file: {args.target}")
    for binary in args.binaries:
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"Binary is not executable: {binary}")
    try:
        for binary in args.binaries:
            samples = [benchmark_binary(binary, args.target, args.scrolls) for _ in range(args.runs)]
            print(
                f"{binary}: {statistics.mean(samples):.2f} ms mean for {args.scrolls} redraws "
                f"({len(samples)} runs), {binary.stat().st_size / 1024:.1f} KiB binary"
            )
    except (OSError, RuntimeError, TimeoutError, subprocess.TimeoutExpired) as exc:
        parser.exit(1, f"Benchmark failed: {exc}\n")


if __name__ == "__main__":
    main()
