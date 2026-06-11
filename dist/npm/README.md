# @vima_tech/innate

> Self-growing procedural knowledge layer for AI agents.

## Install

```bash
npm install -g @vima_tech/innate
```

After installation, run the setup wizard:

```bash
innate install
```

## Usage

```bash
innate recall "query"              # search knowledge base
innate record <trace_id>           # close a trace with outcome
innate add "content" --kind note   # add a knowledge chunk
innate evolve                      # distill + curate
innate inspect                     # health check
innate mcp                         # start MCP server (stdio)
```

## MCP server

Add to your Claude Code `settings.json`:

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

Or run `innate install` to configure all detected agents automatically.

## Environment variables

| Variable | Description |
|---|---|
| `INNATE_DB` | Path to knowledge base (default: `~/.innate/personal.db`) |
| `INNATE_SKIP_DOWNLOAD` | Skip binary download during `npm install` |
| `NO_COLOR` | Disable color output |

## Platform support

| Platform | Architecture | Support |
|---|---|---|
| Linux | x86_64 | ✓ |
| Linux | aarch64 | ✓ |
| macOS | x86_64 | ✓ |
| macOS | arm64 (Apple Silicon) | ✓ |
| Windows | x86_64 | ✓ |

## Links

- [Documentation](https://github.com/vima-tech/Innate)
- [Releases](https://github.com/vima-tech/Innate/releases)
- [Issues](https://github.com/vima-tech/Innate/issues)
