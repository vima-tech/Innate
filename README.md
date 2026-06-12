# Innate — Self-Growing Procedural Knowledge Layer

[中文文档](README.zh.md)

> **One-line pitch**: An embeddable, self-growing, engine-swappable procedural knowledge layer for AI agents.
> It does not orchestrate (neutral to LangGraph / Claude Code / raw API calls). It solves one thing — **assembling the most relevant, precise knowledge within a token budget, and letting that knowledge evolve through real use.**

Innate does not manage *declarative memory* ("what the world looks like / user preferences") or a *static skill repository* written once and never updated. It manages **procedural knowledge** — "how to do things, which approach works better in this context." Every knowledge chunk is evaluated by real outcomes from the moment it enters the store: effective chunks gain confidence, stale ones decay and archive, sparks incubate and promote, and the whole library gets sharper with use.

---

## Installation

### One-line install (Linux / macOS)

```bash
curl -fsSL https://innate.mengkai.ren/ | sh
```

Downloads the pre-built binary for your platform, verifies the checksum, places it in `~/.local/bin`, and runs `innate install` to configure agent integration — all in one step.

Options:

```bash
# Pin a specific version
curl -fsSL https://innate.mengkai.ren/ | INNATE_VERSION=0.1.6 sh

# Skip agent setup (binary only)
curl -fsSL https://innate.mengkai.ren/ | NO_INNATE_SETUP=1 sh

# Custom install directory
curl -fsSL https://innate.mengkai.ren/ | INNATE_DIR=/usr/local/bin sh
```

### Other install methods

```bash
# Rust
cargo install innate

# Python installer (downloads pre-built binary, no Rust required)
pip install innate-ai

# npm installer (downloads pre-built binary, no Rust required)
npm install -g @vima_tech/innate

# Build from source
cd core && cargo build --release
cp target/release/innate ~/.local/bin/
```

Verify the install:

```bash
innate inspect
```

### Configure agent integration

After the binary is on your PATH, run the interactive wizard to configure the MCP server and install the `innate-memory` agent skill (skipped automatically by the one-line installer above):

```bash
innate install
```

Detects Claude Code, Codex CLI, and opencode automatically.

### Let an agent install for you

Paste the following prompt into Claude Code (or any agent with shell access):

```
Install Innate by running: curl -fsSL https://innate.mengkai.ren/ | sh
This downloads the binary, verifies the checksum, and configures the MCP server and agent skill automatically.
Verify with `innate inspect` when done.
```

### Python SDK (programmatic use)

```bash
pip install innate-py
```

### TypeScript SDK (programmatic use)

```bash
npm install @innate/sdk
```

### MCP server (Claude Code / Claude Desktop)

Add to `.claude/settings.json`:

```json
{
  "mcpServers": {
    "innate": {
      "command": "innate",
      "args": ["mcp"]
    }
  }
}
```

Once configured, the agent can call `innate_recall`, `innate_record`, and other MCP tools directly — no CLI invocation needed.

### Daemon (Linux only)

Background process that watches log directories and JSON hook events and bridges them into the knowledge layer:

```bash
innate daemon start --watch /path/to/log/dir
innate daemon status
innate daemon stop
```

The daemon calls the `innate` CLI via subprocess and never opens the database directly. Its state (file offsets, processed events, session traces) is stored in a separate `daemon_state.sqlite`.

### Uninstall

```bash
innate uninstall              # interactive — keeps data
innate uninstall -y           # skip prompts, keep data
innate uninstall -y --purge-data  # remove everything including ~/.innate/
```

---

## Quick Start

```bash
# 1. Add a knowledge chunk
innate add "List comprehensions are more readable than map/filter in Python" \
  --kind note --trigger "python list processing"

# 2. Recall knowledge (returns top chunks + trace_id)
innate recall "python list optimization" --budget 2000 --format json

# 3. Close the trace (complete the experience loop)
innate record <trace_id> --outcome ok --used <chunk_id1>,<chunk_id2>

# 4. Trigger growth (distil + curate)
innate evolve --trigger manual

# 5. Health check
innate inspect
```

---

## Core Loop

Five loops must all be complete — any missing one means it is not a "knowledge layer":

| Loop | Path |
|---|---|
| **Recall** | `query → recall → context` (dual-vector cosine + scalar filter, synchronous pure math) |
| **Observe** | `context → use → trace` (records which chunks were used and the outcome) |
| **Grow** | `trace → distil → pending` (offline distillation of new experience) |
| **Govern** | `usage → confidence → curate` (EMA confidence update + archive) |
| **Safety** | `pending / archived / no hard delete` (default sanitize hook, blacklist + red action) |

> The default synchronous recall path **never calls any LLM** — Innate is the librarian, not the reader and editor. Only when the caller explicitly enables the optional Refiner does trim/adapt execute.

---

## MCP Integration

The recommended way to connect an agent is through the MCP server.

### Agent workflow protocol

After configuration, give your agent the following protocol:

