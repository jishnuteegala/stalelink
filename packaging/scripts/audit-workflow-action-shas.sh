#!/usr/bin/env bash
# Resolve every external workflow action pin through GitHub's commits API.

set -euo pipefail

root=$(git rev-parse --show-toplevel)
status=0

while IFS= read -r uses; do
  action=${uses#*@}
  reference=${uses%@*}
  if [[ "$reference" == ./* ]]; then
    printf 'local %s\n' "$uses"
    continue
  fi
  if [[ ! "$action" =~ ^[0-9a-f]{40}$ ]]; then
    printf 'invalid pin %s\n' "$uses" >&2
    status=1
    continue
  fi
  if resolved=$(gh api "repos/$reference/commits/$action" --jq .sha 2>/dev/null) && [[ "$resolved" == "$action" ]]; then
    printf 'ok %s\n' "$uses"
  else
    printf 'missing %s\n' "$uses" >&2
    status=1
  fi
done < <(git -C "$root" ls-files '.github/workflows/*.yml' | xargs rg --no-filename -o 'uses:\s*[^[:space:]]+' | sed -E 's/^uses:[[:space:]]*//')

exit "$status"
