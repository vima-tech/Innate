---
name: innate-memory
description: >
  Self-growing procedural knowledge layer for agents.
  ACTIVATE TO READ when starting complex tasks, debugging recurring issues, referencing past patterns, or avoiding repeated mistakes.
  ACTIVATE TO WRITE when the user says "remember this", "save this insight", "follow this rule from now on", "log this lesson", or after successfully solving a hard problem worth distilling.
  Activate even when "memory" is not explicitly mentioned — any situation involving experience reuse or knowledge capture qualifies.
license: MIT
metadata:
  author: vima-tech
  version: "4.5.1"
compatibility: Requires `innate` CLI installed (`pip install innate`). Uses a local SQLite knowledge base.
---

## Prerequisites

```bash
pip install innate
# First run creates ~/.innate/personal.db automatically
```

## Core Workflow

### Before a task — Recall

```bash
# Machine integration (get trace_id for later record):
innate recall "<core task intent>" --top 5 --format json

# Extract trace_id from JSON output, inject recalled chunks into context.
# High-confidence chunks are hard constraints; low-confidence are soft guidance.

# Alternative — direct system-prompt injection (no trace_id needed):
innate recall "<core task intent>" --top 5 --format prompt
# The prompt output embeds <!-- innate_trace_id: xxx --> for later extraction.
```

### After a task — Record outcome

```bash
# Close the trace so the experience can be distilled later:
innate record <trace_id> --outcome ok --used <chunk_id1>,<chunk_id2> --feedback up

# If the task failed:
innate record <trace_id> --outcome fail
```

### Capture a structured insight

```bash
# Confirmed knowledge (human has reviewed and approved):
innate add "<insight>" --kind note --trigger "<when to recall this>" --source agent
# Always writes pending — awaits human approve or auto-promotion via Evolve rules.

# Raw idea / spark to revisit later:
innate spark "<idea distilled to: core claim + applicable context + open hypothesis>"
# Never store verbatim — always distil first.
```

### Periodic growth (end of session or scheduled)

```bash
innate evolve --trigger manual    # Distil episodic logs → pending chunks + Curate
innate inspect                    # Health check: debt ratio, stale screening, embed rebuild queue
```

## Safety Rails

- **Never** run `innate invalidate` or `innate archive` — human governance only.
- `innate add --source agent` is always `pending`; bypassing review is prohibited.
- On `exit_code != 0`: read stderr, retry once with correction. If still failing — **abandon and continue the main task**. Innate is auxiliary; knowledge failures must never block the agent.
- Never mark agent-synthesised experience as high-confidence without human verification.

## Anti-verbatim Rule

Raw conversation text must **never** be stored directly. Always distil to:
> `<core reusable principle> + <triggering context> + <what to avoid or do instead>`

## Write-back Self-check

Before ending a long conversation or responding "got it / will do":

> "Did this session produce a new code pattern, debugging heuristic, or domain insight worth keeping?"

If yes — **propose** to the user: "I have distilled a spark/note — shall I save it to Innate?"
Only call `innate spark` / `innate add` after the user confirms. Never write silently.
