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
  version: "0.4.0"
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

Use **canonical intent**, not literal user words. Keep queries consistent across sessions —
the same class of problem should always use the same canonical form, because Innate
accumulates context statistics per query pattern.

| User says | Good query | Bad query |
|---|---|---|
| "why does this crash on startup?" | `startup crash sqlite init` | `why does this crash on startup` |
| "refactor the auth flow" | `auth flow session token handling` | `refactor auth flow` |
| "add rate limiting to the API" | `rate limiting middleware pattern` | `add rate limiting` |
| "debug the flaky test" | `flaky test race condition timing` | `debug flaky test` |

Call: `innate_recall(query=<canonical_intent>, budget=4000, source="mcp")`

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

### Used Chunk IDs — Be Precise

Only list chunks you **actively referenced** in your reasoning or response. One to three IDs
is typical. **Empty is correct** if no prior knowledge was actually used (do not include
chunks that were merely recalled but not applied — the difference matters for confidence
scoring and future retrieval quality).

### Output Summary — Structured Format

Write a **self-contained sentence** that a future agent can act on cold. Use this structure:

```
<what was done or learned> — <key constraint or method> — <when this applies>
```

Good examples:
- `Fixed SQLite WAL mode detection by checking PRAGMA journal_mode response rather than assuming WAL — triggers on any new db connection in multi-process setups`
- `Resolved flaky auth test by injecting a clock mock — real system time causes race in token expiry check`
- `Rate limiting must be applied before auth middleware, not after — otherwise unauthenticated requests bypass the limiter`

Bad examples (too vague to reuse):
- `Fixed the bug`
- `I helped the user with auth`
- `The problem was timing`

The output_summary becomes the reusable knowledge chunk. If it reads like a story, rewrite it
as a principle. **Prefer the general, transferable form** — strip repo/file/function names and
one-off identifiers, and phrase the lesson so it helps on the next project too; keep
project-specific detail only when the lesson cannot be generalized without losing its meaning.
**If you cannot write a useful summary, set output="unknown" and skip recording.**

Call: `innate_record(trace_id=<id>, outcome=<ok|fail|unknown>, used=[<chunk_ids>], output_summary=<sentence>)`

### Explicit Feedback — Use It

When a recalled chunk was **directly helpful** (saved significant time, prevented a mistake):
```
innate_record(..., feedback_up=[<chunk_id>])
```

When a recalled chunk was **wrong or misleading** (led you in the wrong direction):
```
innate_record(..., feedback_down=[<chunk_id>])
```

Feedback is the fastest signal for knowledge quality. Two down signals on the same chunk
triggers a governance review; five triggers automatic archival. Use it.

## Nomination — Rare, Explicit

Use the `nomination` field only for genuinely generalizable lessons — patterns that will
save significant time the next time this type of problem appears. Rare. Not for every task.

Format for nomination (self-contained principle):
```
<principle> — <when it applies> — <what to avoid>
```

Example:
```
innate_record(..., nomination="SQLite UNIQUE index on partial WHERE clause requires exact WHERE match in ON CONFLICT — applies to all upsert patterns; bare ON CONFLICT without WHERE will silently skip the upsert")
```

## MCP Tool Reference

| Tool | Use | Notes |
|---|---|---|
| `innate_recall` | Task start (when relevant) | Use canonical query form — consistent phrasing accumulates context stats |
| `innate_record` | Task end (when real outcome exists) | Structured output_summary + precise used IDs |
| `innate_add` | Capture confirmed insight | Always `source="mcp"`; goes to pending, awaits review |
| `innate_spark` | Quick idea for later | Brief distilled form; no review needed |
| `innate_evolve` | End of session | `trigger="manual"`; distils logs + curates knowledge base |
| `innate_inspect` | Health check | chunk counts, debt ratio, rebuild queue |

**Never call without explicit user request:** `innate_approve`, `innate_archive`,
`innate_invalidate`, `innate_restore`, `innate_mature_spark`, `innate_promote_spark`,
`innate_drop_spark`.

## Write-back Decision

Before ending a long session, run this checklist silently:

