---
name: innate-memory
description: >
  Innate procedural knowledge layer for coding, debugging, and analysis sessions.
  ACTIVATE when: (1) user says "remember", "save", "log", "follow this rule", "don't forget",
  "recall", or "what did we learn"; (2) a non-obvious solution, workaround, constraint, or
  invariant was discovered during this session; (3) starting a task where prior mistakes or
  project-specific patterns are likely relevant (refactoring, debugging recurring errors,
  architectural decisions); (4) user asks to save or retrieve any past experience.
  DO NOT activate for: routine Q&A, simple factual lookups, one-off throwaway requests,
  tasks the user marks as experimental/scratch, or when the user says "don't save this".
license: MIT
metadata:
  author: vima-tech
  version: "0.3.0"
  architecture: mcp
compatibility: >
  Requires `innate` binary (Rust). Install: `cargo build --release` from the innate directory.
  Configure as MCP server (see CLAUDE.md). CLI fallback available.
---

## Layer Role

Innate is **auxiliary** — it must never block the main task. All innate operations are
best-effort. On any tool error: retry once with a corrected call, then abandon and continue.

## When to Recall

Recall at the **start of a task** only if the task is:
- In a domain with known recurring patterns (e.g., a specific codebase, framework, or protocol)
- About debugging or fixing something that may have been encountered before
- Architectural/design — prior decisions matter

**Skip recall** for: quick one-liners, questions with clear answers, tasks the user scoped
as entirely new territory with no prior context.

### Query Formulation

Use the **intent**, not the literal user message. Strip filler words.

| User says | Good query | Bad query |
|---|---|---|
| "why does this crash on startup?" | `startup crash sqlite init` | `why does this crash on startup` |
| "refactor the auth flow" | `auth flow session token handling` | `refactor auth flow` |
| "add rate limiting to the API" | `rate limiting middleware pattern` | `add rate limiting` |

Call: `innate_recall(query=<intent>, budget=4000, source="agent")`

Inject recalled chunks into your working context. Mention to the user which (if any) relevant
prior knowledge was found — one sentence is enough.

## When to Record

Record **after** a task that produced a real outcome (success or failure). Skip recording for:
- Exploratory back-and-forth with no definitive result
- Tasks where the user interrupted before completion
- Pure retrieval (the user only asked a question, nothing was built or changed)

### Outcome Rules

| Situation | outcome |
|---|---|
| Task completed, user confirmed or accepted the result | `ok` |
| Task failed, approach was wrong, user had to correct course | `fail` |
| Session cut off, outcome unclear | `unknown` |

### Used Chunk IDs

Only list chunks you **actively referenced** in your response — not every candidate that was
recalled. One to three IDs is typical. Empty is fine if no prior knowledge was relevant.

### Output Summary

One sentence, written for a future agent reading it cold:
> "Fixed SQLite WAL mode detection by checking PRAGMA journal_mode response rather than assuming WAL."

Not: "I helped the user fix a bug."

Call: `innate_record(trace_id=<id>, outcome=<ok|fail|unknown>, used=[<chunk_ids>], output_summary=<sentence>)`

## MCP Tool Reference

| Tool | Use | Notes |
|---|---|---|
| `innate_recall` | Task start (when relevant) | Formulate with intent, not literal wording |
| `innate_record` | Task end (when real outcome exists) | Outcome + used IDs + 1-sentence summary |
| `innate_add` | Capture confirmed insight | Always `source=agent`; goes to pending, awaits review |
| `innate_spark` | Quick idea for later | No trigger needed; brief distilled form |
| `innate_evolve` | End of session | `trigger=manual`; distils logs + curates |
| `innate_inspect` | Health check | chunk counts, debt ratio, rebuild queue |

**Never call without explicit user request:** `innate_approve`, `innate_archive`,
`innate_invalidate`, `innate_restore`, `innate_mature_spark`, `innate_promote_spark`,
`innate_drop_spark`.

## Nomination

Use the `nomination` field in `innate_record` only for genuinely exceptional outcomes —
a pattern that will likely save significant time if recalled next time. Rare. Not for every
successful task.

## CLI Fallback (if MCP not configured)

```bash
# Recall
innate recall "<intent>" --top 5 --format json --source agent

# Record
innate record <trace_id> --outcome ok --used <id1>,<id2> --output-summary "<sentence>"

# Capture insight
innate add "<principle>" --trigger "<context>" --source agent

# Quick spark
innate spark "<distilled idea>"

# End of session
innate evolve --trigger manual
innate inspect
```

## Write-back Decision

Before ending a long session, run this checklist silently:

1. Was a non-obvious solution found? (workaround, hidden constraint, subtle bug pattern)
2. Was a project-specific rule established? ("always do X in this codebase")
3. Did the user correct a wrong assumption of mine that I'd likely repeat?
4. Was a hard-won insight reached that took multiple attempts?

If **any** answer is yes → propose to the user:
> "This session surfaced [one-line description]. Want me to save it to Innate?"

Only call `innate_spark` or `innate_add` **after user confirms**. Never write silently.

## Anti-verbatim Rule

Raw conversation text must never be stored. Always distil to the reusable form:
> `<principle> — <trigger context> — <what to avoid>`

Example:
- ❌ "The user told me to use WAL mode and I fixed it by changing the pragma"
- ✅ "SQLite WAL mode must be verified via PRAGMA response, not assumed — triggers on any db open"

## Sparks vs. Notes

| Use `innate_spark` | Use `innate_add` |
|---|---|
| Quick idea, half-formed hunch | Confirmed reusable principle |
| Not yet validated | Validated in this session |
| User says "note this for later" | User says "remember this rule" |
