#!/usr/bin/env python3
"""Capture macOS sampled-stack flame graphs for both Kilo implementations.

Requires sample, llvm-cxxfilt (for Rust v0 symbols), and rsvg-convert.
These are wall-clock stack samples, including syscall/wait stacks, NOT CPU-only.
"""

import argparse
from collections import Counter
from dataclasses import dataclass, field
import hashlib
from html import escape
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from benchmark import PtySession  # noqa: E402


@dataclass
class Node:
    symbol: str
    count: int = 0
    children: list = field(default_factory=list)


def parse_sample(text):
    """Convert inclusive macOS call-tree counts into exclusive folded stacks."""
    if 'Call graph:\n' not in text:
        raise ValueError('Missing call graph in sampler output')
    graph = text.split('Call graph:\n', 1)[1]
    roots, parents = [], []
    for line in graph.splitlines():
        match = re.match(r'^([ +|:!\.]*)(\d+) (.+)$', line)
        if not match:
            if not line.strip():
                continue
            if line.startswith(('Total number in stack', 'Sort by top of stack', 'Binary Images:')):
                break
            raise ValueError(f'Unrecognized call graph line: {line!r}')
        prefix, count, symbol = match.groups()
        depth = len(prefix)
        if symbol.startswith('???'):
            raise ValueError('Unresolved stack symbols: generate a dSYM and capture again')
        symbol = symbol.split('  (in ', 1)[0]
        if symbol.startswith('Thread_'):
            symbol = '[main thread]' if not roots else '[thread]'
        while parents and parents[-1][0] >= depth:
            parents.pop()
        node = Node(symbol, int(count))
        if parents:
            parents[-1][1].children.append(node)
        else:
            roots.append(node)
        parents.append((depth, node))
    if not roots:
        raise ValueError('No sample stacks parsed')
    folded = Counter()

    def walk(node, path):
        path = path + (node.symbol,)
        own = node.count - sum(child.count for child in node.children)
        if own < 0:
            raise ValueError(f'Inconsistent inclusive counts at {node.symbol}')
        if own:
            folded[path] += own
        for child in node.children:
            walk(child, path)

    for root in roots:
        walk(root, ())
    if sum(folded.values()) != sum(root.count for root in roots):
        raise ValueError('Folded stacks did not preserve the sample total')
    return folded


def demangle(folded, executable):
    symbols = sorted({s for path in folded for s in path})
    result = subprocess.run([executable], input='\n'.join(symbols) + '\n',
                            text=True, capture_output=True, check=True)
    decoded = result.stdout.splitlines()
    if len(decoded) != len(symbols):
        raise ValueError('Demangler changed the number of symbols')
    lookup = dict(zip(symbols, decoded))
    normalized = Counter()
    for path, count in folded.items():
        path = tuple(lookup[s].replace(';', ':') for s in path)
        # Remove common process startup wrappers above the application entry point.
        entry = next((i for i, s in enumerate(path) if s in ('main', 'kilo_rs::main')), None)
        if entry is not None:
            # Rust also has a C main shim before its Rust entry point.
            rust_entry = next((i for i, s in enumerate(path) if s == 'kilo_rs::main'), None)
            path = path[rust_entry if rust_entry is not None else entry:]
        normalized[path] += count
    return normalized


