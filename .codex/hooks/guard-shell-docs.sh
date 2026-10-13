#!/usr/bin/env bash
# PreToolUse guard: block edits to generated shell docs (CLAUDE.md, …).
# These files only `@AGENTS.md`-import the single source of truth; all real
# guidance must be edited in AGENTS.md instead. See AGENTS.md "Editing rule".
#
# Reads the PreToolUse JSON payload on stdin, denies the write when the target
# basename is one of the protected shells, and tells the agent where to go.
set -euo pipefail

# Protected shell docs (basenames). Add GEMINI.md etc. here if you create them.
PROTECTED='CLAUDE.md'

payload="$(cat)"
file_path="$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty')"
[ -z "$file_path" ] && exit 0

base="$(basename "$file_path")"
for p in $PROTECTED; do
  if [ "$base" = "$p" ]; then
    reason="$base is a generated shell that only contains @AGENTS.md. Edit AGENTS.md instead — that is the single source of truth for all agent guidance."
    jq -cn --arg r "$reason" '{
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: $r
      }
    }'
    exit 0
  fi
done

exit 0
