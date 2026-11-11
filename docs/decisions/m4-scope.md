# ADR: architecture-only M4 with open platform gates

Status: accepted under the owner's latest scope instruction.

The original development plan includes minimal compiling skeletons, a Cargo
workspace and CI in M4. After reviewing the initial implementation, the owner
requested no code at this stage and review of code in its respective milestones.
This instruction narrows M4 to architecture, decisions and repository documentation.

Defer workspace creation and basic CI to M5 with the protocol codec. Implement
engine/control behavior in M6, native backends in their platform milestones and
the full C/IPC SDK in M12. Do not retain placeholder runtime code, headers or
tests solely to make the original M4 clean-build criterion appear satisfied.

The earlier request to proceed with M4 permits architecture work despite the M3
report's overall UNPROVEN platform gate. Preserve all measured statuses in
[gate status](../gate-status.md). This is a sequencing and scope decision, not a
resource-safety waiver, native shim approval or CONDITIONAL production pass.
Backend activation and release still require their own gate closure.
