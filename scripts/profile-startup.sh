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
binary="$repo_root/target/release/u-forge-ui-gpui"
report_dir="$repo_root/target/startup-profiles"
mkdir -p "$report_dir"
report="$report_dir/$scenario-$(date -u +%Y%m%dT%H%M%SZ).jsonl"

cargo build --release -p u-forge-ui-gpui --manifest-path "$repo_root/Cargo.toml"

export UFORGE_STARTUP_PROFILE="$scenario"
export UFORGE_STARTUP_PROFILE_OUTPUT="$report"
export RUST_LOG=${RUST_LOG:-info}

if [[ "$scenario" == fresh ]]; then
  profile_root=$(mktemp -d)
  trap 'rm -rf -- "$profile_root"' EXIT
  mkdir -p "$profile_root/config" "$profile_root/data" "$profile_root/xdg-data"
  config_path="$profile_root/u-forge.toml"
  db_path="$profile_root/data/db"
  {
    echo '[storage]'
    printf 'db_path = "%s"\n' "$db_path"
    echo 'embedding_dimensions = 768'
    echo 'high_quality_embedding_dimensions = 4096'
  } > "$config_path"
  export XDG_CONFIG_HOME="$profile_root/config"
  export XDG_DATA_HOME="$profile_root/xdg-data"
  export UFORGE_STARTUP_EXIT_AFTER=setup_first_paint
  (cd "$profile_root" && "$binary")
else
  export UFORGE_STARTUP_EXIT_AFTER=lemonade_metadata_ready
  (cd "$repo_root" && "$binary")
fi

echo "Startup profile written to $report"
