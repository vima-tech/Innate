# innate-cli

> Self-growing procedural knowledge layer for AI agents.

## Install

```bash
pip install innate-cli
```

The platform binary is downloaded automatically during installation. On first use it will download if the wheel didn't include it.

## Usage

```bash
innate install                     # configure agents (Claude Code, Codex, opencode)
innate recall "query"              # search knowledge base
innate record <trace_id>           # close a trace
innate add "content" --kind note   # add a knowledge chunk
innate evolve                      # distill + curate
innate inspect                     # health check
innate mcp                         # start MCP server (stdio)
```

Or use as a Python module:

```bash
python -m innate_cli recall "query"
```

## Python API

The package also exposes a subprocess wrapper for use from Python:

```python
from innate_cli import find_binary
import subprocess

binary = find_binary()
result = subprocess.run([str(binary), "recall", "your query"], capture_output=True, text=True)
print(result.stdout)
```

## Environment variables

| Variable | Description |
|---|---|
| `INNATE_DB` | Path to knowledge base (default: `~/.innate/personal.db`) |
| `INNATE_SKIP_DOWNLOAD` | Skip binary download during `pip install` |
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

- [GitHub](https://github.com/innate-rs/innate)
- [Releases](https://github.com/innate-rs/innate/releases)
