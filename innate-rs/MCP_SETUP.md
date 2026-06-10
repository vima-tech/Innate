# Innate MCP Setup

## Build

```bash
cd innate-rs
cargo build --release
# Binary: target/release/innate
```

## Claude Code / Claude Desktop (MCP)

Add to `.claude/settings.json` (project) or `~/.claude/settings.json` (global):

```json
{
  "mcpServers": {
    "innate": {
      "command": "/path/to/innate",
      "args": ["mcp"],
      "env": {
        "INNATE_DB": "/path/to/your.db"
      }
    }
  }
}
```

Or with a custom DB path:

```json
{
  "mcpServers": {
    "innate": {
      "command": "/path/to/innate",
      "args": ["mcp", "--db", "/home/user/.innate/personal.db"]
    }
  }
}
```

After restarting Claude Code, the following tools are available directly:

| Tool | Purpose |
|---|---|
| `innate_recall` | Search knowledge base |
| `innate_record` | Close trace with outcome |
| `innate_add` | Capture insight |
| `innate_spark` | Save quick idea |
| `innate_inspect` | Health check |
| `innate_evolve` | Distil + curate |
| `innate_approve` | Approve pending chunk |
| `innate_archive` | Archive chunk |
| `innate_invalidate` | Invalidate + blacklist |
| `innate_restore` | Restore archived chunk |
| `innate_mature_spark` | Advance spark maturity |
| `innate_promote_spark` | Promote spark to knowledge |
| `innate_drop_spark` | Drop spark |

## CLI (manual use)

```bash
innate recall "how to handle rate limits" --format json
innate record <trace_id> --outcome ok --used <chunk_id>
innate add "always validate input at system boundaries" --trigger "input validation"
innate spark "idea: use HNSW for faster recall"
innate evolve --trigger manual
innate inspect
```

## Python SDK

```bash
pip install innate-py  # or pip install -e sdks/python/
```

```python
from innate import KnowledgeBase

kb = KnowledgeBase()  # uses INNATE_DB env var or default path
result = kb.recall("rate limit handling")
kb.record(result.trace_id, outcome="ok", used=[result.knowledge[0]["id"]])
```

## TypeScript SDK

```bash
npm install @innate/sdk  # or npm install ./sdks/typescript/
```

```typescript
import { KnowledgeBase, McpClient } from "@innate/sdk";

// CLI subprocess mode
const kb = new KnowledgeBase();
const result = await kb.recall("rate limit handling");

// MCP client mode (for agent integrations)
const client = new McpClient();
await client.initialize();
const r = await client.recall("rate limit handling");
client.close();
```
