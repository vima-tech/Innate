---
name: innate-memory
description: >
  Procedural knowledge layer: call innate_recall before any task, innate_record after any task.
  Activate when: starting a coding/debugging/analysis task; user says "remember", "save", "log",
  or "follow this rule"; a hard problem is solved and the lesson is worth keeping.
license: MIT
metadata:
  author: vima-tech
  version: "0.2.0"
  architecture: mcp
compatibility: >
  Requires `innate` binary (Rust). Install: `cargo build --release` from the innate directory.
  Configure as MCP server (recommended) or use CLI directly.
---

## Architecture

Innate v0.2+ is a **Rust binary** exposing:
- An MCP server (`innate mcp`) for agent/host integration
- A CLI (`innate recall`, `innate record`, …) for manual use
- Python SDK (`innate-py`) and TypeScript SDK (`@innate/sdk`) for programmatic use

## MCP Integration (Recommended)

When Innate is configured as an MCP server, use these tools directly — no CLI calls needed:

| Tool | When to use |
|---|---|
| `innate_recall` | Before any task — retrieve relevant knowledge |
| `innate_record` | After any task — close trace with outcome |
| `innate_add` | Capture confirmed insight (pending, awaits human approval) |
| `innate_spark` | Save a quick idea for later incubation |
| `innate_inspect` | Check health: chunk counts, debt ratio, rebuild queue |
| `innate_evolve` | At session end — distil logs + curate |

## CLI Fallback (if MCP not configured)

```bash
# Before task — Recall
innate recall "<core task intent>" --top 5 --format json
# Extract trace_id from JSON output, inject recalled chunks into context.

# After task — Record
innate record <trace_id> --outcome ok --used <chunk_id1>,<chunk_id2>

# Capture insight (agent-sourced → always pending)
innate add "<insight>" --trigger "<when to recall this>" --source agent

# Quick idea
innate spark "<distilled idea>"

# End of session
innate evolve --trigger manual
innate inspect
```

## Safety Rails

- **Never** run `innate_approve`, `innate_archive`, `innate_invalidate`, `innate_restore`,
  `innate_mature_spark`, `innate_promote_spark`, or `innate_drop_spark` unless the human
  explicitly requests that exact governance action.
- `innate_add` with `source=agent` is always `pending`; bypassing review is prohibited.
- Pass `feedback_up|down` only when the human explicitly provides that feedback.
- On tool error: read the error message, retry once with correction. If still failing — **abandon and continue the main task**. Innate is auxiliary; knowledge failures must never block the agent.
- Never mark agent-synthesised experience as high-confidence without human verification.

## Anti-verbatim Rule

Raw conversation text must **never** be stored directly. Always distil to:
> `<core reusable principle> + <triggering context> + <what to avoid or do instead>`

## Write-back Self-check

Before ending a long conversation or responding "got it / will do":

> "Did this session produce a new code pattern, debugging heuristic, or domain insight worth keeping?"

If yes — **propose** to the user: "I have distilled a spark/note — shall I save it to Innate?"
Only call `innate_spark` / `innate_add` after the user confirms. Never write silently.
