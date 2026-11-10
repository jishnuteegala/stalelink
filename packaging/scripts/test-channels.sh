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
node "$root/packaging/scripts/package-chocolatey.test.mjs"

expect_fail() {
  if "$@" >/dev/null 2>&1; then
    printf 'expected failure: %s\n' "$*" >&2
    exit 1
  fi
}

test "$(release_allowed '{"announcement_is_prerelease":false,"publish_prereleases":false}')" = true
test "$(release_allowed '{"announcement_is_prerelease":true,"publish_prereleases":false}')" = false
test "$(release_allowed '{"announcement_is_prerelease":true,"publish_prereleases":true}')" = true

plan='{"releases":[{"artifacts":["sha256.sum","a.tar.xz","a.tar.xz.sha256","b.zip","installer.sh"]}],"artifacts":{"sha256.sum":{"name":"sha256.sum","kind":"unified-checksum"},"a.tar.xz":{"name":"a.tar.xz","kind":"executable-zip"},"a.tar.xz.sha256":{"name":"a.tar.xz.sha256","kind":"checksum"},"b.zip":{"name":"b.zip","kind":"executable-zip"},"installer.sh":{"name":"installer.sh","kind":"installer"}}}'
release() {
  rm -rf "$tmp/release"
  mkdir "$tmp/release"
  cd "$tmp/release"
  printf 'a\n' > a.tar.xz
  printf 'b\n' > b.zip
  printf 'installer\n' > installer.sh
  # dist's layout: binary-mode lines followed by a blank line.
  printf '%s *a.tar.xz\n\n' "$(sha256sum a.tar.xz | cut -d' ' -f1)" > a.tar.xz.sha256
  printf '%s *a.tar.xz\n%s *b.zip\n\n' "$(sha256sum a.tar.xz | cut -d' ' -f1)" "$(sha256sum b.zip | cut -d' ' -f1)" > sha256.sum
  cd - >/dev/null
}
release
verify_release_artifacts "$tmp/release" "$plan" >/dev/null
expect_fail verify_release_artifacts "$tmp/release" '{"releases":[{"artifacts":[]}],"artifacts":{}}'
release; rm "$tmp/release/installer.sh"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
release; : > "$tmp/release/installer.sh"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
release; sed -i '/b\.zip/d' "$tmp/release/sha256.sum"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
release; printf 'x\n' >> "$tmp/release/b.zip"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
release; printf 'not a checksum line\n' >> "$tmp/release/sha256.sum"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
release; printf '%064d *a.tar.xz\n' 0 > "$tmp/release/a.tar.xz.sha256"
expect_fail verify_release_artifacts "$tmp/release" "$plan"
