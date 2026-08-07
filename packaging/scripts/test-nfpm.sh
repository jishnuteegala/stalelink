#!/usr/bin/env bash
set -euo pipefail

nfpm=${1:-nfpm}
root=$(git rev-parse --show-toplevel)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf '#!/bin/sh\nexit 0\n' > "$tmp/stalelink"
chmod +x "$tmp/stalelink"

for arch in amd64 arm64; do
  for format in deb rpm apk archlinux; do
    case "$format" in
      deb) extension=deb ;;
      rpm) extension=rpm ;;
      apk) extension=apk ;;
      archlinux) extension=pkg.tar.zst ;;
    esac
    output="$tmp/stalelink-0.1.0-${arch}.${extension}"
    VERSION=0.1.0 ARCH="$arch" BINARY="$tmp/stalelink" "$nfpm" package --config "$root/packaging/nfpm.yaml" --packager "$format" --target "$output"
    test -s "$output"
  done
done