def flame_svg(folded, title, duration, interval, width=2200):
    tree = {'count': 0, 'children': {}}
    for path, count in folded.items():
        tree['count'] += count
        node = tree
        for symbol in path:
            node = node['children'].setdefault(symbol, {'count': 0, 'children': {}})
            node['count'] += count
    total = tree['count']
    if not total:
        raise ValueError('Cannot render an empty flame graph')
    depth = max(map(len, folded)) + 1
    row_height = 25
    height = 185 + depth * row_height
    baseline = height - 65
    output = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        '<rect width="100%" height="100%" fill="#fffaf5"/>',
        '<style>text{font-family:Menlo,monospace;fill:#292524}.frame{stroke:#fffaf5;stroke-width:0.6}</style>',
        f'<text x="30" y="44" font-size="29" font-weight="bold">{escape(title)}</text>',
        f'<text x="30" y="76" font-size="16">macOS sample • {duration} s • {interval} ms interval • {total:,} stack samples</text>',
        '<text x="30" y="102" font-size="16">Width = share of sampled stacks, including terminal I/O waits. Each graph is normalized independently.</text>',
        f'<text x="30" y="{height-24}" font-size="14">Bottom → top: caller → callee. Blue/gray = syscall or synchronization frame; warm colors = other functions.</text>',
    ]

    def draw(node, symbol, x, level, rect_width):
        y = baseline - level * row_height
        digest = hashlib.sha256(symbol.encode()).digest()
        if symbol in ('read', 'write', '__read_nocancel', '__write_nocancel') or 'pthread_' in symbol or 'ulock' in symbol:
            color = '#93b6c7'
        else:
            color = f'rgb({225 + digest[0] % 25},{110 + digest[1] % 100},{65 + digest[2] % 65})'
        label = f"{symbol} — {node['count']:,} samples ({node['count']/total*100:.1f}%)"
        output.append(f'<g><title>{escape(label)}</title><rect class="frame" x="{x:.3f}" y="{y}" width="{rect_width:.3f}" height="24" fill="{color}"/>')
        if rect_width > 28:
            max_chars = int((rect_width - 9) / 7.2)
            text = label if len(label) <= max_chars else symbol[:max(0, max_chars - 1)] + '…'
            output.append(f'<text x="{x+4:.3f}" y="{y+17}" font-size="12">{escape(text)}</text>')
        output.append('</g>')
        cursor = x
        for name, child in sorted(node['children'].items(), key=lambda item: (-item[1]['count'], item[0])):
            child_width = rect_width * child['count'] / node['count']
            draw(child, name, cursor, level + 1, child_width)
            cursor += child_width

    draw(tree, 'all samples', 30, 0, width - 60)
    output.append('</svg>')
    return '\n'.join(output), height


def line_position(frame):
    matches = re.findall(rb'(\d+)/(\d+)', frame)
    if not matches:
        raise ValueError('No logical cursor position in completed frame')
    return tuple(map(int, matches[-1]))


def symbolicate_live(text, pid):
    pattern = re.compile(r'\?\?\?  \(in [^)]+\).*?\[(0x[0-9a-f]+)\]')
    addresses = sorted({match[1] for match in pattern.finditer(text)})
    if not addresses:
        return text
    result = subprocess.run(['atos', '-p', str(pid), *addresses], text=True,
                            capture_output=True, check=True)
    symbols = result.stdout.splitlines()
    if len(symbols) != len(addresses) or any('(in ' not in s for s in symbols):
        raise ValueError('Live atos symbol lookup did not resolve all addresses')
    lookup = {address: symbol.split(' (in ', 1)[0] for address, symbol in zip(addresses, symbols)}
    return pattern.sub(lambda match: match[0].replace('???', lookup[match[1]], 1), text)


