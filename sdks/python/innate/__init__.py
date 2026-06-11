"""Innate Python SDK — thin subprocess wrapper over the `innate` binary.

The Rust binary (`innate`) must be in PATH or passed as INNATE_BIN env var.
Install the binary: `cargo install --path <repo>/innate`

Usage::

    from innate import KnowledgeBase

    kb = KnowledgeBase()  # defaults to ~/.innate/personal.db
    result = kb.recall("how to handle rate limits")
    print(result.trace_id, result.knowledge)
    kb.record(result.trace_id, outcome="ok", used=[result.knowledge[0]["id"]])
"""

from .client import KnowledgeBase, RecallResult

__all__ = ["KnowledgeBase", "RecallResult"]
