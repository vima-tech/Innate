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

## Commands

Slash commands below are automatically installed to `~/.claude/commands/` by `innate install`
and updated on every `innate install` re-run. Each `command` block defines one command:
metadata before `---`, body (the agent prompt) after `---`.

```command
name: innate-recall
description: Recall prior innate knowledge for the current task context
---
Run innate_recall for the current task context and inject the result into this conversation.

If the user provided a query after the command, use that as the recall query exactly.
If no query was provided ($ARGUMENTS is empty), infer the query from the most recent user
message or current task intent — use the **intent** (e.g. "sqlite wal mode init") not the
literal text.

Steps:
1. Formulate the query string (from $ARGUMENTS or inferred intent).
2. Call `innate_recall` with `query=<query>`, `budget=4000`, `source="agent"`.
3. If knowledge is returned: summarize in one sentence what was found, then inject the
   relevant chunks into context.
4. If empty: say "No prior knowledge found for: <query>" — do not fabricate.
5. Save the `trace_id` for use in `/innate-record` later.
```

```command
name: innate-record
description: Close the current innate trace with an outcome and one-sentence summary
---
Close the current innate trace with an outcome and summary.

Parse $ARGUMENTS for:
- outcome: one of `ok`, `fail`, `unknown` (default: `ok` if not specified)
- Any remaining text becomes the output_summary override

Steps:
1. Identify the active trace_id from this session's most recent `innate_recall` call.
   If no trace_id is available, say "No active trace — run /innate-recall first."
2. Determine which recalled chunk IDs were actually used in the final response (not
   just candidates recalled).
3. Write a one-sentence output_summary for a future agent reading cold:
   - If the user provided summary text in $ARGUMENTS, use that.
   - Otherwise synthesize from the task outcome.
4. Call `innate_record` with `trace_id`, `outcome`, `used=[<chunk_ids>]`, `output_summary`.
5. Confirm: "Recorded trace <trace_id> as <outcome>."
```

```command
name: innate-save
description: Save a confirmed insight to Innate as a pending knowledge chunk
---
Save a confirmed insight to Innate as a pending knowledge chunk.

$ARGUMENTS is the insight text to save (required). If empty, ask: "What insight would
you like to save?"

Steps:
1. Parse $ARGUMENTS as the insight content.
2. Distil to reusable form: `<principle> — <trigger context> — <what to avoid/do>`.
   Never store raw conversation text verbatim.
3. Infer a trigger_desc from the distilled content (2-5 words describing when to recall it).
4. Call `innate_add` with `content=<distilled>`, `trigger_desc=<trigger>`, `source="agent"`.
5. Confirm: "Saved as pending chunk <id>. Awaits your review via innate_approve."
```

```command
name: innate-spark
description: Save a quick idea or half-formed hunch to Innate as a spark
---
Save a quick idea or half-formed hunch to Innate as a spark (no review needed).

$ARGUMENTS is the idea text (required). If empty, ask: "What idea would you like to spark?"

Steps:
1. Parse $ARGUMENTS as the spark content.
2. Distil to brief, reusable form (1-2 sentences max). Drop filler words.
3. Call `innate_spark` with `content=<distilled>`.
4. Confirm: "Sparked <id>. Use `innate_mature_spark` when ready to develop it further."
```

```command
name: innate-evolve
description: Run end-of-session evolution — distil logs and curate knowledge
---
Run end-of-session evolution: distil episodic logs into knowledge chunks, then curate.

Steps:
1. Call `innate_evolve` with `trigger="manual"`.
2. Report the result: new chunks distilled, archived chunks, errors (if any).
3. Then call `innate_inspect` and show: chunk counts, debt ratio, rebuild queue size.
4. If debt_ratio > 0.3: suggest "Consider reviewing pending chunks with innate_approve."
```

```command
name: innate-inspect
description: Show innate knowledge base health — chunk counts, debt ratio, config params
---
Show innate knowledge base health: chunk counts, debt ratio, rebuild queue, config params.

$ARGUMENTS can be empty (show all) or a param name prefix to filter (e.g. "recall" or "curate").

Steps:
1. Call `innate_inspect`.
2. Display a readable summary:
   - Total active chunks, pending count, spark count
   - Debt ratio (pending / active)
   - Rebuild queue depth
   - Config params: if $ARGUMENTS specifies a prefix, filter to that prefix; otherwise show all.
3. If debt_ratio > 0.3, highlight: "High debt ratio — consider running /innate-evolve and
   reviewing pending chunks."
```
