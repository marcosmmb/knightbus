# Security and privilege model

M4 established these security requirements. M6 now implements exporter grants,
lease ownership, budget admission and mock host authority checks; it has no TLS,
OS-authenticated listener or native backend. A peer identifier or engine startup
must never itself confer authority. See [M6 boundaries](milestone6-validation.md);
there is no production security-support claim.

## Remote identity and exporter grants

Use an ordinary maintained Rust TLS implementation (rustls preferred, dependency/
provider selection pending audit), mutually authenticated pinned peer keys and
explicit enrollment. Discovery offers addresses and coarse capabilities only.
Pairing requires an exporter-approved, short-lived single-use enrollment token;
never pair implicitly from mDNS or an observed certificate. Persist identity keys
in OS-protected storage; do not log tokens/keys or include them in device metadata.

Separate authenticated peer-control and USB/IP streams bind to the same identity
and a fresh one-use import capability outside standard USB/IP framing. Disable
0-RTT for grant/import mutations and reject stale session generations. The
exporter rechecks grant and physical generation before committing a claim. Grant
revocation, lease expiry and device removal initiate cancellation and release.
No opportunistic plaintext fallback. Raw stock-peer tests require explicit local
opt-in, isolation and an equivalent exporter allowlist; not a product default.

## Local IPC and OS privilege

Service defaults are an unprivileged frontend and a narrowly privileged backend
host/broker. macOS root capture is measured only as a lab primitive; a supported
launchd/helper/entitlement and signing path remains unproved. Windows uses a
separately installed original signed driver; production device ACLs should admit
the intended service identity, not arbitrary network callers or every local user.
Embedded mode uses the same rights and may need an approved narrow helper. Never
run a whole GUI or streaming host as root/administrator to hide a missing design.

For a system Unix service, use a root-owned runtime directory and a socket owned
by the service with restrictive group access (typically 0660). Reject symlink/
path substitution; verify peer credentials with the appropriate OS mechanism
(`getpeereid` on macOS, `SO_PEERCRED` on Linux). Bind the resulting principal to
connection generation and authorize operations; do not trust a supplied UID.
Per-user variants need a protected user runtime directory and cannot gain system
capture rights merely by changing the socket path.

On Windows, use local named pipes with explicit DACLs for SYSTEM/service identity
and authorized management principals, reject remote clients, and validate the
connected client's token/identity through supported pipe authentication. A claimed
PID/name is insufficient. Keep any impersonation tightly scoped and revert it
before USB work. Deployment must choose exact service SID, principal groups and
upgrade ACL behavior in its platform milestone; no permissive default is shipped.

Grant administration, inspection and attaching/releasing one's own sessions are
separate rights. GUI closure leaves daemon-owned sessions alone by default;
explicit application-scoped leases may end at disconnect if exporter policy
selected that behavior. The authority is always the engine/exporter, not a UI cache.

## Untrusted data and retirement

The codec validates framing/lengths before allocation; the engine validates
policy, topology, flags, bounds and generation; the backend revalidates its narrow
FFI/IOCTL inputs. Windows kernel messages contain bounded owned buffers and
monotonic tokens, never user pointers or raw network parsers. Reserve independent
cleanup capacity so saturation cannot prevent cancellation. A data-plane TLS
failure closes the affected generation and starts cleanup, not silent reconnect.

After owner death, stop admission and explicitly complete/cancel owned native
requests. Retain memory until OS callbacks retire; deadline expiry quarantines
resources rather than freeing live storage. Persist only the minimal recovery
journal needed to reconcile ownership before re-export. M1's total framework-
resource retention is a blocking uncertainty for enabling the production importer.
No automatic controller recycling or arbitrary internal queue deletion is approved.

Logs contain bounded timings/counts, phase, sanitized status and opaque IDs only.
Physical descriptors may contain serial strings and input can contain secrets;
raw capture is an explicit local lab activity with its own retention controls.
No production installation may silently depend on SIP off, Secure Boot off or
test-signing. The development license and source audit do not certify OS signing.
