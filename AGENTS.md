# AGENTS.md

This file is the single source of truth for AI coding agents (Claude Code, Codex,
opencode, Gemini CLI, …) working in this repository. Tool-specific shells such as
`CLAUDE.md` import this file via `@AGENTS.md` — keep all shared guidance here.

> **Editing rule (applies to all agents):** Any request to "update / improve
> `CLAUDE.md`" or the project guidance means **edit `AGENTS.md`**. `CLAUDE.md` is a
> generated shell that only contains `@AGENTS.md` — never add real guidance to it.
> Other tool shells (`GEMINI.md`, etc.) follow the same rule.

## Commands

```bash
# Build the binary
cd core && cargo build --release
# Binary: core/target/release/innate

# Run tests
cd core && cargo test

# Recall-quality eval suite (regression safety net for fused-score / ACT-R tuning)
cd core && cargo test --release eval -- --nocapture

# Run the CLI (after adding to PATH or using full path)
innate recall "query" --format json
innate inspect
innate evolve --trigger manual

# Start MCP server (for Claude Code / Claude Desktop integration)
innate mcp

# Daemon (Linux only) — background log/hook watcher
innate daemon start --watch /path/to/log/dir
innate daemon status
innate daemon stop
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

`docs/Innate-设计文档-v0.1.9.md` is the **编码基线** (latest; supersedes the v0.1.8 doc after the modular refactor). Every design decision references a section. When behavior is ambiguous, consult the doc first.

## Architecture

### Conceptual model — 记忆 · 技能 · 直觉 (Memory · Skill · Intuition)

Innate presents as three cooperating layers over one procedural-knowledge core. Keep this framing when writing user-facing copy, and map each layer to its real mechanism:

| Layer | Mechanism in code |
|---|---|
| **记忆 Memory** | `recall → record → evolve` flywheel; confidence EMA + decay + curate (`kb/recall.rs`, `kb/record/`, `kb/evolve.rs`, `kb/curate.rs`) |
| **技能 Skill** | `kind="skill"` / `origin="installed"` chunks; `innate-memory` SKILL.md; `install/` wizard |
| **直觉 Intuition** | `appraise` critic — synchronous, no-LLM, value-domain-safe (no answer text) (`kb/appraise.rs`, `innate_appraise`) |

Five access modules, one Rust KnowledgeBase Core:

```
MCP    (innate mcp)          ← JSON-RPC 2.0 over stdio; 15 tools; direct Core calls
CLI    (innate <cmd>)        ← clap thin wrapper; direct Core calls
Web    (innate web)          ← local read + governance HTTP UI; direct Core calls (read-write)
SDKs   (Python / TypeScript) ← subprocess wrapper over CLI binary
Daemon (innate daemon start) ← background log/hook watcher; invokes CLI subprocesses
                                    │
                                    ▼
                        KnowledgeBase (lib) ← all 8 Public APIs
                        SQLite + pure-Rust vector search
