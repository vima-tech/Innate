# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Commands

```bash
# Build the binary
cd core && cargo build --release
# Binary: core/target/release/innate

# Run tests
cd core && cargo test

# Run the CLI (after adding to PATH or using full path)
innate recall "query" --format json
innate inspect
innate evolve --trigger manual

# Start MCP server (for Claude Code / Claude Desktop integration)
innate mcp
```

## MCP Integration

Add to `.claude/settings.json` to enable MCP tools directly in Claude Code:

```json
{
  "mcpServers": {
    "innate": {
      "command": "/path/to/core/target/release/innate",
      "args": ["mcp"]
    }
  }
}
```

## Authoritative Design Reference

`docs/Innate-设计文档-v4.5.1.md` is the **编码基线**. Every design decision references a section (§一–§九). When behavior is ambiguous, consult the doc first.

## Architecture

Single Rust binary (`innate/`) — three modes, one process:

```
innate recall/record/...  ← CLI adapter (clap, thin wrapper over KnowledgeBase)
innate mcp                ← MCP stdio server (JSON-RPC 2.0 over stdin/stdout)
KnowledgeBase (lib)       ← core: all 8 Public APIs, SQLite + pure-Rust vector search
```

### Crate layout (`core/src/`)

| File | Role |
|---|---|
| `kb.rs` | `KnowledgeBase` — all 8 Public APIs |
| `storage.rs` | rusqlite backend; schema init, BLOB-vector search, SQL helpers |
| `utils.rs` | `utc_now_iso()`, `gen_uuid()`, `content_hash()`, `default_sanitize()`, cosine similarity |
| `embedding.rs` | `EmbeddingProvider` trait + `DummyEmbeddingProvider` (hash-based, for tests) |
| `refine.rs` | `Refiner`/`Distiller` traits + `NullRefiner`/`HeuristicDistiller` defaults |
| `errors.rs` | `InnateError` enum covering all error kinds |
| `mcp/mod.rs` | MCP stdio server — 13 tools, JSON-RPC 2.0 dispatcher |
| `cli/mod.rs` | CLI commands (clap), thin wrappers over KnowledgeBase |
| `schema.sql` | Embedded schema (v4.5.1); `include_str!` at compile time |

### The 8 Public APIs (on `KnowledgeBase`)

`recall` → `record` → `evolve` → `approve/archive/invalidate/restore` → `add` → `spark/mature_spark/promote_spark/drop_spark` → `inspect`

### Key Data Flow

```
recall()  →  writes usage_trace(retrieved/selected) + episodic_log(distill_state='open')
record()  →  appends usage_trace(used/task_ok/task_fail) + updates episodic_log → 'new' or 'discarded'
evolve()  →  distill (new→pending chunks) + builtin_curate (aggregate→archive→promote→purge)
```

### Vector Search

No sqlite-vec dependency. Embeddings stored as raw `f32` BLOBs in `vec_content` / `vec_trigger` tables. `storage.rs` loads all embeddings into memory and computes cosine similarity in Rust. Suitable for moderate corpus sizes; swap `EmbeddingProvider` and `Storage` for HNSW if scale demands it.

## Non-Obvious Implementation Constraints

**Time functions** — `utc_now_iso()` in `utils.rs` is the **only** time source. Format: `YYYY-MM-DDTHH:MM:SS.mmmZ` (fixed 3-digit ms). Never use system time directly. All SQL cutoff comparisons rely on lexicographic ordering of this format.

**`record()` distill_state transition** — `open→new/discarded` judgment runs **only when `distill_state == 'open'`**. Second call must not downgrade a log already in `'new'` or `'screening'` state.

**`record()` fresh-insert path** — When no pre-existing `episodic_log` row exists (Hook/Daemon direct record), `is_fresh_insert = true` triggers `apply_outcome_implicit` even though `existing_outcome == outcome` after insert.

**`spark` chunks are Curate-exempt** — Archive/decay/confidence logic must filter `origin='spark'`. `mature_spark()` advances sequentially (`seed→sprouting→incubating`). Sparks use `maturity`, not `state`/`confidence`.

**`record()` is `BEGIN IMMEDIATE`** — Entire method body runs in one exclusive transaction. Confidence and last_used updates inside do **not** issue their own commits.

**Curate aggregate order is fixed** (§四, atomic BEGIN IMMEDIATE):
1. `aggregate_success_traces` — upsert into `chunk_success_traces` fact table
2. `aggregate_success_counts` — derive `used_success_count / last_success_at`
3. `aggregate_counters` — derive `selected_count / used_count` from `usage_trace`
4. Write `meta.last_agg_ts = cutoff_ts` → `purge_usage_trace(ts < cutoff_ts)`

`cutoff_ts` is fixed once at the start of curate; all steps share the same value.

**`add()` trigger vector** — `tvec = embed_trigger(trigger_desc or content)` always. Never fall back to truncating `cvec`.

## Extension Points

Injectable at `KnowledgeBase::open_with(...)`:
- `embedding: Arc<dyn EmbeddingProvider>` — swap embedding model
- `refiner: Arc<dyn Refiner>` — online trim/adapt
- `distiller: Arc<dyn Distiller>` — episodic log → chunk extraction

## Configurable Parameters

All tuning knobs in the `meta` table (keys `recall.*` and `curate.*`). Loaded once at `KnowledgeBase::open`. `innate inspect` prints current values.

## SDKs

| Path | Description |
|---|---|
| `sdks/python/` | Python SDK (`innate-py`) — subprocess wrapper, zero deps, API-compatible with core |
| `sdks/typescript/` | TypeScript SDK (`@innate/sdk`) — CLI subprocess + async MCP client |

## MCP Tool Reference (`innate mcp`)

| Tool | Rust method |
|---|---|
| `innate_recall` | `KnowledgeBase::recall` |
| `innate_record` | `KnowledgeBase::record` |
| `innate_add` | `KnowledgeBase::add` |
| `innate_spark` | `KnowledgeBase::spark` |
| `innate_evolve` | `KnowledgeBase::evolve` |
| `innate_inspect` | `KnowledgeBase::inspect` |
| `innate_approve/archive/invalidate/restore` | governance APIs |
| `innate_mature_spark/promote_spark/drop_spark` | spark lifecycle |

## SKILL.md (`skills/innate-memory/SKILL.md`)

- `name:` must match directory name (`innate-memory`)
- `description:` is the agent activation signal — keep precise about WHEN to activate
- MCP tools are the primary interface; CLI is the fallback
- Body is plain markdown agent instructions; no code execution
