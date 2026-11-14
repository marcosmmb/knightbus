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
