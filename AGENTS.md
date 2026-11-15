# Lab and development artifacts

Put lab-only tools, manual hardware fixtures, diagnostic programs, development
signing/install helpers, and their generated artifacts in the repository-root
`lab/` directory. That directory is gitignored. Keep lab-only hooks out of
production code and do not make normal builds or CI depend on `lab/`.

Keep ordinary automated regression tests, fuzz tests, and product build tooling
in the tracked codebase. Lab tooling and tests requiring a specially prepared
machine belong in `lab/`.

Lab tools must depend on the actual crates or invoke their built executables.
Do not copy importer, exporter, shared-crate, or driver implementations into
`lab/`, and do not generate alternate source workspaces or restore lab hooks.

Keep public documentation focused on usage, architecture and contributions.
Put development notes, validation reports and hardware instructions in `lab/`.
Do not include milestones or internal project tracking in public documentation.

# Numeric values

Do not use magic numbers. Use named constants for limits, timeouts, intervals,
buffer sizes, protocol widths and offsets; use enums for numeric tags or states.
Reuse existing definitions and share constants when multiple components must agree
on a value. Briefly explain non-obvious values or how they are derived. Ordinary
zero/one values and self-explanatory test data may remain numeric literals.
