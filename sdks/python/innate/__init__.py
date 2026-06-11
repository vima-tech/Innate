"""Innate Python SDK — thin subprocess wrapper over the `innate` binary.

The Rust binary (`innate`) must be in PATH or passed as INNATE_BIN env var.
Install the binary: `cargo install --path <repo>/innate`

Usage::

    from innate import KnowledgeBase

    kb = KnowledgeBase()  # defaults to ~/.innate/personal.db
    result = kb.recall("how to handle rate limits")
    print(result.trace_id, result.knowledge)
    kb.record(result.trace_id, outcome="ok", used=[result.knowledge[0]["id"]])

    # @augmented decorator — auto-inject recalled knowledge
    @kb.augmented(budget=4000)
    def my_agent(query, *, knowledge, trace_id, **kw):
        answer = call_llm(query, knowledge)
        return {"result": answer, "outcome": "ok"}
"""

from .client import KnowledgeBase, RecallResult
from .errors import OutcomeConflictError

__all__ = ["KnowledgeBase", "OutcomeConflictError", "RecallResult"]
