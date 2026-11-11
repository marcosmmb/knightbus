# Architecture decisions

Accepted here means a design choice within the owner's authorized M4 scope,
not a platform PASS or release approval. Historical gates are in
[gate status](../gate-status.md); unresolved implementation decisions stay explicit.

| Decision | Status |
| --- | --- |
| [M4 scope with open gates](m4-scope.md) | Accepted for documentation only; code/workspace/CI deferred |
| [Rust and package boundaries](language-and-boundaries.md) | Accepted |
| [Original-code policy](original-code-policy.md) | Accepted, mandatory project constraint |
| [License](license.md) | MIT selected for original code |
| [USB/IP protocol](protocol.md) | USB/IP 1.1.1 selected; implementation starts M5 |
| [Secure transport](secure-transport.md) | mTLS/tunnel boundary selected; dependency/provider/pairing details pending |
| [Windows Rust driver](windows-rust-driver.md) | Rust UDE/user-mode split selected; lifecycle and release gates open |
| [Dual hosting and service boundaries](dual-mode-hosting.md) | Both required; Service initial default |
| [macOS backend](macos-backend.md) | Generic USB path selected for continued work; distribution open |
| [Remaining choices](open-decisions.md) | Explicit deferred decisions and closure milestones |

M5 compiler and test-workspace choices are recorded in
[language and boundaries](language-and-boundaries.md).
