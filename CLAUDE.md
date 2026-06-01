# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Install (editable + dev deps)
pip install -e ".[dev]"

# Run all tests
python -m pytest tests/

# Run a single test
python -m pytest tests/test_v451_compliance.py::test_record_second_call_does_not_downgrade_new_state -x

# Run the CLI
innate recall "query" --format json
innate inspect
innate evolve --trigger manual
```

## Authoritative Design Reference

`docs/Innate-设计文档-v4.5.1.md` is the **编码基线**. Every design decision references a section (§一–§九). When behavior is ambiguous, consult the doc first. The schema, API contracts, confidence formulas, and Curate rules in the doc are authoritative.

## Architecture

Three-layer system — each layer may only interact downward:

```
Core SDK  (innate/core/)        ← only layer allowed to read/write the DB
CLI Adapter  (innate/cli/)      ← thin Click wrapper, 1:1 maps to Core Public API
Runtime Daemon  (innate/daemon/)← external process, calls CLI only, never DB directly
```

### Core SDK (`innate/core/`)

| File | Role |
|---|---|
| `kb.py` | `KnowledgeBase` — all 8 Public APIs live here |
| `storage.py` | sqlite-vec backend; schema init + migration runner; all SQL |
| `utils.py` | `utc_now_iso()`, `gen_uuid()`, `content_hash()`, `default_sanitize()` |
| `embedding.py` | `EmbeddingProvider` ABC + `DummyEmbeddingProvider` (hash-based, for tests) |
| `refine.py` | `Refiner`/`Distiller` ABCs + `NullRefiner`/`HeuristicDistiller` defaults |
| `exceptions.py` | `EmbeddingUnavailable`, `OutcomeConflictError`, `ChunkNotFoundError`, `InvalidStateError` |

**`schema.sql` lives in `migrations/`** (not inside the package). `Storage._init_schema()` finds it at `../../migrations/schema.sql` relative to `storage.py`, then auto-applies incremental migrations (`4.x_to_4.y.sql`).

### The 8 Public APIs (on `KnowledgeBase`)

`recall` → `record` → `evolve` → `approve/archive/invalidate/restore` → `add` → `spark/promote_spark/drop_spark` → `inspect` → `@augmented`

### Key Data Flow

```
recall()  →  writes usage_trace(retrieved/selected) + episodic_log(distill_state='open')
record()  →  appends usage_trace(used/task_ok/task_fail) + updates episodic_log → 'new' or 'discarded'
evolve()  →  distill (new→pending chunks) + _builtin_curate (aggregate→decay→dedupe→archive→promote→purge)
```

## Non-Obvious Implementation Constraints

**Time functions** — `utc_now_iso()` in `utils.py` is the **only** allowed Python time source. Never call `datetime.utcnow()`, `time.time()`, or SQLite `datetime('now')` directly. All SQL time generation must use `strftime('%Y-%m-%dT%H:%M:%fZ','now')`. This is enforced for SQLite TEXT dictionary-order comparisons.

**`record()` distill_state transition** — The `open→new/discarded` judgment runs **only when `distill_state == 'open'`**. Calling `record()` a second time (e.g., to add feedback) must not downgrade a log already in `'new'` or `'screening'` state.

**`record()` fresh-insert path** — When there is no pre-existing `episodic_log` row (Hook/Daemon direct record without a prior `recall()`), `is_fresh_insert = True` must be set before re-reading the inserted log. This flag makes `_apply_outcome_implicit` fire even though `existing_outcome == outcome` after the insert.

**`spark` chunks are Curate-exempt** — Any code reading `confidence` or running archive/decay logic must first filter out `origin='spark'`. Sparks use `maturity` lifecycle (`seed→incubating→promoted/dropped`), not `state`/`confidence`.

**`record()` is `BEGIN IMMEDIATE`** — The entire method body runs inside one exclusive transaction. `update_chunk_confidence` and `update_chunk_last_used` do **not** call `commit()`; the outer `self.storage.commit()` at the end flushes everything.

**Curate aggregate order is fixed**: aggregate success traces → aggregate counters → write `meta.last_agg_ts = cutoff_ts` → then purge. `purge_usage_trace` uses `ts <= cutoff_ts` (the value fixed at aggregate start), never a fresh `now()`.

**`add()` trigger vector** — `tvec` is always computed as `embed_trigger(trigger_desc or content)`. Use `tvec` unconditionally for `insert_vec_trigger`; never fall back to truncating `cvec`.

## Extension Points

Five pluggable objects injected at `KnowledgeBase(...)`:
- `embedding: EmbeddingProvider` — swap embedding model
- `curator: Curator` — replace entire Curate logic (single `run()` method)
- `refiner: Refiner` — online trim/adapt (default `NullRefiner`, off)
- `distiller: Distiller` — episodic log → chunk extraction (default `HeuristicDistiller`)
- `sanitize: Callable` — content safety hook (default regex-only; `None` disables)

## Configurable Parameters

All tuning knobs live in the `meta` table (keys prefixed `recall.*` and `curate.*`). They are loaded once at `KnowledgeBase.__init__` into instance attributes; changing them requires a new instance. `innate inspect` prints current values.

## Skill File (`skills/innate-memory/SKILL.md`)

Follows the [Agent Skills open standard](https://agentskills.io/specification). Enables `npx skills add vima-tech/Innate` to install the skill into Claude Code and other compatible agents.

**Rules when editing:**
- `name:` in frontmatter must exactly match the directory name (`innate-memory`)
- `description:` is the activation signal the agent uses — keep it precise about WHEN to activate (read vs. write triggers)
- Body is plain markdown agent instructions; no code execution, no SDK calls — CLI only
- If adding a second skill, create `skills/<new-name>/SKILL.md`; never put two skills in one file

## Test Layout

| File | What it covers |
|---|---|
| `test_core.py` | Core recall/record/evolve/curate round-trips |
| `test_v451_compliance.py` | Design-doc compliance (sanitize paths, spark lifecycle, invalidate cascade, `record()` state machine) |
| `test_v451_gaps.py` | v4.5.1 checklist items (aggregate cutoff_ts, outcome conflict, stale screening, embed rebuild) |
| `test_boundaries.py` | Budget packing, dependency closure, first-fit + density refill |
| `test_v4_paths.py` | CLI output formats, Hook/Daemon record paths |
| `test_cross_lib.py` | Multi-library shared recall |
| `test_augmented.py` | `@augmented` decorator |
| `test_cli.py` | Click CLI commands |
