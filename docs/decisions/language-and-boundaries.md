# ADR: original Rust workspace with inward dependencies

Status: architecture direction accepted; workspace creation deferred to M5.

Use Rust for the reusable engine and native boundaries. Select the Rust edition,
minimum compiler, Cargo resolver and lint configuration when introducing the
workspace, with a lockfile and CI that test the selected versions.

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
