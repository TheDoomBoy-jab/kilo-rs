# Checked benchmark results

Run date: November 15, 2026 (Asia/Kolkata). Host: Darwin 24.5.0, arm64. Rust: `rustc 1.98.0 (88d9e12ae 2026-08-18)`.

The release binary was rebuilt with `cargo build --offline --release`. The supplied `benchmark-checked.py` was copied unchanged from `/Users/aahannayak/Documents/Codex/2026-11-15/can-x20/outputs/benchmark-checked.py` into this directory.

Command run from the repository root:

```sh
python3 outputs/benchmark-checked.py > outputs/benchmark-checked-results.json
```

Defaults selected the existing C binary `/Users/aahannayak/kilo/kilo_stripped`, the rebuilt Rust binary `/Users/aahannayak/kilo-rs/target/release/kilo-rs`, and `/Users/aahannayak/kilo/kilo.c` as input.

| Binary | Median ms | Min ms | Max ms | Sample standard deviation ms | Binary bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| C | 13.220 | 11.929 | 15.401 | 1.162 | 69,104 |
| Rust | 6.482 | 5.768 | 8.433 | 0.877 | 502,944 |

Each trial measured 200 Down-key input-to-complete-frame round trips on a 24×80 PTY after startup, one PageDown and 10 warmup Down keys. There were seven trials per binary, with order randomized using seed 0. Every trial acknowledged 212 total frames, including 200 measured frames, moved from line 54 to 254 of 1,308, ended at the scrolling boundary, and exited with status 0.

Rust's median was 51.0% lower (2.04× relative throughput) for this workload. C emitted 282,050 measured frame bytes per trial; Rust emitted 273,411. The renderings are not byte-for-byte identical.

This is end-to-end PTY wall time. Startup, terminal-emulator painting, isolated renderer CPU time, allocations and peak memory were not measured. The C binary was pre-existing and stripped; the Rust release binary was not stripped. Compiler/build settings for C were not independently reproduced, so binary sizes and timings do not represent a controlled compiler comparison. System load was not isolated.

Raw measurements and SHA-256 hashes for both binaries and the input file are retained in [benchmark-checked-results.json](benchmark-checked-results.json).

Validation of the fixes: 14 Rust unit tests and 10 executable PTY integration tests passed on this macOS host. Formatting and Clippy with warnings denied also passed. Linux was not exercised in this environment.
