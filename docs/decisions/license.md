# ADR: MIT for original KnightBus work

Date: 2026-11-10. Status: accepted by explicit owner choice.

License original KnightBus code and documentation under **MIT only**. Its short,
permissive terms suit the reusable engine and future integration components.
The owner selected MIT instead of the initially proposed dual license. The full
text is in [LICENSE](../../LICENSE); use `MIT` in future package metadata.

M4 has no workspace or adopted dependencies. Candidate ordinary
libraries were reviewed at the license-metadata level on 2026-11-10:
[Tokio](https://raw.githubusercontent.com/tokio-rs/tokio/master/tokio/Cargo.toml)
identifies MIT;
[rustls](https://raw.githubusercontent.com/rustls/rustls/main/rustls/Cargo.toml)
identifies Apache-2.0 OR ISC OR MIT;
[Microsoft Rust WDK tooling](https://raw.githubusercontent.com/microsoft/windows-drivers-rs/main/Cargo.toml)
identifies MIT OR Apache-2.0. These are mutable upstream metadata observations,
not adopted versions, transitive audits or security approval. Before introduction,
pin and audit the actual dependency graph, including crypto providers and SDK
redistribution terms. See [inventory](../../third_party/README.md).

Both [Moonlight Qt](https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/LICENSE)
and [Sunshine](https://raw.githubusercontent.com/LizardByte/Sunshine/master/LICENSE)
publish GPLv3 license texts. Future patches/adapters in those projects must follow
their exact file-level and distribution obligations, including corresponding
source and notices where required. Our permissive license does not relicense
host code. Using IPC is not an automatic exemption from integration/bundling
obligations. Review the exact
integration and release packages in M14–M17; no blanket legal compatibility claim
is made for unreviewed dependencies or future distributions.

Alternative: GPL-only for the entire engine. Not selected because the engine is
an independently reusable SDK as well as a future integration component. No
license choice permits importing another forwarding implementation contrary to
the original-code policy. Third-party/OS materials retain their own terms.
