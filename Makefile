SHELL := /usr/bin/env bash

CARGO ?= cargo
TEST_THREADS ?= 1
CARGO_CLIPPY_FLAGS := --workspace --no-deps
COSMIC_TEXT_MANIFEST := crates/cosmic-text-patched/Cargo.toml

.PHONY: help build check test fmt fmt-check clippy

help:
	@printf '%s\n' 'u-forge.ai development targets'
	@printf '%s\n' '  make build      Build the workspace'
	@printf '%s\n' '  make check      Check required targets and run workspace clippy'
	@printf '%s\n' '  make test       Test the workspace serially by default'
	@printf '%s\n' '  make fmt        Format the workspace'
	@printf '%s\n' '  make fmt-check  Verify workspace formatting'
	@printf '%s\n' '  make clippy     Run strict workspace clippy'

build:
	$(CARGO) build --workspace

check:
	$(CARGO) check -p u-forge-core
	$(CARGO) check -p u-forge-core --example convert_memorymesh
	$(CARGO) check -p u-forge-graph-view --benches
	$(CARGO) clippy $(CARGO_CLIPPY_FLAGS)

test:
	$(CARGO) test --workspace -- --test-threads=$(TEST_THREADS)
	$(CARGO) test --manifest-path $(COSMIC_TEXT_MANIFEST) -- --test-threads=$(TEST_THREADS)

fmt:
	$(CARGO) fmt --all
	$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all

fmt-check:
	$(CARGO) fmt --all -- --check
	$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all -- --check

clippy:
	$(CARGO) clippy $(CARGO_CLIPPY_FLAGS) -- -D warnings
