"""Python MCP stdio client — JSON-RPC 2.0 over a long-lived ``innate mcp`` process.

The default :class:`~innate.client.KnowledgeBase` shells out one CLI subprocess
per call. ``McpClient`` instead keeps a single ``innate mcp`` server alive and
speaks JSON-RPC over its stdin/stdout, which avoids per-call process startup
cost and mirrors the TypeScript SDK's ``McpClient``.

Usage::

    from innate import McpClient

    with McpClient() as mcp:
        result = mcp.recall("how to handle rate limits")
        mcp.record(result["trace_id"], outcome="ok")
"""

from __future__ import annotations

import json
import os
import subprocess
import threading
from pathlib import Path
from typing import Any


def _binary() -> str:
    return os.environ.get("INNATE_BIN", "innate")


class McpError(RuntimeError):
    """Raised when the MCP server returns a JSON-RPC error or an isError result."""


class McpClient:
    """Long-lived ``innate mcp`` JSON-RPC client over stdio.

    Requests are issued synchronously: each call writes one JSON-RPC request
    line and blocks until the matching response id is read back. A background
    reader thread fans responses out to per-id events so a dead subprocess
    fails in-flight callers instead of hanging forever.
    """

    def __init__(
        self,
        db_path: str | Path | None = None,
        *,
        timeout: float = 30.0,
    ) -> None:
        args = [_binary()]
        env_db = db_path or os.environ.get("INNATE_DB")
        if env_db:
            args += ["--db", str(env_db)]
        args.append("mcp")

        try:
            self._proc = subprocess.Popen(
                args,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                bufsize=1,  # line-buffered
            )
        except FileNotFoundError:
            raise RuntimeError(
                "innate binary not found. Install with: "
                "cargo install --path <repo>/innate"
            ) from None

        self._timeout = timeout
        self._next_id = 1
        self._lock = threading.Lock()
        self._pending: dict[int, dict[str, Any]] = {}
        self._events: dict[int, threading.Event] = {}
        self._dead: str | None = None
        self._closed = False

        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()
        self.initialize()

    # ── transport ───────────────────────────────────────────────────────────
    def _read_loop(self) -> None:
        try:
            assert self._proc.stdout is not None
            for line in self._proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    continue  # ignore malformed / non-JSON noise
                msg_id = msg.get("id")
                if msg_id is None:
                    continue
                with self._lock:
                    self._pending[msg_id] = msg
                    ev = self._events.get(msg_id)
                if ev:
                    ev.set()
        finally:
            # Subprocess closed stdout — fail every in-flight caller so no one
            # blocks on a response that can never arrive.
            self._fail_all("innate mcp process exited")

    def _fail_all(self, reason: str) -> None:
        with self._lock:
            if self._dead is None:
                self._dead = reason
            for ev in self._events.values():
                ev.set()

    def _call(self, method: str, params: Any | None = None) -> Any:
        with self._lock:
            if self._dead:
                raise McpError(self._dead)
            req_id = self._next_id
            self._next_id += 1
            ev = threading.Event()
            self._events[req_id] = ev

        req = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            req["params"] = params

        try:
            assert self._proc.stdin is not None
            self._proc.stdin.write(json.dumps(req) + "\n")
            self._proc.stdin.flush()
        except (BrokenPipeError, ValueError) as exc:
            self._fail_all(f"innate mcp stdin write failed: {exc}")
            raise McpError(f"innate mcp stdin write failed: {exc}") from None

        if not ev.wait(self._timeout):
            with self._lock:
                self._events.pop(req_id, None)
            raise McpError(f"innate mcp request timed out after {self._timeout}s: {method}")

        with self._lock:
            self._events.pop(req_id, None)
            msg = self._pending.pop(req_id, None)
            dead = self._dead

        if msg is None:
            raise McpError(dead or "innate mcp request failed")
        if "error" in msg and msg["error"]:
            raise McpError(str(msg["error"].get("message", msg["error"])))
        return msg.get("result")

    def initialize(self) -> None:
        self._call(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "clientInfo": {"name": "innate-py", "version": "0.1.17"},
            },
        )
        # notifications/initialized is a notification (no id, no response).
        try:
            assert self._proc.stdin is not None
            self._proc.stdin.write(
                json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"
            )
            self._proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def tool_call(self, name: str, args: dict[str, Any]) -> Any:
        result = self._call("tools/call", {"name": name, "arguments": args})
        content = (result or {}).get("content") or []
        text = content[0].get("text", "") if content else ""
        if (result or {}).get("isError"):
            raise McpError(text)
        try:
            return json.loads(text)
        except (json.JSONDecodeError, TypeError):
            return text

    # ── typed wrappers (mirror KnowledgeBase / TS McpClient) ─────────────────
    def recall(
        self,
        query: str,
        *,
        budget: int = 6000,
        top: int | None = None,
        source: str = "sdk",
        include_sparks: bool | None = None,
        expand_deps: str | None = None,
        allow_trim: bool | None = None,
    ) -> dict[str, Any]:
        args: dict[str, Any] = {"query": query, "budget": budget, "source": source}
        if top is not None:
            args["top"] = top
        if include_sparks is not None:
            args["include_sparks"] = include_sparks
        if expand_deps is not None:
            args["expand_deps"] = expand_deps
        if allow_trim is not None:
            args["allow_trim"] = allow_trim
        return self.tool_call("innate_recall", args)

    def record(self, trace_id: str, **options: Any) -> Any:
        return self.tool_call("innate_record", {"trace_id": trace_id, **options})

    def appraise(self, **situation: Any) -> dict[str, Any]:
        return self.tool_call("innate_appraise", {**situation, "source": situation.pop("source", "sdk")})

    def add(self, content: str, *, kind: str = "note", source: str = "agent", **options: Any) -> str:
        r = self.tool_call("innate_add", {"content": content, "kind": kind, "source": source, **options})
        return r.get("chunk_id", "") if isinstance(r, dict) else str(r)

    def spark(self, content: str) -> str:
        r = self.tool_call("innate_spark", {"content": content})
        return r.get("chunk_id", "") if isinstance(r, dict) else str(r)

    def inspect(self) -> dict[str, Any]:
        return self.tool_call("innate_inspect", {})

    def evolve(self, trigger: str = "manual", *, rebuild_embeddings: bool = False) -> dict[str, Any]:
        args: dict[str, Any] = {"trigger": trigger}
        if rebuild_embeddings:
            args["rebuild_embeddings"] = True
        return self.tool_call("innate_evolve", args)

    def approve(self, chunk_id: str) -> None:
        self.tool_call("innate_approve", {"chunk_id": chunk_id})

    def archive(self, chunk_id: str, reason: str = "stale") -> None:
        self.tool_call("innate_archive", {"chunk_id": chunk_id, "reason": reason})

    def invalidate(self, chunk_id: str, reason: str = "") -> None:
        self.tool_call("innate_invalidate", {"chunk_id": chunk_id, "reason": reason})

    def restore(self, chunk_id: str) -> None:
        self.tool_call("innate_restore", {"chunk_id": chunk_id})

    def mature_spark(self, spark_id: str, to: str) -> None:
        self.tool_call("innate_mature_spark", {"spark_id": spark_id, "to": to})

    def promote_spark(self, spark_id: str, to: str = "note") -> str:
        r = self.tool_call("innate_promote_spark", {"spark_id": spark_id, "to": to})
        return r.get("chunk_id", "") if isinstance(r, dict) else str(r)

    def drop_spark(self, spark_id: str, reason: str = "") -> None:
        self.tool_call("innate_drop_spark", {"spark_id": spark_id, "reason": reason})

    # ── lifecycle ────────────────────────────────────────────────────────────
    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._fail_all("innate mcp client closed")
        try:
            if self._proc.stdin is not None:
                self._proc.stdin.close()
        except Exception:
            pass
        self._proc.terminate()
        try:
            self._proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self._proc.kill()

    def __enter__(self) -> "McpClient":
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.close()
