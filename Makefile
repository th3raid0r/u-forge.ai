SHELL := /usr/bin/env bash

CARGO ?= cargo
TEST_THREADS ?= 1
CARGO_CLIPPY_FLAGS := --workspace --no-deps
COSMIC_TEXT_MANIFEST := crates/cosmic-text-patched/Cargo.toml
CI_LEMONADE_TEST_FILTERS := \
	--skip lemonade \
	--skip startup_tests::fresh_start_measures_until_setup_is_painted \
	--skip startup_tests::configured_start_measures_metadata_before_activation \
	--skip via_provider_factory \
	--skip two_transcription_workers_compete
CI_OFFLINE_ENV := env \
	-u LEMONADE_URL \
	-u LEMONADE_API_KEY \
	-u LEMONADE_ADMIN_API_KEY \
	-u UFORGE_LEMOND_PATH \
	-u UFORGE_REQUIRE_EMBEDDED_LEMONADE \
	UFORGE_SKIP_EMBEDDED_LEMONADE=1 \
	UFORGE_INTEGRATION_TESTS=skip

ifeq ($(V),1)
define run_silent
@printf '%s\n' '$(1): starting'
$(2)
@printf '%s\n' '$(1): succeeded'
endef
define run_tests
@printf '%s\n' '$(1): starting'
$(2)
@printf '%s\n' '$(1): succeeded'
endef
else
define run_silent
@printf '%s\n' '$(1): starting'
@log_file="$$(mktemp)"; \
trap 'rm -f "$$log_file"' EXIT; \
if $(2) >"$$log_file" 2>&1; then \
	printf '%s\n' '$(1): succeeded'; \
else \
	cat "$$log_file" >&2; \
	exit 1; \
fi
endef
define run_tests
@printf '%s\n' '$(1): starting'
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
	printf '%s\n' '$(1): succeeded'; \
else \
	cat "$$log_file" >&2; \
	exit 1; \
fi
endef
endif

.PHONY: help build check test test-ci _test-with-embedded-lemonade fmt fmt-check clippy startup-profile-fresh startup-profile-configured

help:
	@printf '%s\n' 'u-forge.ai development targets'
	@printf '%s\n' '  make build      Build the workspace'
	@printf '%s\n' '  make check      Check required targets and run workspace clippy'
	@printf '%s\n' '  make test       Test serially with one pinned embedded Lemonade server'
	@printf '%s\n' '  make test-ci    Test serially without provisioning or Lemonade tests'
	@printf '%s\n' '  make fmt        Format the workspace'
	@printf '%s\n' '  make fmt-check  Verify workspace formatting'
	@printf '%s\n' '  make clippy     Run strict workspace clippy'
	@printf '%s\n' '  Add V=1 to show full command output'
	@printf '%s\n' '  make startup-profile-fresh       Profile isolated first launch'
	@printf '%s\n' '  make startup-profile-configured  Profile configured metadata readiness'

build:
	$(call run_silent,workspace build,$(CARGO) build --workspace)

check:
	$(call run_silent,u-forge-core check,$(CARGO) check -p u-forge-core)
	$(call run_silent,u-forge-core benchmark check,$(CARGO) check -p u-forge-core --benches)
	$(call run_silent,convert_memorymesh example check,$(CARGO) check -p u-forge-core --example convert_memorymesh)
	$(call run_silent,u-forge-graph-view benchmark check,$(CARGO) check -p u-forge-graph-view --benches)
	$(call run_silent,workspace clippy check,$(CARGO) clippy $(CARGO_CLIPPY_FLAGS))

test:
	$(call run_silent,workspace test build,env -u UFORGE_SKIP_EMBEDDED_LEMONADE -u UFORGE_LEMOND_PATH UFORGE_REQUIRE_EMBEDDED_LEMONADE=1 $(CARGO) test --workspace --no-run)
	$(call run_silent,cosmic-text test build,$(CARGO) test --manifest-path $(COSMIC_TEXT_MANIFEST) --no-run)
	@env -u UFORGE_LEMOND_PATH $(CARGO) run --quiet -p u-forge-core --example with_embedded_lemonade -- $(MAKE) --no-print-directory _test-with-embedded-lemonade

test-ci:
	$(call run_tests,workspace,$(CI_OFFLINE_ENV) $(CARGO) test --workspace -- --test-threads=$(TEST_THREADS) $(CI_LEMONADE_TEST_FILTERS))
	$(call run_tests,cosmic-text,$(CARGO) test --manifest-path $(COSMIC_TEXT_MANIFEST) -- --test-threads=$(TEST_THREADS))

_test-with-embedded-lemonade:
	@test "$${UFORGE_INTEGRATION_TESTS:-}" = require || { printf '%s\n' 'internal test target requires the embedded Lemonade runner' >&2; exit 2; }
	$(call run_tests,workspace,$(CARGO) test --workspace -- --test-threads=$(TEST_THREADS))
	$(call run_tests,cosmic-text,$(CARGO) test --manifest-path $(COSMIC_TEXT_MANIFEST) -- --test-threads=$(TEST_THREADS))

fmt:
	$(call run_silent,workspace format,$(CARGO) fmt --all)
	$(call run_silent,cosmic-text format,$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all)

fmt-check:
	$(call run_silent,workspace format check,$(CARGO) fmt --all -- --check)
	$(call run_silent,cosmic-text format check,$(CARGO) fmt --manifest-path $(COSMIC_TEXT_MANIFEST) --all -- --check)

clippy:
	$(call run_silent,workspace clippy,$(CARGO) clippy $(CARGO_CLIPPY_FLAGS) -- -D warnings)

startup-profile-fresh:
	bash ./scripts/profile-startup.sh fresh

startup-profile-configured:
	bash ./scripts/profile-startup.sh configured
