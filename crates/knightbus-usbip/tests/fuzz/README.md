# USB/IP fuzzing

This separate workspace lives beside the crate tests and is excluded from product builds.
`decode` exercises arbitrary wire frames and original-request contexts, requiring
successful decodes to re-encode exactly the consumed prefix. `stream` requires
fragmented decoding to agree with whole-prefix decoding, including failures,
coalesced remainder ownership, EOF and poisoned-state behavior.

Both use bounded fuzz budgets and a 13-byte context prefix: selector u8
(0 management, 1 requests, 2 IN replies, 3 OUT replies), expected sequence u32,
request capacity u32 and ISO count i32, all network-order. Remaining bytes are
wire data. Context is not part of USB/IP. Pure assertions in `support.rs` also run
against fixtures and deterministic noise in the normal offline suite.

```sh
rustup toolchain install nightly-2026-05-25 --profile minimal
cargo +nightly-2026-05-25 install cargo-fuzz --version 0.13.1 --locked
cargo +nightly-2026-05-25 fetch --manifest-path crates/knightbus-usbip/tests/fuzz/Cargo.toml --locked
cargo test -p knightbus-usbip --locked --offline --test protocol write_fuzz_seeds -- --ignored
ASAN_OPTIONS=quarantine_size_mb=32 CARGO_NET_OFFLINE=true cargo +nightly-2026-05-25 fuzz run --fuzz-dir crates/knightbus-usbip/tests/fuzz decode -- -max_total_time=45 -max_len=8192 -rss_limit_mb=512
ASAN_OPTIONS=quarantine_size_mb=32 CARGO_NET_OFFLINE=true cargo +nightly-2026-05-25 fuzz run --fuzz-dir crates/knightbus-usbip/tests/fuzz stream -- -max_total_time=45 -max_len=8192 -rss_limit_mb=512
```

Linux/macOS x86-64 or AArch64 and a C++ compiler are required. Windows still runs
the ordinary corpus/mutation tests. The committed fuzz lockfile pins dependencies;
fetch with `--locked` before running offline because cargo-fuzz 0.13.1 does not
accept a `run --locked` switch. CI verifies that running did not change the lock.
Corpus, coverage, crash artifacts and raw logs stay untracked. Minimize any real
failure and add an original regression test before changing the codec.

The 512 MiB limit includes the sanitizer/runtime; per-input codec budgets are much
smaller. The initial local 256 MiB process cap was exceeded during ASan fuzzing. The
verified configuration uses a 32 MiB quarantine and 512 MiB process limit.
A short smoke run does not establish exhaustive safety.

## Test dependencies

The product crate has zero third-party dependencies. Only this fuzz workspace
uses the packages below, pinned with checksums in `Cargo.lock`. Original code and
fixtures are MIT. Manifests, build scripts and license declarations were reviewed
on 2026-11-10.

| Package | Version | Declared license | Role |
| --- | --- | --- | --- |
| libfuzzer-sys | 0.4.10 | (MIT OR Apache-2.0) AND NCSA | LLVM fuzz runtime wrapper; C++ runtime built by cc |
| arbitrary | 1.5.0 | MIT OR Apache-2.0 | Fuzz runtime input support |
| cc | 1.11.1 | MIT OR Apache-2.0 | Native test runtime compiler driver |
| find-msvc-tools | 0.1.15 | MIT OR Apache-2.0 | Compiler discovery |
| jobserver | 0.1.35 | MIT OR Apache-2.0 | Build concurrency |
| libc | 0.2.191 | MIT OR Apache-2.0 | Host ABI for build/runtime tools |
| getrandom | 0.4.3 | MIT OR Apache-2.0 | Jobserver temporary-name randomness |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | Conditional compilation |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | Target-specific getrandom dependency, not active on tested hosts |
| shlex | 2.0.1 | MIT OR Apache-2.0 | Compiler-argument parsing |

Select MIT where offered; libFuzzer's NCSA notice remains required if redistributing
the test runtime. No test dependency is linked into KnightBus's product library.
Do not present the project's MIT choice as relicensing third-party packages.

`libfuzzer-sys/build.rs` compiles packaged C++ sources and links the platform C++
runtime; it can also honor an explicit custom runtime path. `getrandom/build.rs`
sets a memory-sanitizer cfg. `libc/build.rs` probes compiler/target capabilities
and emits cfgs. These scripts do not fetch forwarding code. No derive/proc-macro
feature was enabled for arbitrary. This is a targeted source/build/license review,
not a comprehensive vulnerability audit of the compiler or every dependency.

cargo-fuzz 0.13.1 (MIT OR Apache-2.0) is an external tool installed with its
published lockfile. Linux USB/IP and Wireshark were external GPL test peers; no
forwarding source, binaries or fixtures are bundled in KnightBus.
