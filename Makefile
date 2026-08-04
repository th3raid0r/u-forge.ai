SHELL := /usr/bin/env bash

CARGO ?= cargo
TEST_THREADS ?= 1
CARGO_CLIPPY_FLAGS := --workspace --exclude cosmic-text --no-deps

.PHONY: help build check test fmt fmt-check clippy

help:
	@printf '%s\n' 'u-forge.ai development targets'
	@printf '%s\n' '  make build      Build the workspace'
	@printf '%s\n' '  make check      Check core and run workspace clippy'
	@printf '%s\n' '  make test       Test the workspace serially by default'
	@printf '%s\n' '  make fmt        Format the workspace'
	@printf '%s\n' '  make fmt-check  Verify workspace formatting'
	@printf '%s\n' '  make clippy     Run strict workspace clippy'

build:
	$(CARGO) build --workspace

check:
	$(CARGO) check -p u-forge-core
	$(CARGO) clippy $(CARGO_CLIPPY_FLAGS)

test:
	$(CARGO) test --workspace -- --test-threads=$(TEST_THREADS)

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy $(CARGO_CLIPPY_FLAGS) -- -D warnings
