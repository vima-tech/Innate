"""Python SDK client — subprocess wrapper over `innate` binary."""

from __future__ import annotations

import functools
import inspect
import json
import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from .errors import OutcomeConflictError


def _binary() -> str:
    return os.environ.get("INNATE_BIN", "innate")


def _db_args(db_path: str | Path | None) -> list[str]:
    if db_path:
        return ["--db", str(db_path)]
    env_db = os.environ.get("INNATE_DB")
    return ["--db", env_db] if env_db else []


def _run(*args: str, check: bool = True) -> dict[str, Any]:
    """Run the innate binary and parse JSON stdout."""
    cmd = [_binary()] + list(args)
    try:
        # Never pass check=True to subprocess.run — CalledProcessError fires before
        # we can inspect stderr for OutcomeConflict. Always check returncode ourselves.
        result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    except FileNotFoundError:
        raise RuntimeError(
            "innate binary not found. Install with: "
            "cargo install --path <repo>/innate"
        ) from None
    if result.returncode != 0:
        stderr = result.stderr.strip()
        if "outcome_conflict" in stderr or "OutcomeConflict" in stderr:
            raise OutcomeConflictError(stderr)
        if check:
            raise RuntimeError(f"innate error: {stderr}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"_raw": result.stdout.strip()}


@dataclass
class RecallResult:
    knowledge: list[dict[str, Any]] = field(default_factory=list)
    sparks: list[dict[str, Any]] = field(default_factory=list)
    trace_id: str = ""
    empty: bool = True


class KnowledgeBase:
    """Programmatic access to Innate via the `innate` CLI binary."""

    def __init__(self, db_path: str | Path | None = None) -> None:
        self.db_path = db_path

    def _args(self) -> list[str]:
        return _db_args(self.db_path)

    def recall(
        self,
        query: str,
        *,
        budget: int = 6000,
        top: int | None = None,
        include_sparks: bool = False,
        source: str = "sdk",
        expand_deps: str = "false",
        allow_trim: bool = False,
        refine_mode: str = "off",
    ) -> RecallResult:
        args = self._args() + [
            "recall", query,
            "--budget", str(budget),
            "--format", "json",
            "--source", source,
            "--expand-deps", expand_deps,
            "--refine-mode", refine_mode,
        ]
        if top is not None:
            args += ["--top", str(top)]
        if include_sparks:
            args.append("--include-sparks")
        if allow_trim:
            args.append("--allow-trim")
        data = _run(*args)
        return RecallResult(
            knowledge=data.get("knowledge", []),
            sparks=data.get("sparks", []),
            trace_id=data.get("trace_id", ""),
            empty=data.get("empty", True),
        )

    def record(
        self,
        trace_id: str,
        *,
        query: str | None = None,
        outcome: str | None = None,
        used: list[str] | None = None,
        output_summary: str | None = None,
        nomination: str | None = None,
        feedback: str | None = None,
        priority: int = 0,
        source: str = "sdk",
    ) -> None:
        args = self._args() + ["record", trace_id, "--source", source]
        if query:
            args += ["--query", query]
        if outcome:
            args += ["--outcome", outcome]
        if used:
            args += ["--used", ",".join(used)]
        if output_summary:
            args += ["--output-summary", output_summary]
        if nomination:
            args += ["--nomination", nomination]
        if feedback:
            args += ["--feedback", feedback]
        if priority:
            args += ["--priority", str(priority)]
        _run(*args)

    def add(
        self,
        content: str,
        *,
        kind: str = "note",
        trigger_desc: str | None = None,
        anti_trigger_desc: str | None = None,
        source: str = "agent",
        skill_name: str | None = None,
    ) -> str:
        args = self._args() + ["add", content, "--kind", kind, "--source", source]
        if trigger_desc:
            args += ["--trigger", trigger_desc]
        if anti_trigger_desc:
            args += ["--anti-trigger", anti_trigger_desc]
        if skill_name:
            args += ["--skill-name", skill_name]
        result = _run(*args)
        return str(result.get("_raw", result.get("chunk_id", "")))

    def spark(
        self,
        content: str,
        *,
        trigger_desc: str | None = None,
    ) -> str:
        args = self._args() + ["spark", content]
        if trigger_desc:
            args += ["--trigger", trigger_desc]
        result = _run(*args)
        return str(result.get("_raw", result.get("chunk_id", "")))

    def evolve(self, trigger: str = "manual") -> dict[str, Any]:
        return _run(*self._args(), "evolve", "--trigger", trigger)

    def inspect(self) -> dict[str, Any]:
        return _run(*self._args(), "inspect")

    def approve(self, chunk_id: str) -> None:
        _run(*self._args(), "approve", chunk_id)

    def archive(self, chunk_id: str, reason: str = "stale") -> None:
        _run(*self._args(), "archive", chunk_id, "--reason", reason)

    def invalidate(self, chunk_id: str, reason: str = "") -> None:
        args = self._args() + ["invalidate", chunk_id]
        if reason:
            args += ["--reason", reason]
        _run(*args)

    def restore(self, chunk_id: str) -> None:
        _run(*self._args(), "restore", chunk_id)

    def mature_spark(self, spark_id: str, to: str) -> None:
        _run(*self._args(), "mature-spark", spark_id, to)

    def promote_spark(self, spark_id: str, to: str = "note") -> str:
        result = subprocess.run(
            [_binary()] + self._args() + ["promote-spark", spark_id, "--to", to],
            capture_output=True, text=True, check=True,
        )
        return result.stdout.strip()

    def drop_spark(self, spark_id: str, reason: str = "") -> None:
        args = self._args() + ["drop-spark", spark_id]
        if reason:
            args += ["--reason", reason]
        _run(*args)

    def augmented(
        self,
        *,
        budget: int = 6000,
        source: str = "augmented",
        expand_deps: str = "false",
        allow_trim: bool = False,
    ) -> Callable:
        """Decorator that auto-injects recalled knowledge into the wrapped function.

        Two outcome modes (§五 @augmented 边界):
        - Auto: function returns ``{"result": ..., "outcome": "ok"|"fail"}``
          → decorator calls ``kb.record()`` automatically.
        - Manual: function receives ``trace_id`` as a keyword argument; caller
          must call ``kb.record(trace_id, ...)`` explicitly.

        Example::

            @kb.augmented(budget=4000)
            def my_agent(query, *, knowledge, trace_id, **kw):
                answer = call_llm(query, knowledge)
                return {"result": answer, "outcome": "ok"}
        """
        def decorator(fn: Callable) -> Callable:
            sig = inspect.signature(fn)
            accepts_knowledge = "knowledge" in sig.parameters
            accepts_trace_id = "trace_id" in sig.parameters

            @functools.wraps(fn)
            def wrapper(*args: Any, **kwargs: Any) -> Any:
                query = ""
                if args:
                    query = str(args[0])
                elif "query" in kwargs:
                    query = str(kwargs["query"])

                recall_result = self.recall(
                    query,
                    budget=budget,
                    source=source,
                    expand_deps=expand_deps,
                    allow_trim=allow_trim,
                )

                if accepts_knowledge:
                    kwargs["knowledge"] = recall_result.knowledge
                if accepts_trace_id:
                    kwargs["trace_id"] = recall_result.trace_id

                result = fn(*args, **kwargs)

                # Auto-record if function returns dict with "outcome" key.
                if isinstance(result, dict) and "outcome" in result:
                    outcome = result.get("outcome")
                    used_ids = result.get("used", [])
                    summary = result.get("output_summary") or result.get("summary")
                    self.record(
                        recall_result.trace_id,
                        outcome=outcome,
                        used=used_ids if used_ids else None,
                        output_summary=summary,
                        nomination=result.get("nomination"),
                        source=source,
                    )
                    return result.get("result", result)

                return result

            return wrapper
        return decorator