1. Was a non-obvious solution found? (workaround, hidden constraint, subtle bug pattern)
2. Was a general, transferable skill, method, or technique surfaced? (prefer these — abstract
   project-specific lessons into reusable form; keep a project-specific rule only when it
   genuinely can't be generalized)
3. Did the user correct a wrong assumption of mine that I'd likely repeat?
4. Was a hard-won insight reached after multiple failed attempts?
5. Did a recalled chunk actively help? (record feedback_up)
6. Did a recalled chunk mislead? (record feedback_down)

If **any** of 1-4 → propose to the user:
> "This session surfaced [one-line description]. Want me to save it to Innate?"

Only call `innate_spark` or `innate_add` **after user confirms**. Never write silently.

## Anti-verbatim Rule

Raw conversation text must never be stored. Always distil to the reusable principle form:
> `<principle> — <trigger context> — <what to avoid>`

| ❌ Verbatim | ✅ Distilled |
|---|---|
| "The user told me to use WAL mode and I fixed it by changing the pragma" | "SQLite WAL mode must be verified via PRAGMA response, not assumed — triggers on any db open in write-heavy multi-process setups" |
| "We spent 2 hours debugging auth because of clock skew" | "JWT validation must account for clock skew tolerance (≥30s) — triggers on distributed auth between services with independent system clocks" |

## Prefer General Over Project-Specific

Innate is biased toward **general, transferable skills, methods, and techniques** — knowledge
that pays off across projects, not just the current codebase. When a lesson is project-specific:

1. Try to abstract it — drop repo/file/function/path names and one-off identifiers, and rephrase
   as a principle the next project could reuse.
2. Save the abstracted form. Keep concrete project-specific detail only when the lesson genuinely
   cannot be generalized without losing its meaning.

| ❌ Project-bound | ✅ Generalized |
|---|---|
| "In this repo, `record()` must run inside BEGIN IMMEDIATE" | "Multi-step state mutations that must be atomic belong in one exclusive transaction, not per-step commits — applies to any SQLite writer with concurrent processes" |
| "Set `recall.budget=4000` in the meta table" | "Expose retrieval budget as a tunable parameter instead of hardcoding it — lets recall scale with the context window without code changes" |

## Sparks vs. Notes

| Use `innate_spark` | Use `innate_add` |
|---|---|
| Quick idea, half-formed hunch | Confirmed reusable principle |
| Not yet validated | Validated in this session |
| User says "note this for later" | User says "remember this rule" |

## CLI Fallback (if MCP not configured)

```bash
# Recall
innate recall "<canonical_intent>" --top 5 --format json --source cli

# Record with structured summary
innate record <trace_id> --outcome ok \
  --used <id1>,<id2> \
  --output-summary "Fixed X by doing Y — applies when Z" \
  --feedback up  # or down

# Capture insight
innate add "<principle> — <trigger> — <what to avoid>" \
  --trigger "<2-5 word context>" \
  --source agent

# Quick spark
innate spark "<distilled idea in 1-2 sentences>"

# End of session
innate evolve --trigger manual
innate inspect
```

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
message or current task intent — use the **canonical intent** (e.g. `sqlite wal mode init`,
not `why does this crash`). Keep query form consistent with how you'd phrase the same
class of problem in the future.

Steps:
1. Formulate the canonical query string (from $ARGUMENTS or inferred intent).
2. Call `innate_recall` with `query=<query>`, `budget=4000`, `source="mcp"`.
3. If knowledge is returned: summarize in one sentence what was found, then inject the
   relevant chunks into context.
4. If empty: say "No prior knowledge found for: <query>" — do not fabricate.
5. Save the `trace_id` for use in `/innate-record` later.
```

```command
name: innate-record
description: Close the current innate trace with an outcome and structured summary
---
Close the current innate trace with an outcome and summary.

Parse $ARGUMENTS for:
- outcome: one of `ok`, `fail`, `unknown` (default: `ok` if not specified)
- Any remaining text becomes the output_summary override

Steps:
1. Identify the active trace_id from this session's most recent `innate_recall` call.
   If no trace_id is available, say "No active trace — run /innate-recall first."
2. Determine which recalled chunk IDs were **actually used** in the final response (not
   just candidates recalled). Be precise — unused recalled chunks should NOT be listed.
3. Write a structured output_summary for a future agent reading cold:
   Format: `<what was done> — <key constraint/method> — <when this applies>`
   If the user provided summary text in $ARGUMENTS, use that.
4. Assess feedback:
   - If any chunk was directly helpful → include `feedback_up=[chunk_id]`
   - If any chunk was wrong/misleading → include `feedback_down=[chunk_id]`
5. Call `innate_record` with `trace_id`, `outcome`, `used=[<chunk_ids>]`,
   `output_summary`, and feedback if applicable.
6. Confirm: "Recorded trace <trace_id> as <outcome>."
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
2. Distil to reusable principle form:
   `<principle> — <when it applies> — <what to avoid>`
   Never store raw conversation text verbatim.
3. Infer a trigger_desc (2-5 canonical words describing when to recall it).
   Use the same phrasing you would use when querying innate_recall for this topic.
4. Call `innate_add` with `content=<distilled>`, `trigger_desc=<trigger>`, `source="mcp"`.
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
