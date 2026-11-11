# Milestone 4 review

M4 is **architecture and decisions only**, following the owner's scope correction.
The original plan's compiling skeletons, workspace and CI deliverables are deferred;
this branch does not claim to meet its original clean-build acceptance criterion.
See [the scope decision](decisions/m4-scope.md).

The documents cover terminology, control/data boundaries, exporter authority,
both hosting modes, platform capabilities, privilege/security boundaries, licensing
and measured M0–M3 inputs. Remaining platform failures stay explicit.

Workspace and codec implementation start in M5; engine behavior belongs to M6;
platform code belongs to its backend milestones; full C/IPC integration belongs
to M12. Code, tests and CI will be reviewed with those implementations.

The earlier bootstrap implementation, manifests, headers, tests, check scripts
and workflow have been removed from the proposed change. Their prior build/test
results do not describe this documentation-only branch. No runtime or hardware
validation is claimed here.

Validation for this revision checks local document links, whitespace and the
absence of implementation files from the proposed additions. Historical research
is preserved without rewriting its old local-evidence references.