| Operation | MCP tool |
|---|---|
| Agent may call freely | `innate_recall` · `innate_record` · `innate_evolve` · `innate_inspect` |
| Confirm before calling | `innate_add` · `innate_spark` |
| Human governance only | `innate_approve` · `innate_archive` · `innate_invalidate` · `innate_restore` · `innate_mature_spark` · `innate_promote_spark` · `innate_drop_spark` |

**Workflow**

- Call `innate_recall(query="<task intent>")` before each task; incorporate the result into your plan
- Call `innate_record(trace_id=..., outcome="ok"|"fail")` after each task to close the trace
- When you find experience worth keeping, distil it and confirm with the user before calling `innate_add` or `innate_spark`
- When knowledge seems stale, propose governance — never execute governance actions autonomously
- Call `innate_evolve(trigger="manual")` at the end of a session to trigger distillation

---

## Python SDK

```python
from innate import KnowledgeBase

kb = KnowledgeBase("personal.db")  # or set INNATE_DB env var

# Add knowledge
note_id = kb.add("insight text", kind="note", trigger_desc="when to use this")
spark_id = kb.spark("a hypothesis to explore", trigger_desc="related context")

# Recall (synchronous, pure math)
ctx = kb.recall("task description", budget=6000, include_sparks=True)
for chunk in ctx.knowledge:
    print(chunk["id"], chunk["content"])

# Record usage
kb.record(ctx.trace_id, outcome="ok", used=[note_id], output_summary="solved X")

# Grow
result = kb.evolve(trigger="manual")

# Governance (human-initiated)
kb.approve(pending_id)
kb.archive(note_id, reason="stale")
kb.invalidate(note_id, reason="logic error")
kb.restore(archived_id)

# Spark lifecycle
kb.mature_spark(spark_id, to="sprouting")
kb.mature_spark(spark_id, to="incubating")
new_id = kb.promote_spark(spark_id, to="note")
kb.drop_spark(spark_id, reason="disproved")

# Health check
report = kb.inspect()
```

---

## TypeScript SDK

```typescript
import { KnowledgeBase, McpClient } from "@innate/sdk";

// CLI subprocess mode (synchronous, suitable for scripts)
const kb = new KnowledgeBase({ dbPath: "personal.db" });

const ctx = kb.recall("task description", { budget: 6000 });
kb.record(ctx.trace_id, { outcome: "ok", used: [chunkId] });
kb.add("insight text", { kind: "note", triggerDesc: "when to use this" });
kb.evolve("manual");
const report = kb.inspect();

// MCP client mode (async, suitable for agent integration)
const client = new McpClient({ dbPath: "personal.db" });
await client.initialize();

const result = await client.recall("task description", { budget: 6000 });
await client.record(result.trace_id, { outcome: "ok" });
await client.add("new insight", { kind: "note" });
await client.inspect();

client.close();
```

---

## CLI Reference

The CLI is a thin wrapper over the SDK public API — it adds no knowledge-layer logic beyond argument parsing and output formatting.

| Command | Domain | Options |
|---|---|---|
| `innate recall <query>` | Read | `--budget` · `--top` · `--include-sparks` · `--format text\|json` |
| `innate record <trace_id>` | Write | `--outcome ok\|fail\|unknown` · `--used` · `--output-summary` · `--nomination` · `--source` |
| `innate evolve` | Grow | `--trigger manual\|scheduled\|threshold` |
| `innate inspect` | Debug | Health check: 5 signals + current parameters |
| `innate add <content>` | Write | `--kind note\|skill` · `--trigger` · `--anti-trigger` · `--skill-name` · `--source` |
| `innate spark <content>` | Spark | `--trigger` |
| `innate mature-spark <id> <to>` | Govern | to: `sprouting\|incubating` (forward only) |
| `innate promote-spark <id>` | Govern | `--to note\|skill` |
| `innate drop-spark <id>` | Govern | `--reason` |
| `innate approve <id>` | Govern | pending → active |
| `innate archive <id>` | Govern | `--reason` |
| `innate invalidate <id>` | Govern | `--reason` (archive + blacklist) |
| `innate restore <id>` | Govern | archived → active; if previously invalidated, also clears hash blacklist |
| `innate mcp` | Integrate | Start MCP stdio server (JSON-RPC 2.0) for Claude Code / Desktop |
| `innate install` | Setup | Interactive wizard — configure agents, install binary, install skill |
| `innate uninstall` | Setup | Remove Innate from all configured agents and PATH |
| `innate daemon start` | Integrate | Background log/hook watcher `--watch <dir>` (Linux only) |
| `innate daemon status` | Integrate | Show daemon status and error stats |
| `innate daemon stop` | Integrate | Stop a running daemon |

### MCP tools (13 tools exposed by `innate mcp`)

