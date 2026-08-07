#!/usr/bin/env bash
# Shared nFPM release package matrix and naming operations.

set -euo pipefail

nfpm_arches() {
  printf '%s\n' amd64 arm64
}

nfpm_target_for_arch() {
  case "$1" in
    amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
    arm64) printf '%s\n' aarch64-unknown-linux-gnu ;;
    *) printf 'unknown nFPM arch: %s\n' "$1" >&2; return 1 ;;
  esac
}

nfpm_formats() {
  printf '%s\n' deb rpm apk archlinux
}

nfpm_extension() {
  case "$1" in
    deb) printf '%s\n' deb ;;
    rpm) printf '%s\n' rpm ;;
    apk) printf '%s\n' apk ;;
    archlinux) printf '%s\n' pkg.tar.zst ;;
    *) printf 'unknown nFPM format: %s\n' "$1" >&2; return 1 ;;
  esac
}

nfpm_package_name() {
  local version=$1 arch=$2 format=$3
  printf 'stalelink-%s-%s.%s\n' "$version" "$arch" "$(nfpm_extension "$format")"
}

build_nfpm_packages() {
  local nfpm=$1 config=$2 binary=$3 output_dir=$4 version=$5 arch=$6 format output
  mkdir -p "$output_dir"
  while IFS= read -r format; do
    output="$output_dir/$(nfpm_package_name "$version" "$arch" "$format")"
    VERSION="$version" ARCH="$arch" BINARY="$binary" "$nfpm" package --config "$config" --packager "$format" --target "$output"
  done < <(nfpm_formats)
}
