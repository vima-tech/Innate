"""Python SDK client — subprocess wrapper over `innate` binary."""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


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
        result = subprocess.run(
            cmd, capture_output=True, text=True, check=check
        )
    except FileNotFoundError:
        raise RuntimeError(
            "innate binary not found. Install with: "
            "cargo install --path <repo>/innate-rs"
        ) from None
    if result.returncode != 0:
        raise RuntimeError(f"innate error: {result.stderr.strip()}")
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
    ) -> RecallResult:
        args = self._args() + [
            "recall", query,
            "--budget", str(budget),
            "--format", "json",
        ]
        if top is not None:
            args += ["--top", str(top)]
        if include_sparks:
            args.append("--include-sparks")
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
        outcome: str | None = None,
        used: list[str] | None = None,
        output_summary: str | None = None,
        nomination: str | None = None,
        source: str = "sdk",
    ) -> None:
        args = self._args() + ["record", trace_id, "--source", source]
        if outcome:
            args += ["--outcome", outcome]
        if used:
            args += ["--used", ",".join(used)]
        if output_summary:
            args += ["--output-summary", output_summary]
        if nomination:
            args += ["--nomination", nomination]
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
        result = subprocess.run(
            [_binary()] + args, capture_output=True, text=True, check=True
        )
        return result.stdout.strip()

    def spark(
        self,
        content: str,
        *,
        trigger_desc: str | None = None,
    ) -> str:
        args = self._args() + ["spark", content]
        if trigger_desc:
            args += ["--trigger", trigger_desc]
        result = subprocess.run(
            [_binary()] + args, capture_output=True, text=True, check=True
        )
        return result.stdout.strip()

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
