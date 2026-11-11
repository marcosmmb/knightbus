# ADR: independently authored forwarding implementation

Date: 2026-11-10. Status: accepted; mandatory owner constraint.

Implement KnightBus protocol, session, policy, forwarding, platform logic and
native virtual controller independently in Rust from the USB/IP specification,
official OS contracts and measured behavior. Extender, sing-usbip and other
forwarding projects are research/interoperability references only. Never copy,
vendor, fork, translate, link or redistribute their forwarding source, packet
fixtures or driver binaries as KnightBus functionality. Permissive licenses do
not override this project boundary. NKS remains public feature-documentation only.

Ordinary async/TLS/serialization/binding/test libraries and official SDK glue are
permitted after source, license, transitive/native and build-script review.
A justified OS-mandated native shim requires a separate explicit ADR/owner
approval before introduction. None is approved. C/C++ test consumers and future
thin integration adapters are foreign API users, not native forwarding shims.

Record provenance beside new dependencies and externally sourced requirements.
Keep stock peers in separate lab installations and out of product artifacts.
M4 includes no code or dependency graph. Add provenance and boundary checks when
the workspace begins in M5. Historical M0 audit gaps remain explicitly open.
