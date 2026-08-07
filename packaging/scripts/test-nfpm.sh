#!/usr/bin/env bash
set -euo pipefail

nfpm=${1:-nfpm}
root=$(git rev-parse --show-toplevel)
source "$root/packaging/scripts/nfpm-lib.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf '#!/bin/sh\nexit 0\n' > "$tmp/stalelink"
chmod +x "$tmp/stalelink"

while IFS= read -r arch; do
  build_nfpm_packages "$nfpm" "$root/packaging/nfpm.yaml" "$tmp/stalelink" "$tmp/$arch" 0.1.0 "$arch"
  while IFS= read -r format; do
    output="$tmp/$arch/$(nfpm_package_name 0.1.0 "$arch" "$format")"
    test -s "$output"
  done < <(nfpm_formats)
done < <(nfpm_arches)

while IFS= read -r format; do
  documented="stalelink-\${v#v}-amd64.$(nfpm_extension "$format")"
  grep -Fq "$documented" "$root/README.md"
done < <(nfpm_formats)