def capture(binary, target, output, duration, interval):
    with PtySession(binary, target) as session:
        session.frame()
        session.send(b'\x1b[6~')
        last = session.frame()
        for _ in range(10):
            session.send(b'\x1b[B')
            last = session.frame()
        start_line, total_lines = line_position(last)
        if total_lines < start_line + 512:
            raise ValueError('Input must have enough lines for the 512-line scroll cycle')
        log_path = output.with_suffix('.log')
        with log_path.open('w') as log:
            profiler = subprocess.Popen(
                ['sample', str(session.process.pid), str(duration), str(interval), '-file', str(output.resolve())],
                stdout=log, stderr=subprocess.STDOUT,
            )
            frames = 0
            cycles = 0
            deadline = time.monotonic() + duration + 40
            try:
                while profiler.poll() is None:
                    for key, direction in ((b'\x1b[B', 1), (b'\x1b[A', -1)):
                        for batch in range(8):
                            session.send(key * 64)
                            for _ in range(64):
                                last = session.frame()
                                frames += 1
                            expected = start_line + (batch + 1) * 64 if direction > 0 else start_line + 512 - (batch + 1) * 64
                            if line_position(last) != (expected, total_lines):
                                raise ValueError('Scroll workload did not reach its expected logical position')
                            if time.monotonic() > deadline:
                                raise TimeoutError('Sampler exceeded its time budget')
                    cycles += 1
                if profiler.returncode != 0:
                    raise RuntimeError(log_path.read_text())
            finally:
                if profiler.poll() is None:
                    profiler.kill()
                profiler.wait(timeout=5)
        resolved = symbolicate_live(output.read_text(), session.process.pid)
        output.with_suffix('.symbolicated.txt').write_text(resolved)
        session.send(b'\x11')
        if session.wait_exit() != 0:
            raise RuntimeError('Editor did not exit cleanly')
        return {'acknowledged_workload_frames': frames, 'completed_cycles': cycles,
                'line_before': start_line, 'line_after': line_position(last)[0],
                'file_lines': total_lines, 'exit_code': session.process.returncode}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--c-bin', type=Path, default=ROOT / 'target/profiling-c/kilo')
    parser.add_argument('--rust-bin', type=Path, default=ROOT / 'target/profiling/release/kilo-rs')
    parser.add_argument('--file', type=Path, default=ROOT.parent / 'kilo/kilo.c')
    parser.add_argument('--output', type=Path, default=ROOT / 'outputs/flames')
    parser.add_argument('--duration', type=int, default=8)
    parser.add_argument('--interval', type=int, default=1)
    parser.add_argument('--demangler', default=shutil.which('llvm-cxxfilt') or '/opt/homebrew/opt/llvm/bin/llvm-cxxfilt')
    parser.add_argument('--render-only', action='store_true')
    args = parser.parse_args()
    if args.duration <= 0 or args.interval <= 0:
        parser.error('Duration and interval must be positive')
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {'metric': 'macOS wall-clock sampled stacks, including blocked I/O; not CPU-only',
                'duration_seconds': args.duration, 'interval_ms': args.interval,
                'workload': '24x80 PTY; PageDown + 10 Down warmup; repeated 512 Down / 512 Up; batches of 64, every frame acknowledged',
                'target_sha256': hashlib.sha256(args.file.read_bytes()).hexdigest(), 'profiles': {}}
    svgs = []
    for name, binary in [('C', args.c_bin), ('Rust', args.rust_bin)]:
        slug = name.lower()
        sample_path = args.output / f'{slug}.sample.txt'
        stats = {} if args.render_only else capture(binary.resolve(strict=True), args.file.resolve(strict=True), sample_path, args.duration, args.interval)
        folded = demangle(parse_sample(sample_path.with_suffix('.symbolicated.txt').read_text()), args.demangler)
        folded_path = args.output / f'{slug}.folded'
        folded_path.write_text(''.join(f"{';'.join(path)} {count}\n" for path, count in sorted(folded.items())))
        svg, height = flame_svg(folded, f'{name} implementation — sampled-stack flame graph', args.duration, args.interval)
        svg_path = args.output / f'{slug}-flamegraph.svg'
        svg_path.write_text(svg)
        subprocess.run(['rsvg-convert', str(svg_path), '-o', str(args.output / f'{slug}-flamegraph.png')], check=True)
        leaves = Counter()
        for path, count in folded.items():
            leaves[path[-1]] += count
        stats.update({'samples': sum(folded.values()), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                      'top_sampled_leaf_frames': leaves.most_common(12)})
        manifest['profiles'][name] = stats
        svgs.append((svg, height))
        print(f'{name}: {sum(folded.values()):,} samples; PNG created', flush=True)
    combined = [f'<svg xmlns="http://www.w3.org/2000/svg" width="2200" height="{sum(h for _, h in svgs)}">']
    offset = 0
    for svg, height in svgs:
        combined.append(f'<g transform="translate(0,{offset})">{svg[svg.index(chr(62))+1:svg.rfind("</svg>")]}</g>')
        offset += height
    combined.append('</svg>')
    comparison = args.output / 'comparison.svg'
    comparison.write_text('\n'.join(combined))
    subprocess.run(['rsvg-convert', str(comparison), '-o', str(args.output / 'comparison.png')], check=True)
    if not args.render_only:
        (args.output / 'profile-results.json').write_text(json.dumps(manifest, indent=2) + '\n')


if __name__ == '__main__':
    main()
