# ADR: one engine, Embedded and Service hosting

Date: 2026-11-10. Status: both modes required and accepted; **Service is the
initial default** for macOS exporter and Windows importer packaging.

The same `KnightBusEngine` implements policy, sessions and future USB/IP/backends.
Embedded uses `ControlConnection` through `libknightbus` C ABI. Service hosts that
engine in `knightbusd` and exposes the same operations through `IpcControl` over
OS-authenticated Unix sockets/named pipes. The CLI, standalone desktop manager
and C++ streaming integrations consume that control contract. No frontend or
adapter owns a second engine implementation or relays USB data through control IPC.

Service separates privileges and forwarding lifetime from GUI/streaming crashes,
lets signed drivers/helpers be installed independently, and allows multiple
management clients to observe one authoritative engine. Closing a GUI does not
stop sharing unless exporter policy explicitly makes that lease client-scoped.
Embedded remains a first-class supported API direction where process rights allow;
it is not removed because Service is easier to package initially. It requires the
same native backend/driver permissions. Never elevate an entire C++ host merely
to make the Embedded matrix appear supported.

Distribute a reusable Rust library, shared C library/header (optional static
library), daemon, IPC client SDK and CLI separately from optional UI and native
signed drivers/helpers. Future Moonlight/Sunshine adapters stay thin and retain
their host project's language/license. The desktop manager remains optional and
independent; Tauri is preferred for M13 pending packaging review, not a dependency
of this workspace. Headless and SDK builds must not require a GUI framework.

M4 implements neither adapter. [Control API](../control-api.md) records the
requirements for independent C ABI/IPC versions, stable errors/events,
asynchronous operations, ownership and shutdown. Review concrete types, headers,
transport and tests with M6/M12 rather than freezing placeholder APIs here.

M6 requires identical mock policy/session/control scenarios in both modes; M12
requires the same real devices/network/build for hardware parity and benchmarks.
Service adds management IPC, not one IPC crossing per USB transfer. No numeric
performance advantage is assumed. The required Windows driver/user boundary is
present in both modes and needs its own timing/resource evidence.

Alternative: separate daemon/embedded forwarding engines. Rejected because it
duplicates policy, lifetime bugs and protocol logic. Alternative: choose Service
only and defer whether Embedded exists. Rejected by the owner's dual-hosting
requirement. Actual blocked OS/process contexts must be reported, not bypassed.
