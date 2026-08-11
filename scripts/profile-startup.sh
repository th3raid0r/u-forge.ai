#!/usr/bin/env bash
set -euo pipefail

scenario=${1:-}
case "$scenario" in
  fresh|configured) ;;
  *)
    echo "usage: $0 fresh|configured" >&2
    exit 2
    ;;
esac

repo_root=$(cd "$(dirname "$0")/.." && pwd)
binary="$repo_root/target/release/u-forge"
report_dir="$repo_root/target/startup-profiles"
mkdir -p "$report_dir"
report="$report_dir/$scenario-$(date -u +%Y%m%dT%H%M%SZ).jsonl"

cargo build --release -p u-forge --manifest-path "$repo_root/Cargo.toml"

export UFORGE_STARTUP_PROFILE="$scenario"
export UFORGE_STARTUP_PROFILE_OUTPUT="$report"
export RUST_LOG=${RUST_LOG:-info}

profile_root=$(mktemp -d)
trap 'rm -rf -- "$profile_root"' EXIT
export XDG_CONFIG_HOME="$profile_root/config"
export XDG_DATA_HOME="$profile_root/data"
export XDG_CACHE_HOME="$profile_root/cache"

if [[ "$scenario" == fresh ]]; then
  export UFORGE_STARTUP_EXIT_AFTER=setup_first_paint
  (cd "$profile_root" && "$binary")
else
  export UFORGE_STARTUP_EXIT_AFTER=lemonade_metadata_ready
  (cd "$profile_root" && "$binary")
fi

echo "Startup profile written to $report"