| MCP Tool | Capability |
|---|---|
| `innate_recall` | Recall knowledge — call FIRST at task start |
| `innate_record` | Record trace outcome — call LAST after task completion |
| `innate_add` | Write a knowledge chunk |
| `innate_spark` | Create a spark (idea / hypothesis) |
| `innate_evolve` | Trigger growth — call at session end |
| `innate_inspect` | Health check |
| `innate_approve` / `innate_archive` / `innate_invalidate` / `innate_restore` | Governance |
| `innate_mature_spark` / `innate_promote_spark` / `innate_drop_spark` | Spark lifecycle |

---

## Architecture

Four access modules share one Rust KnowledgeBase core:

```
MCP    (innate mcp)          ──────────────────────────┐
CLI    (innate <cmd>)         ──────────────────────────┤──> KnowledgeBase Core ──> SQLite
SDKs   (Python / TypeScript)  ──> CLI ─────────────────┤
Daemon (innate daemon start)  ──> CLI ─────────────────┘
```

| Module | Implementation | Notes |
|---|---|---|
| **MCP** | Direct Core call | JSON-RPC 2.0 over stdio; 13 tools; for Claude Code / Claude Desktop |
| **CLI** | Direct Core call | clap thin wrapper; full public API as CLI commands |
| **Python SDK** | subprocess → CLI | `innate-py`, zero extra dependencies |
| **TypeScript SDK** | subprocess → CLI + async MCP client | `@innate/sdk`, Node.js 18+ |
| **Daemon** | subprocess → CLI | Background log/JSON hook watcher; idempotent events; session trace; error stats; tail resumption (Linux only) |

```
Innate System
├── Rust core (core/)
│   ├── KnowledgeBase (lib)     8 public APIs, SQLite + pure-Rust cosine similarity
│   ├── CLI (innate <cmd>)      clap thin wrapper → KnowledgeBase calls
│   ├── MCP Server (innate mcp) JSON-RPC 2.0 over stdio, 13 MCP tools
│   └── Daemon (innate daemon)  background log/hook watcher, subprocess → CLI (Linux only)
│
├── Vector search               f32 BLOB storage + full-scan cosine similarity (zero native extensions)
│   ├── vec_content             content vector (BLOB)
│   └── vec_trigger             trigger vector (BLOB)
│
├── SDKs
│   ├── sdks/python/            innate-py  — subprocess → CLI, zero extra dependencies
│   └── sdks/typescript/        @innate/sdk — CLI subprocess + async MCP client
│
└── skills/innate-memory/       SKILL.md (MCP tools primary, CLI fallback)
```

---

## Core Features

- **Dual-vector recall**: `content_vec` + `trigger_vec`, fused ranking with configurable `w_content / w_trigger / w_confidence` weights
- **Confidence-driven ranking**: EMA update + recency weighting (explicit signals) + time decay — knowledge improves with use
- **Zero native extensions**: vectors stored as f32 BLOBs, cosine similarity in pure Rust — no sqlite-vec or C extension required
- **Native MCP integration**: `innate mcp` exposes 13 tools, works out of the box with Claude Code and Claude Desktop
- **Hard-dep fail-closed**: if a hard dependency is unavailable or archived at recall time, the entire seed is discarded — no partial closures returned
- **Zero autonomous behavior**: the SDK never acts on its own; all growth is externally triggered (`evolve` trigger: manual / scheduled / threshold)
- **Sanitize three-state contract**: hook returns `(cleaned, action)`, `action ∈ {allow, redact, discard}`; `discard` rejects the write, `redact` caps confidence at 0.4
- **Spark independent lifecycle**: maturity = `seed → sprouting → incubating`, exits only on `promote` / `drop`; not included in confidence ranking, not archived by low score
- **Atomic dual-vector write**: chunk + content_vec + trigger_vec written in a single `BEGIN IMMEDIATE` transaction; any failure rolls back
- **Auto schema migration**: applies incremental migrations on startup; runs `schema.sql` directly on an empty database

---

## Compatibility

- Core runtime: Rust 1.70+ (bundled SQLite, zero external runtime dependencies)
- Python SDK: Python 3.8+ (subprocess, zero extra dependencies)
- TypeScript SDK: Node.js 18+ (child_process, zero extra dependencies)
- Daemon: Linux only (requires `/proc` and `fork`; returns a clear error on non-Linux platforms)
- Default database: `~/.innate/personal.db` (overridable via `INNATE_DB` env var or `--db` flag)
- Platforms: Linux / macOS / Windows; Daemon is Linux only

---

## Documentation

- [`docs/Innate-设计文档-v0.1.6.md`](docs/Innate-设计文档-v0.1.6.md) — Complete system design (authoritative baseline, Chinese)
- [`skills/innate-memory/SKILL.md`](skills/innate-memory/SKILL.md) — Agent skill metadata and usage protocol

---

## Development

```bash
# Build
cd core && cargo build --release

# Run all tests
cd core && cargo test

# Run a specific test
cd core && cargo test test_add_and_recall

# Inspect the default knowledge base
innate inspect
```

Tests are grouped by responsibility: `add_and_recall` · `spark_and_promote` · `record_state_machine` · `invalidate_cascade` · `inspect_returns_counts` · `evolve_smoke` + 3 utils tests.

---

## License

MIT
