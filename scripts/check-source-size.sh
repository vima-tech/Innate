#!/usr/bin/env bash
set -euo pipefail

# Always run from the repo root so the find paths below resolve.
cd "$(dirname "$0")/.."

warn_limit="${SOURCE_FILE_WARN_LINES:-800}"
fail_limit="${SOURCE_FILE_MAX_LINES:-1200}"
failed=0

while IFS= read -r -d '' file; do
  lines="$(wc -l < "$file")"
  if (( lines > fail_limit )); then
    printf 'error: %s has %d lines (maximum %d)\n' "$file" "$lines" "$fail_limit" >&2
    failed=1
  elif (( lines > warn_limit )); then
    printf 'warning: %s has %d lines (review above %d)\n' "$file" "$lines" "$warn_limit" >&2
  fi
done < <(
  find core/src sdks \
    -type f \
    \( -name '*.rs' -o -name '*.py' -o -name '*.ts' \) \
    -print0
)

exit "$failed"
