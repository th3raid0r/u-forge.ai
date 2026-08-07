SHELL := /usr/bin/env bash

CARGO ?= cargo
TEST_THREADS ?= 1
CARGO_CLIPPY_FLAGS := --workspace --no-deps
COSMIC_TEXT_MANIFEST := crates/cosmic-text-patched/Cargo.toml

ifeq ($(V),1)
define run_silent
$(1)
endef
define run_tests
$(2)
endef
else
define run_silent
@log_file="$$(mktemp)"; \
trap 'rm -f "$$log_file"' EXIT; \
if $(1) >"$$log_file" 2>&1; then \
	:; \
else \
	cat "$$log_file" >&2; \
	exit 1; \
fi
endef
define run_tests
@log_file="$$(mktemp)"; \
trap 'rm -f "$$log_file"' EXIT; \
if $(2) >"$$log_file" 2>&1; then \
	awk '/^test result:/ { \
		suites++; passed += $$4; failed += $$6; ignored += $$8; \
		measured += $$10; filtered += $$12 \
	} END { \
		printf "$(1): %d passed; %d failed; %d ignored; %d measured; %d filtered out across %d suites\n", \
			passed, failed, ignored, measured, filtered, suites \
	}' "$$log_file"; \
else \
	cat "$$log_file" >&2; \
	exit 1; \
fi
endef
endif

.PHONY: help build check test fmt fmt-check clippy startup-profile-fresh startup-profile-configured

help:
	@printf '%s\n' 'u-forge.ai development targets'
	@printf '%s\n' '  make build      Build the workspace'
	@printf '%s\n' '  make check      Check required targets and run workspace clippy'
	@printf '%s\n' '  make test       Test the workspace serially by default'
	@printf '%s\n' '  make fmt        Format the workspace'
	@printf '%s\n' '  make fmt-check  Verify workspace formatting'
	@printf '%s\n' '  make clippy     Run strict workspace clippy'
	@printf '%s\n' '  Add V=1 to show full command output'
	@printf '%s\n' '  make startup-profile-fresh       Profile isolated first launch'
	@printf '%s\n' '  make startup-profile-configured  Profile configured metadata readiness'

build:
	$(call run_silent,$(CARGO) build --workspace)

check:
	$(call run_silent,$(CARGO) check -p u-forge-core)
	$(call run_silent,$(CARGO) check -p u-forge-core --example convert_memorymesh)
	$(call run_silent,$(CARGO) check -p u-forge-graph-view --benches)
	$(call run_silent,$(CARGO) clippy $(CARGO_CLIPPY_FLAGS))

test:
	$(call run_tests,workspace,$(CARGO) test --workspace -- --test-threads=$(TEST_THREADS))
	$(call run_tests,cosmic-text,$(CARGO) test --manifest-path $(COSMIC_TEXT_MANIFEST) -- --test-threads=$(TEST_THREADS))

fmt:
	$(call run_silent,$(CARGO) fmt --all)
	$(call run_silent,$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all)

fmt-check:
	$(call run_silent,$(CARGO) fmt --all -- --check)
	$(call run_silent,$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all -- --check)

clippy:
	$(call run_silent,$(CARGO) clippy $(CARGO_CLIPPY_FLAGS) -- -D warnings)

startup-profile-fresh:
	bash ./scripts/profile-startup.sh fresh

startup-profile-configured:
	bash ./scripts/profile-startup.sh configured
