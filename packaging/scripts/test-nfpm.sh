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
  documented="stalelink-\${VERSION}-amd64.$(nfpm_extension "$format")"
  grep -Fq "$documented" "$root/README.md"
done < <(nfpm_formats)

# The workflow matrix must equal the shared arch mapping so neither can drift.
expected=$(while IFS= read -r arch; do
  printf 'target=%s arch=%s\n' "$(nfpm_target_for_arch "$arch")" "$arch"
done < <(nfpm_arches) | sort)
actual=$(node - "$root/.github/workflows/publish-nfpm.yml" <<'EOF'
const fs = require("fs");
const text = fs.readFileSync(process.argv[2], "utf8");
const include = [];
const lines = text.split("\n");
let current = null;
for (const line of lines) {
  const target = line.match(/^\s+- target:\s*(\S+)/);
  const arch = line.match(/^\s+arch:\s*(\S+)/);
  if (target) current = { target: target[1] };
  else if (arch && current) {
    include.push(`target=${current.target} arch=${arch[1]}`);
    current = null;
  }
}
console.log(include.sort().join("\n"));
EOF
)
if [[ "$expected" != "$actual" ]]; then
  printf 'publish-nfpm.yml matrix drifted from nfpm-lib.sh arch mapping\nexpected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
  exit 1
fi
