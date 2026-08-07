#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
source "$root/packaging/scripts/channel-lib.sh"
fixture="$root/packaging/fixtures/cargo-dist-assets.txt"
checksum_fixture="$root/packaging/fixtures/sha256.sum"

for asset in $(cat "$fixture"); do
  case "$asset" in
    *.sha256|sha256.sum|*.tar.gz|*.ps1|*.sh) continue ;;
  esac
  if [[ "$asset" == stalelink-* ]]; then
    require_release_checksum "$checksum_fixture" "$asset" >/dev/null
  fi
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
x64=stalelink-x86_64-pc-windows-msvc.zip
arm64=stalelink-aarch64-pc-windows-msvc.zip
render_scoop_manifest "$tmp/stalelink.json" 0.1.0 "https://example.test/$x64" "$(require_release_checksum "$checksum_fixture" "$x64")" "https://example.test/$arm64" "$(require_release_checksum "$checksum_fixture" "$arm64")"
node "$root/packaging/scripts/validate-scoop-manifest.js" "$tmp/stalelink.json"
render_pkgbuild "$root/packaging/aur/PKGBUILD.template" "$tmp/PKGBUILD" 0.1.0 "$(require_release_checksum "$checksum_fixture" stalelink-x86_64-unknown-linux-gnu.tar.xz)"
bash -n "$tmp/PKGBUILD"
