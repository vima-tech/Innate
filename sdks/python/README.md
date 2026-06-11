# innate-py

Python SDK for [Innate](https://github.com/innate-rs/innate) — self-growing procedural knowledge layer for AI agents.

## Installation

```sh
pip install innate-py
```

Requires the `innate` binary in PATH. Install it:

```sh
curl -fsSL https://raw.githubusercontent.com/innate-rs/innate/main/install.sh | sh
```

## Usage

```python
from innate import KnowledgeBase

kb = KnowledgeBase()  # uses ~/.innate/personal.db

# Recall knowledge
result = kb.recall("how to handle rate limits")
print(result.trace_id, result.knowledge)

# Record outcome
kb.record(result.trace_id, outcome="ok", used=[result.knowledge[0]["id"]],
          output_summary="Used exponential backoff")

# @augmented decorator — auto-inject + auto-record
@kb.augmented(budget=4000)
def my_agent(query, *, knowledge, trace_id, **kw):
    answer = call_llm(query, knowledge)
    return {"result": answer, "outcome": "ok"}
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `INNATE_BIN` | `innate` | Path to innate binary |
| `INNATE_DB` | `~/.innate/personal.db` | Path to knowledge database |
