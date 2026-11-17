.DEFAULT_GOAL := help

CARGO ?= cargo
NPM ?= npm
NPX ?= npx
PYTHON ?= python3
POWERSHELL ?= powershell.exe

DESKTOP_MANIFEST := applications/knightbus-desktop/Cargo.toml
DESKTOP_TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
UI_TEST_DIR := applications/knightbus-desktop/tests
WORKSPACE_FLAGS := --workspace --all-features --locked
DESKTOP_FLAGS := --manifest-path $(DESKTOP_MANIFEST) --locked --target-dir "$(DESKTOP_TARGET_DIR)"
# Keep locally generated packages separate from source and normal Cargo output.
PACKAGE_OUTPUT ?= lab/packages/KnightBus
PACKAGE_TARGET_DIR ?= lab/package-build
PACKAGE_ARGS ?=
ARGS ?=
PLAYWRIGHT_INSTALL_ARGS ?= chromium

.PHONY: help build release cli fmt fmt-check lint test test-release docs check \
	desktop desktop-build desktop-fmt desktop-fmt-check desktop-lint desktop-test \
	desktop-check ui-setup ui-test package-test windows-install-test package

help:
	@printf '%s\n' \
	  'KnightBus development commands (run from the repository root)' \
	  '' \
	  '  make build               Build the Rust workspace' \
	  '  make release             Build the workspace in release mode' \
	  '  make cli ARGS="local-list" Run the CLI with arguments' \
	  '  make fmt                 Format workspace Rust code' \
	  '  make fmt-check           Check workspace formatting' \
	  '  make lint                Run workspace Clippy with warnings as errors' \
	  '  make test                Run workspace tests' \
	  '  make test-release        Run release tests and doctests' \
	  '  make docs                Build API docs with warnings as errors' \
	  '  make check               Run all workspace checks above' \
	  '' \
	  '  make desktop             Build and open the desktop app' \
	  '  make desktop-build       Build desktop and its daemon in release mode' \
	  '  make desktop-fmt         Format desktop Rust code' \
	  '  make desktop-fmt-check   Check desktop formatting' \
	  '  make desktop-lint        Run desktop Clippy with warnings as errors' \
	  '  make desktop-test        Run desktop Rust tests' \
	  '  make desktop-check       Check formatting, lint, test and build desktop' \
	  '' \
	  '  make ui-setup            Install UI test dependencies and Chromium' \
	  '  make ui-test             Run Node and browser tests' \
	  '  make package-test        Run Python packaging tests' \
	  '  make windows-install-test Run installer recovery tests on Windows' \
	  '  make package             Build a native app package and ZIP under lab/' \
	  '' \
	  'See CONTRIBUTING.md for prerequisites, overrides and fuzzing.'

build:
	$(CARGO) build $(WORKSPACE_FLAGS)

release:
	$(CARGO) build $(WORKSPACE_FLAGS) --release

cli:
	$(CARGO) run --locked -p knightbus --features knightbus-sdk/mdns -- $(ARGS)

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

lint:
	$(CARGO) clippy $(WORKSPACE_FLAGS) --all-targets -- -D warnings

test:
	$(CARGO) test $(WORKSPACE_FLAGS) --all-targets

test-release:
	$(CARGO) test $(WORKSPACE_FLAGS) --release

docs:
	RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc $(WORKSPACE_FLAGS) --no-deps

# Run checks in order, even with make -j, to avoid competing Cargo builds.
check:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) test-release
	$(MAKE) docs

# The app locates its companion daemon beside the desktop executable.
desktop:
	$(CARGO) build --locked -p knightbusd --features knightbus-sdk/mdns --target-dir "$(DESKTOP_TARGET_DIR)"
	$(CARGO) run $(DESKTOP_FLAGS) -- $(ARGS)

desktop-build:
	$(CARGO) build --locked -p knightbusd --features knightbus-sdk/mdns --target-dir "$(DESKTOP_TARGET_DIR)" --release
	$(CARGO) build $(DESKTOP_FLAGS) --release

desktop-fmt:
	$(CARGO) fmt --manifest-path $(DESKTOP_MANIFEST)

desktop-fmt-check:
	$(CARGO) fmt --manifest-path $(DESKTOP_MANIFEST) --check

desktop-lint:
	$(CARGO) clippy $(DESKTOP_FLAGS) --all-targets -- -D warnings

desktop-test:
	$(CARGO) test $(DESKTOP_FLAGS)

desktop-check:
	$(MAKE) desktop-fmt-check
	$(MAKE) desktop-lint
	$(MAKE) desktop-test
	$(MAKE) desktop-build

ui-setup:
	$(NPM) ci --prefix $(UI_TEST_DIR)
	$(NPX) --prefix $(UI_TEST_DIR) playwright install $(PLAYWRIGHT_INSTALL_ARGS)

ui-test:
	$(NPM) --prefix $(UI_TEST_DIR) test

package-test:
	$(PYTHON) -m unittest discover -s $(UI_TEST_DIR) -p 'test_*.py'

windows-install-test:
	$(POWERSHELL) -NoProfile -File $(UI_TEST_DIR)/windows-installation.tests.ps1

package:
	$(PYTHON) tools/package.py --output "$(PACKAGE_OUTPUT)" --target-dir "$(PACKAGE_TARGET_DIR)" $(PACKAGE_ARGS)
