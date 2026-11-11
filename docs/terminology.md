# Terminology

| KnightBus term | Meaning |
| --- | --- |
| Exporter | Machine/role physically hosting and authorizing access to a USB device; USB/IP **server** |
| Importer | Machine/role requesting the remote device and exposing it through its native USB stack; USB/IP **client** |
| Physical device | Real USB hardware with a current local connection generation |
| Virtual attachment | Native destination USB device representing remote physical descriptors; not a synthetic gamepad substitute |
| Peer identity | Authenticated cryptographic remote identity; distinct from its address, VID/PID and display name |
| Local principal | OS-authenticated management caller, independent of a remote peer |
| Grant | Exporter administrator's permission for one importer and device selector; not an importer self-authorization |
| Lease | Time-bounded exclusive ownership bound to peer, session and physical generation |
| Generation | Fresh identity for one physical connection or session lifetime; prevents stale completion/reconnect reuse |
| Control plane | Pairing, inventory views, grants, lifecycle, diagnostics and events |
| Data plane | Standard USB/IP management/transfer frames and OS USB request/completion traffic |
| Embedded | Application hosts the same Rust engine through a C ABI/in-process adapter |
| Service | `knightbusd` hosts that engine; clients use authenticated local IPC |
| Capability | Backend/device/role/transfer-direction/process-context ability, with evidence and limits |
| Gate verdict | PASS, CONDITIONAL, BLOCKED or UNPROVEN for a named measured scope; distinct from runtime availability |
| Quarantine | Cleanup is incomplete; refuse new claims until explicit reconciliation succeeds |

Moonlight is a streaming client but the initial **USB exporter** on macOS.
Sunshine is a streaming host but the initial **USB importer** on Windows. USB roles
are not fixed to those applications: both may exist in a standalone engine where
backends permit it. Use exporter/importer in code and product UI; retain standard
USB/IP server/client names when explaining the specification.

USB/IP DEVLIST and IMPORT are management messages on the standard data connection.
KnightBus's richer control API does not replace them or insert custom fields into
their framing. GUI and streaming control IPC never carries URBs or USB reports.
