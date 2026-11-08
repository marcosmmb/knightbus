# Original implementation and research boundaries

2026-11-08. This is an engineering provenance policy derived from the user's mandatory instructions; no project license has been chosen. Candidate upstream license indications and verification gaps are in [reference-projects.md](reference-projects.md). A permissive upstream license does not override the no-copy requirement.

KnightBus owns and independently authors Rust USB/IP encoding, sessions, forwarding, policy, backends and its intended Windows virtual controller. Research references are not dependencies, vendored code, forks, translated implementations, packet fixtures or bundled drivers. Do not preserve another forwarding engine's architecture verbatim under renamed modules. Designs in the matrix are new functional requirements and state descriptions, with official specification/API references for implementation.

NKS is proprietary under the user's instruction. Only public README feature descriptions were consulted; no implementation source, algorithm, binary or test fixture is eligible for review or reuse. A public GitHub URL is not a source license. Commit metadata and README may be pinned later without inspecting source. Do not use source review to fill the NKS audit gap.

## Permitted dependency classes, subject to review

Normal async, crypto, serialization, OS binding, CLI, diagnostic and testing libraries are candidates: Tokio, rustls, ordinary platform bindings, bindgen and Microsoft's Rust WDK tooling. No versions/dependencies were added in M0. Audit license, maintenance, advisories, provenance, native/transitive libraries and build scripts before selecting/pinning them. A library that embeds a forwarding engine/driver is excluded even if its API looks like an ordinary Rust crate. OS-provided UdeCx/KMDF and Linux vhci facilities are allowed platform infrastructure; they do not authorize bundling an external forwarding application.

Bindings to documented OS ABIs are allowed. If meaningful native non-Rust code or a shim is required, first record the demonstrated constraint, scope, alternatives and maintenance/security costs in a proposed ADR, and obtain an explicit owner decision before adding it. Microsoft tooling may generate ABI bindings; generation alone does not prove UdeCx linkage or supported distribution. Apple SDK calling conventions or DriverKit tooling may require a design decision as well.

## Reference license indications and exclusions

| Project | Evidence-level license indication | Treatment |
| --- | --- | --- |
| Extender | Manifest MIT OR Apache-2.0 | Research only, no forwarding crate dependency |
| sing-usbip | GPL-3.0-or-later in supplied plan; exact text not retrieved | Research only; embedded drivers excluded |
| usbipd-mac | MIT in README/repository | Research only, no Swift port or copied diagnostics |
| AirUSB Hub | Apache-2.0 shown on repository | Research only, no crypto implementation copied |
| usbip-win2 | BSD-2-Clause repository badge; individual files/releases need audit | External oracle only; never shipping importer |
| usbipd-win | Examined AttachedClient.cs explicitly GPL-3.0-only | Research only; its capture driver not adopted |
| berndverst | MIT shown on repository | Research only; neither custom protocol nor driver copied |
| usbredir | Examined header LGPL-2.1-or-later; repository GPL utilities | No parser/host linkage or protocol substitution |
| wolfusb | MIT in supplied plan; unverified license | README ideas only so far; no dependency |
| LAN USB Bridge | Apache-2.0 README | Reimplement leases/policy independently; no external-engine wrapper |
| OpenUSBIP | GPL-3.0 README; exact text unverified | Functional UX ideas, no GUI/source transplant |
| NKS | Proprietary per mandatory instruction | Public README-level features only |
| windows-drivers-rs | Build script/README MIT OR Apache-2.0 | General-purpose tooling candidate, not a forwarding engine |
| Linux | Protocol SPEC read; implementation license audit not performed | Independent codec; externally installed stock OS peer |

Official documentation is implementation authority, not permission to copy implementation samples wholesale. Write independent tests from stated field constraints and OS behavior. Do not copy sample packets, descriptors or proprietary device data from upstream fixtures. Lab-generated descriptors must strip personal identity from published artifacts.

## Provenance procedure for subsequent milestones

For each requirement retain the source URL, exact commit where applicable, evidence class, independently chosen behavior and original test. Keep source-review notes at a functional level. Separate optional peer installations from KnightBus install packages and dependency graphs; record peer license/version/hash before running them. Before release inspect source inventory, Cargo.lock/build scripts, installer contents, native link map, SBOM and driver artifacts. Fail review if an external forwarding component enters the shipped path, regardless of license compatibility. No license or patent conclusion is implied by this engineering inventory.

M0 added documentation only. No third-party source, fixture, dependency manifest, binary, driver package or installer was added.