```

Dependency direction:

```
MCP    ──────────────────────> KnowledgeBase Core
CLI    ──────────────────────> KnowledgeBase Core
Web    ──────────────────────> KnowledgeBase Core
SDKs   ──> CLI ──────────────> KnowledgeBase Core
Daemon ──> CLI ──────────────> KnowledgeBase Core
```

MCP, CLI, and Web call `KnowledgeBase` directly (in-process). SDKs and Daemon never open the database; they shell out to the `innate` binary. Web is the only access module that exposes governance writes over the network, so it is localhost-bound + token-gated (see design doc §22).

### Crate layout (`core/src/`)

Source is split into focused module directories (the old monolithic `kb.rs` / `storage.rs` / `daemon.rs` no longer exist).

| Path | Role |
|---|---|
| `lib.rs` | crate root; `open_kb()` injects remote models from settings, else Dummy/Heuristic |
| `kb/mod.rs` | `KnowledgeBase` struct, param loading, `open_with` injection, cycle detection |
| `kb/recall.rs` | `recall` — vector candidates, fused scoring, packing, dep expansion, trace write |
| `kb/record/mod.rs` + `kb/record/evidence.rs` | `record`/`record_detailed`; confidence EMA replay, context stats, governance evidence |
| `kb/evolve.rs` | `evolve` — claim request, lease, distill transaction |
| `kb/curate.rs` | aggregate → archive → promote → decay → dedupe → governance |
| `kb/lifecycle.rs` | `add` / spark family / `approve` / `archive` / `invalidate` / `restore` |
| `kb/inspection.rs` | `inspect` — closed-loop health metrics |
| `kb/appraise.rs` | `appraise` — 直觉/intuition critic; synchronous no-LLM footing check, reuses recall's fused score, returns valence/strength/tier/flagged_points (never an answer) |
| `storage/{mod,chunks,traces,evolution,meta,raw}.rs` | rusqlite backend; schema init, BLOB-vector search, SQL helpers |
| `embedding.rs` | `EmbeddingProvider` trait + `DummyEmbeddingProvider` (hash-based, for tests) |
| `entities.rs` | `extract_entities` — deterministic (no-LLM, no-`regex`) high-signal entity extractor (error codes, flags, paths, code symbols) feeding the `chunk_entities` associative index for ACT-R spreading activation |
| `llm.rs` | `HttpDistiller` (OpenAI-compatible + Anthropic, one type) + `LlmEmbeddingProvider`; HTTP retry transport |
| `llm_trace.rs` | LLM/embedding call tracing — `post_json_retry` emits JSONL to `~/.innate/logs/llm_trace.log` (request/response previews, latency, retries, errors; never the API key). Read by `innate web` `/api/llm-traces` |
| `refine.rs` | `Sanitizer`/`Refiner`/`Distiller` traits + `DefaultSanitizer`/`NoopSanitizer`/`NullRefiner`/`HeuristicDistiller` defaults + `ResilientDistiller` (wraps LLM distiller with deterministic fallback after retry-budget exhaustion, so capture never depends on the LLM) |
| `errors.rs` | `InnateError` enum covering all error kinds |
| `mcp.rs` | MCP stdio server — 15 tools, JSON-RPC 2.0 dispatcher |
| `cli.rs` | CLI commands (clap), thin wrappers over KnowledgeBase |
| `web/{mod,api,assets}.rs` | `innate web` — local HTTP UI (`tiny_http` sync). `mod` serve+token; `api` pure router (read + governance, auth); `assets` embedded frontend via `include_str!` |
| `daemon/{mod,watch,events,process,state,command}.rs` | Background daemon — log/JSON-hook watcher; idempotent events; session trace; error stats; tail resumption (Linux only) |
| `install/{wizard,agents,skills,settings,path,ui,uninstall}.rs` | `innate install`/`uninstall` TUI — configures Claude/Codex/opencode MCP, skill, slash commands, Stop hook |
| `backup/{mod,command}.rs` | Cloudflare R2 backup/restore/list/prune (S3-compatible + SigV4) |
| `upgrade.rs` | `innate upgrade` — GitHub Releases self-update + SHA-256 verify + atomic swap |
| `migrate.rs` | Schema migration chain 4.0 → 4.21, each step atomic |
| `hook.rs` | `innate hook stop` — Claude Code Stop payload → session.log events |
| `paths.rs` | Single source of truth for the `~/.innate` directory layout; `ensure_layout()` creates subdirs + migrates legacy flat files |
| `utils.rs` | `utc_now_iso()`, `gen_uuid()`, `content_hash()`, `sanitize()`, cosine similarity |
| `settings.rs` | `settings.json` parsing (LLM / Embedding / Daemon / Backup) |
| `schema.sql` | Embedded schema (v4.21); `include_str!` at compile time |

### Filesystem layout (`~/.innate/`)

All local state lives under `~/.innate/`, split into three subdirectories with only config at the root:

```text
~/.innate/
  settings.json  settings.schema.jsonc   ← config (root)
  data/      personal.db (+ -shm/-wal), daemon_state.sqlite, daemon.pid, backup_state.json, tmp/
  logs/      daemon.log, mcp.log
  sessions/  session.log   (watched by the daemon)
