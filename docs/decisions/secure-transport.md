# ADR: authenticated Rust userspace tunnel outside USB/IP

Date: 2026-11-10. Status: boundary accepted; production security implementation
and dependency/provider selection pending M11.

Default product connections to mutually authenticated encrypted peer transport,
with exporter-approved enrollment and pinned identities. Prefer rustls after
reviewing/pinning its exact crypto-provider graph. Keep unchanged USB/IP bytes
inside the authorized data stream; bind peer-control/data connections to the same
identity, fresh capability and generation before import. Disable 0-RTT mutation,
implicit trust and plaintext downgrade. Local IPC authentication is a different
OS trust boundary and cannot replace remote peer authentication.

M3's SSH/Python byte launcher was a temporary authenticated lab route. It is not
the production adapter, no non-Rust forwarding shim is approved, and no SSH/key
helper is imported into this workspace. Kernel TCP/TLS would expand the trusted
kernel surface; choose Rust userspace networking with bounded driver IPC instead.

An explicit isolated raw-USB/IP interoperability mode may be introduced with
identical exporter allowlisting and no external exposure by default. It is not
fallback behavior when TLS fails. Pairing UX, certificate lifecycle, key storage,
revoke/renewal and protocol feature negotiation need adversarial tests before
security support is claimed. See [security model](../security-model.md).
