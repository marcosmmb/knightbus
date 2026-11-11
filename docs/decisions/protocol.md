# ADR: standard USB/IP 1.1.1 data plane

Date: 2026-11-10. Status: accepted; implementation starts M5.

Use the published
[Linux USB/IP protocol](https://www.kernel.org/doc/html/latest/usb/usbip_protocol.html)
for management and transfer messages, preserving framing, endian rules and
submit/unlink completion semantics. Independently author the codec and fixtures.
No KnightBus identity, policy, lease or event fields are added to standard frames.
Authentication and session binding occur in an outer tunnel; rich peer control is
separate. The codec owns no sockets, OS handles, UI or policy authority.

M3 used a narrow non-ISO subset; it is evidence for the path, not a production
codec or proof of stock-peer interoperability. Do not transplant discarded spike
code as a reviewed M5 implementation. M5 must cover fragmented/coalesced streams,
checked lengths, control/bulk/interrupt/ISO structures and independently generated
negative tests. Native capability support remains separate from codec coverage.

A custom VUSB-style protocol or HID-only replacement would miss native-driver
compatibility and the owner's full USB requirement; neither is an alternative
implementation of this milestone.