```

- **`paths.rs` is the only place that derives `~/.innate` paths** — never re-join `.innate` elsewhere; add a helper there instead.
- `paths::ensure_layout()` runs at CLI and MCP startup: creates the subdirs and relocates files from the old flat layout (`~/.innate/<file>`). Idempotent, best-effort, moves only when the target is absent; the db moves together with its `-shm`/`-wal` sidecars.
- Default db is `~/.innate/data/personal.db` (override with `--db`). `innate vacuum` checkpoints the WAL and VACUUMs to reclaim space after curate compaction.

### The Public APIs (on `KnowledgeBase`)

`recall` → `record` → `evolve` → `approve/archive/invalidate/restore` → `add` → `spark/mature_spark/promote_spark/drop_spark` → `inspect`

Plus `appraise` — the 直觉/intuition critic (synchronous, no-LLM, reuses recall's fused score; never returns an answer).

### Key Data Flow

```
recall()  →  writes usage_trace(retrieved/selected) + episodic_log(distill_state='open')
record()  →  appends usage_trace(used/task_ok/task_fail) + updates episodic_log → 'new' or 'discarded'
evolve()  →  distill (new→pending chunks) + builtin_curate (aggregate→archive→promote→purge)
```

### Vector Search

No sqlite-vec dependency. Embeddings stored as raw `f32` BLOBs in `vec_content` / `vec_trigger` tables. `storage/` loads all embeddings into memory and computes cosine similarity in Rust. Designed for up to ~100k chunks (HNSW deliberately rejected); swap `EmbeddingProvider` and `Storage` if scale demands it.

### Hybrid retrieval (混合检索, v4.16)

`recall` fuses **three** candidate channels, not just vectors: content-cosine, trigger-cosine, and a **lexical/BM25** channel (`chunks_fts`, a standalone FTS5 table kept in sync by triggers; `storage::search_lexical`). The lexical channel recovers exact-token matches (error codes, flags, symbol names) that embeddings blur. Fused score = `w_content·sim_content + w_trigger·sim_trigger + w_lexical·sim_lexical + w_spread·sim_spread + w_confidence·conf + w_context·context_score + w_activation·activation` (+ pending/anti penalties). All weights are meta-tunable (`recall.w_*`).

Three opt-in levers, all **off by default** so the no-LLM hot path is unchanged:
- `recall.embed_situation_signature` (meta flag) — fold the coarse situation signature into the embedded query (part c, granularity).
- `recall.w_spread` (meta weight, default `0.0`) — **ACT-R spreading activation** (SAG-inspired associative recall, schema 4.18). Source activation flows from the **query's** entities and the entities of the top-`spread_seed_n` base candidates (2-hop), spreading `a_e / fan(e)` over the `chunk_entities` index (`storage::entity_links`/`entities_for_chunks`); the `1/fan` term is ACT-R's associative strength `S_ji = S − ln(fan_j)`, so promiscuous entities self-mute and entities with fan > `recall.spread_fan_cap` are dropped. Completes the activation model (base-level + associative). May pull in NEW candidates reachable only via a shared entity, which then flow through normal scoring. `expand_by_spreading` returns immediately when the weight is 0 — zero hot-path cost. Validate gains with a multi-hop `recall-eval` set before raising the weight.
- `RecallParams.rerank` / CLI `--rerank` — offline LLM rerank of the shortlist via the injectable `Reranker` (default `NoopReranker`; `LlmReranker` wired by `open_kb` when an LLM is configured). **Never** used by hooks/MCP; non-fatal (falls back to fused order).

Measure recall quality on real data with `innate recall-eval <labels.jsonl> [--k N]` (reports P@1 / Recall@k / MRR / nDCG@k using the configured provider). Template: `scripts/recall_eval_template.jsonl`. This is distinct from `cargo test eval`, which validates the ranking math on dummy-embedding fixtures.

## Non-Obvious Implementation Constraints

**Agent-source dimension (`agent` column, schema 4.17)** — `chunks.agent` and `episodic_log.agent` record *which agent product* drove the call (`claude-code` / `codex` / `opencode` / …). This is **orthogonal** to the access channel in `usage_trace.source` / `episodic_log.event_source` (`mcp/cli/hook/daemon/...`): the channel says *which entry point*, `agent` says *which agent*. Source of truth is `utils::agent_source()`, which reads the `INNATE_AGENT` env var (injected by `innate install` into each agent's MCP config; `None`/NULL when unset — backward compatible, never enum-constrained). Distilled chunks **inherit** `agent` from their source `episodic_log` (not the process running `evolve`, which may be a daemon/cron); promoted sparks inherit from the spark. The Stop/hook capture channel is not yet env-tagged (records land with NULL agent) — follow-up.

**Associative entity index (`chunk_entities`, schema 4.18)** — populated by `Storage::insert_chunk` calling `entities::extract_entities` on the **same** write path that feeds `chunks_fts`, so every chunk writer (lifecycle/distill/curate) indexes entities with no extra wiring. Extraction is **deterministic** (no LLM, no `regex` dep) so it runs on the capture hot path and in migration backfill. Sparks are **excluded** (`origin='spark'` skipped) since they are recall-exempt. `entity_links` filters to recall-valid chunks (`state != 'archived' AND origin != 'spark'`), mirroring `search_lexical`; chunks are soft-archived (never hard-deleted), so no entity cleanup is needed on archive. Extraction deliberately ignores plain lowercase words — those are already covered by the lexical channel, and entities must stay discriminative so the ACT-R fan term works.

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
- `sanitizer: Arc<dyn Sanitizer>` — reject/redact before persist

## Configurable Parameters

All tuning knobs in the `meta` table (keys `recall.*` and `curate.*`). Loaded once at `KnowledgeBase::open`. `innate inspect` prints current values.

## SDKs

Both SDKs wrap the CLI binary via subprocess — they are not native Rust FFI bindings.

| Path | Description |
|---|---|
| `sdks/python/` | Python SDK (`innate-py`) — subprocess wrapper, zero deps, API-compatible with core |
| `sdks/typescript/` | TypeScript SDK (`@vima-tech/sdk`) — CLI subprocess + async MCP client |

## MCP Tool Reference (`innate mcp`)

| Tool | Rust method |
|---|---|
| `innate_recall` | `KnowledgeBase::recall` |
| `innate_record` | `KnowledgeBase::record` |
| `innate_appraise` | `KnowledgeBase::appraise` (直觉 / intuition critic) |
| `innate_add` | `KnowledgeBase::add` |
| `innate_spark` | `KnowledgeBase::spark` |
| `innate_evolve` | `KnowledgeBase::evolve` |
| `innate_inspect` | `KnowledgeBase::inspect` |
| `innate_approve/archive/invalidate/restore` | governance APIs |
| `innate_mature_spark/promote_spark/drop_spark` | spark lifecycle |
| `innate_backup` | R2 backup (ops tool; not in install's default auto-allow set) |

## SKILL.md

- `name:` must match directory name (`innate-memory`)
- `description:` is the agent activation signal — keep precise about WHEN to activate
- MCP tools are the primary interface; CLI is the fallback
- Body is plain markdown agent instructions; no code execution
- The Skill is intentionally stored in two locations:
  - `skills/innate-memory/SKILL.md` is the public source used by Skill installers.
  - `core/assets/SKILL.md` is embedded in the Rust binary and used by `innate install`.
- **Always modify both files together and keep their contents byte-for-byte identical.**
  Never update, commit, or release a Skill change when only one copy has changed.
- Before committing a Skill change, verify synchronization with:
  `cmp skills/innate-memory/SKILL.md core/assets/SKILL.md`
