#!/usr/bin/env bash
# Shared release-channel operations. Workflows and offline fixtures source this
# file so CI validates the same checksum parsing and manifest rendering.

set -euo pipefail

release_checksum() {
  local checksum_file=$1 asset=$2
  awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$checksum_file"
}

require_release_checksum() {
  local checksum_file=$1 asset=$2 checksum
  checksum="$(release_checksum "$checksum_file" "$asset")"
  test -n "$checksum" || { printf 'missing checksum for %s in %s\n' "$asset" "$checksum_file" >&2; return 1; }
  printf '%s\n' "$checksum"
}

# The prerelease rule release.yml generates for publish jobs; dist does not
# apply it to post-announce jobs, so channel workflows apply it themselves.
release_allowed() {
  jq -r '(.announcement_is_prerelease | not) or .publish_prereleases' <<<"$1" | tr -d '\r'
}

verify_release_artifacts() {
  local dir=$1 plan=$2 names name
  names="$(jq -er '.releases[].artifacts[]' <<<"$plan" | tr -d '\r')" || return 1
  for name in $names; do
    test -s "$dir/$name" || { printf 'release artifact %s is missing or empty\n' "$name" >&2; return 1; }
  done
  names="$(jq -er '.artifacts[] | select(.kind == "executable-zip") | .name' <<<"$plan" | tr -d '\r')" || return 1
  for name in $names; do
    require_release_checksum "$dir/sha256.sum" "$name" >/dev/null || return 1
  done
  names="$(jq -er '.artifacts[] | select(.kind == "checksum" or .kind == "unified-checksum") | .name' <<<"$plan" | tr -d '\r')" || return 1
  for name in $names; do
    # dist ends every checksum file with a blank line, which --strict rejects.
    (cd "$dir" && grep . "$name" | sha256sum --check --strict) || { printf '%s does not verify\n' "$name" >&2; return 1; }
  done
}

# Republishing an unchanged version must succeed, so an identical tree is not committed.
commit_channel_update() {
  local repo=$1 message=$2
  shift 2
  git -C "$repo" config user.name 'github-actions[bot]'
  git -C "$repo" config user.email 41898282+github-actions[bot]@users.noreply.github.com
  git -C "$repo" add -- "$@"
  git -C "$repo" diff --cached --quiet || git -C "$repo" commit -m "$message"
}

render_scoop_manifest() {
  local output=$1 version=$2 x64_url=$3 x64_sha256=$4 arm64_url=$5 arm64_sha256=$6
  node - "$output" "$version" "$x64_url" "$x64_sha256" "$arm64_url" "$arm64_sha256" <<'NODE'
const fs = require("node:fs");
const [output, version, x64Url, x64Hash, arm64Url, arm64Hash] = process.argv.slice(2);
fs.writeFileSync(output, `${JSON.stringify({
  version,
  description: "Find dead and outdated links in local documents.",
  homepage: "https://github.com/jishnuteegala/stalelink",
  license: "MIT",
  architecture: {
    "64bit": { url: x64Url, hash: x64Hash },
    arm64: { url: arm64Url, hash: arm64Hash },
  },
  bin: "stalelink.exe",
}, null, 2)}\n`);
NODE
}

render_pkgbuild() {
  local template=$1 output=$2 version=$3 sha256=$4
  sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$sha256/g" "$template" > "$output"
}
