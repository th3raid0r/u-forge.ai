#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
release_dir="$repo_root/target/release"
app_binary="$release_dir/u-forge"
lemonade_dir="$release_dir/lemonade"
defaults_dir="$release_dir/defaults"

test -x "$app_binary" || {
  echo "missing release binary: $app_binary" >&2
  exit 1
}
test -x "$lemonade_dir/lemond" || {
  echo "missing embedded Lemonade runtime: $lemonade_dir/lemond" >&2
  exit 1
}
test -f "$defaults_dir/config/u-forge.toml" || {
  echo "missing staged defaults: $defaults_dir" >&2
  exit 1
}

release_version=${RELEASE_VERSION:-}
if [[ -z "$release_version" ]]; then
  release_version=$(awk '
    /^\[package\]/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version[[:space:]]*=/ {
      gsub(/[[:space:]"]/, "", $0)
      sub(/^version=/, "", $0)
      print
      exit
    }
  ' "$repo_root/crates/u-forge/Cargo.toml")
fi
artifact_version=${release_version#v}
test -n "$artifact_version" || {
  echo "could not determine release version" >&2
  exit 1
}

tool_dir="$repo_root/target/appimage-tools"
tool="$tool_dir/linuxdeploy-x86_64.AppImage"
mkdir -p "$tool_dir"
if [[ ! -f "$tool" ]] || ! (cd "$tool_dir" && sha256sum --check --status "$repo_root/packaging/linuxdeploy-x86_64.sha256"); then
  download="$tool.download"
  curl --fail --location --retry 3 \
    --output "$download" \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
  mv "$download" "$tool"
fi
(cd "$tool_dir" && sha256sum --check "$repo_root/packaging/linuxdeploy-x86_64.sha256")
chmod +x "$tool"

work_dir=$(mktemp -d "$repo_root/target/appimage-build.XXXXXX")
trap 'rm -rf -- "$work_dir"' EXIT
app_dir="$work_dir/u-forge.AppDir"
mkdir -p \
  "$app_dir/usr/bin" \
  "$app_dir/usr/lib/u-forge" \
  "$app_dir/usr/share/u-forge"
install -m 0755 "$app_binary" "$app_dir/usr/bin/u-forge"
cp -a "$lemonade_dir" "$app_dir/usr/lib/u-forge/lemonade"
cp -a "$defaults_dir" "$app_dir/usr/share/u-forge/defaults"

export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH=x86_64
export VERSION="$artifact_version"
export LINUXDEPLOY_OUTPUT_VERSION="$artifact_version"
# The pinned linuxdeploy bundles an older strip that cannot parse RELR sections
# emitted by current distributions. Release binaries are already optimized.
export NO_STRIP=1
desktop-file-validate "$repo_root/packaging/ai.u-forge.u-forge.desktop"
(
  cd "$work_dir"
  "$tool" \
    --appdir "$app_dir" \
    --deploy-deps-only "$app_dir/usr/bin/u-forge" \
    --deploy-deps-only "$app_dir/usr/lib/u-forge/lemonade" \
    --desktop-file "$repo_root/packaging/ai.u-forge.u-forge.desktop" \
    --icon-file "$repo_root/packaging/ai.u-forge.u-forge.svg" \
    --custom-apprun "$repo_root/packaging/AppRun" \
    --output appimage
)

mapfile -t generated_images < <(find "$work_dir" -maxdepth 1 -type f -name '*.AppImage' -print)
if [[ ${#generated_images[@]} -ne 1 ]]; then
  echo "expected one generated AppImage, found ${#generated_images[@]}" >&2
  exit 1
fi

dist_dir="$repo_root/dist"
artifact="$dist_dir/u-forge-$artifact_version-x86_64.AppImage"
mkdir -p "$dist_dir"
install -m 0755 "${generated_images[0]}" "$artifact"
(
  cd "$dist_dir"
  sha256sum "$(basename "$artifact")" > "$(basename "$artifact").sha256"
)

echo "AppImage: $artifact"
echo "Checksum: $artifact.sha256"
