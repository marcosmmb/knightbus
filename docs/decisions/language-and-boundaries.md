# ADR: original Rust workspace with inward dependencies

Status: accepted; workspace introduced in M5.

Use Rust for the reusable engine and native boundaries. M5 selects edition 2024,
resolver 3 and MSRV/default toolchain 1.85.0, with a committed lockfile. CI checks
1.85.0 and current stable on Linux/macOS/Windows. Forbid unsafe code; lint public
API documentation and Clippy warnings. The only product crate uses std alone.
Fuzzing has its own locked workspace under the crate's `tests/` directory, keeping
its runtime dependencies out of product builds. CI also runs a Linux fuzz smoke.

Separate portable contracts, the protocol codec, engine, host adapters and native
backends. Add packages only when their milestone supplies a meaningful behavior
and dependency boundary. Both hosts use one engine; no placeholder packages are
required in M4. Platform APIs and GUI dependencies must not enter portable types.

Userspace FFI must contain ordinary unwinding panics; review panic profiles with
the adapter implementation. The Windows kernel driver needs a separate workspace,
toolchain and profile for its WDK/no_std requirements.

A monolithic daemon would obscure embedding and privileges, while creating empty
crates for every future module would add maintenance prematurely. Introduce the
smallest useful boundaries as code arrives. Async runtime selection belongs at
composition; prefer an injected runtime/clock interface for testable engine logic.
Tokio remains a candidate, not an adopted dependency.
