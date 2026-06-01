"""Runtime Daemon — 外部独立进程,监听日志/事件 → 调 CLI."""

from __future__ import annotations

import hashlib
import json
import logging
import logging.handlers
import os
import re
import signal
import sqlite3
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List

from innate.core.utils import utc_now_iso


DEFAULT_STATE_DB = Path.home() / ".innate" / "daemon_state.sqlite"
DEFAULT_LOG = Path.home() / ".innate" / "daemon.log"
# 日志轮转(§九 方案 B): 默认 10MB × 5 份
DEFAULT_LOG_MAX_BYTES = 10 * 1024 * 1024
DEFAULT_LOG_BACKUP_COUNT = 5


class DaemonServer:
    def __init__(
        self,
        db_path: str,
        watch_dirs: List[str],
        pid_file: str = "/tmp/innate-daemon.pid",
        log_file: str | None = None,
        state_db: str | None = None,
        log_max_bytes: int = DEFAULT_LOG_MAX_BYTES,
        log_backup_count: int = DEFAULT_LOG_BACKUP_COUNT,
    ):
        self.db_path = db_path
        self.watch_dirs = [Path(d) for d in watch_dirs if d and Path(d).exists()]
        self.pid_file = Path(pid_file)
        self.log_file = Path(log_file) if log_file else DEFAULT_LOG
        self.log_max_bytes = log_max_bytes
        self.log_backup_count = log_backup_count
        self.state_db = Path(state_db) if state_db else DEFAULT_STATE_DB
        self._running = False
        self._error_streaks: Dict[tuple[str, str], int] = {}

    def _init_state_db(self) -> None:
        self.state_db.parent.mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(str(self.state_db))
        conn.executescript("""
            CREATE TABLE IF NOT EXISTS watch_state (
                watch_path            TEXT PRIMARY KEY,
                last_processed_offset INTEGER NOT NULL DEFAULT 0,
                last_processed_inode  TEXT,
                updated_at            TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS processed_events (
                event_id   TEXT PRIMARY KEY,
                watch_path TEXT,
                trace_id   TEXT,
                event_type TEXT,
                ts         TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS trace_context (
                watch_path TEXT PRIMARY KEY,
                trace_id   TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
        """)
        conn.close()

    def _setup_logging(self) -> logging.Logger:
        self.log_file.parent.mkdir(parents=True, exist_ok=True)
        logger = logging.getLogger("innate.daemon")
        logger.setLevel(logging.INFO)
        # 防止重复添加 handler(daemon 重启时)
        logger.handlers.clear()
        # §九 方案 B: RotatingFileHandler — 默认 10MB × 5 份自动轮转
        handler = logging.handlers.RotatingFileHandler(
            str(self.log_file),
            maxBytes=self.log_max_bytes,
            backupCount=self.log_backup_count,
            encoding="utf-8",
        )
        handler.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s"))
        logger.addHandler(handler)
        return logger

    def start(self) -> None:
        if self.pid_file.exists():
            pid = self.pid_file.read_text().strip()
            if pid and Path(f"/proc/{pid}").exists():
                print(f"Daemon already running (pid={pid})")
                return
        self._init_state_db()
        self.logger = self._setup_logging()
        self.logger.info("Daemon starting")

        # 出生版使用单次 fork 进入后台运行.
        pid = os.fork()
        if pid > 0:
            self.pid_file.write_text(str(pid))
            print(f"Daemon started (pid={pid})")
            return

        os.setsid()
        self._running = True
        signal.signal(signal.SIGTERM, self._on_signal)
        signal.signal(signal.SIGINT, self._on_signal)

        self._loop()

    def _on_signal(self, signum, frame):
        self._running = False

    def _loop(self) -> None:
        while self._running:
            try:
                for wd in self.watch_dirs:
                    self._process_watch(wd)
            except Exception as exc:
                self.logger.error(f"loop error: {exc}")
            time.sleep(5)
        self.logger.info("Daemon stopped")

    def _process_watch(self, wd: Path) -> None:
        conn = sqlite3.connect(str(self.state_db))
        for log_file in sorted(wd.glob("*.log")):
            try:
                st = log_file.stat()
                inode = f"{st.st_ino}"
                row = conn.execute(
                    """SELECT last_processed_offset, last_processed_inode
                       FROM watch_state WHERE watch_path=?""",
                    (str(log_file),),
                ).fetchone()
                offset = row[0] if row else 0
                previous_inode = row[1] if row else None
                if previous_inode != inode or st.st_size < offset:
                    offset = 0
                with open(log_file, "r", encoding="utf-8", errors="ignore") as f:
                    f.seek(offset)
                    while True:
                        line_offset = f.tell()
                        line = f.readline()
                        if not line:
                            break
                        offset = f.tell()
                        self._handle_line(
                            line, str(log_file), str(log_file), inode, line_offset, conn
                        )
                conn.execute(
                    """INSERT OR REPLACE INTO watch_state(watch_path, last_processed_offset, last_processed_inode, updated_at)
                       VALUES (?, ?, ?, ?)""",
                    (str(log_file), offset, inode, utc_now_iso()),
                )
            except Exception as exc:
                self.logger.warning(f"process {log_file} error: {exc}")
        conn.commit()
        conn.close()

    def _handle_line(
        self,
        line: str,
        watch_path: str,
        file_path: str,
        inode: str,
        offset: int,
        conn: sqlite3.Connection,
    ) -> None:
        line = line.strip()
        if not line:
            return
        event_id = hashlib.sha256(
            f"{file_path}:{inode}:{offset}:{line}".encode()
        ).hexdigest()[:16]
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            payload = None
        if isinstance(payload, dict) and payload.get("event_type"):
            self._handle_hook_event(payload, event_id, watch_path, conn)
            return

        row = conn.execute("SELECT 1 FROM processed_events WHERE event_id=?", (event_id,)).fetchone()
        if row:
            return

        trace_match = re.search(r"innate_trace_id\s*[:=]\s*([A-Za-z0-9_-]+)", line)
        if trace_match:
            self._set_active_trace(conn, watch_path, trace_match.group(1))

        # 简单模式匹配 + 连续同类异常
        outcome = None
        if (
            any(k in line for k in ("Build successful", "Tests passed"))
            or re.search(r"✓\s*(?:\[\d+\]|\d+)?\s*passed", line)
        ):
            outcome = "ok"
            self._reset_error_streaks(watch_path)
        elif any(k in line for k in ("SyntaxError", "Error:", "FAIL")):
            outcome = "fail"
            self._reset_error_streaks(watch_path)
        else:
            error_match = re.search(r"\b([A-Za-z_][\w.]*(?:Error|Exception))\b", line)
            if error_match:
                key = (watch_path, error_match.group(1))
                self._reset_error_streaks(watch_path, except_error=error_match.group(1))
                self._error_streaks[key] = self._error_streaks.get(key, 0) + 1
                if self._error_streaks[key] >= 3:
                    outcome = "fail"
            else:
                self._reset_error_streaks(watch_path)

        trace_id = self._get_active_trace(conn, watch_path)
        event_type = outcome or "unknown"
        if outcome:
            trace_id = trace_id or self._ensure_trace(event_id)
            cmd = [
                "innate", "--db", self.db_path, "record", trace_id,
                "--outcome", outcome, "--source", "daemon",
            ]
            if not self._run_cli(cmd):
                event_type = f"{outcome}_error"

        conn.execute(
            """INSERT INTO processed_events(event_id, watch_path, trace_id, event_type, ts)
               VALUES (?, ?, ?, ?, ?)""",
            (event_id, watch_path, trace_id, event_type, utc_now_iso()),
        )

    def _handle_hook_event(
        self,
        payload: Dict[str, Any],
        fallback_event_id: str,
        watch_path: str,
        conn: sqlite3.Connection,
    ) -> None:
        """处理 Hook JSON 行并映射到 CLI."""
        event_id = str(payload.get("event_id") or fallback_event_id)
        if conn.execute(
            "SELECT 1 FROM processed_events WHERE event_id=?", (event_id,)
        ).fetchone():
            return

        event_type = str(payload["event_type"])
        trace_id = payload.get("trace_id") or self._get_active_trace(conn, watch_path)
        ok = True

        if event_type == "session_start":
            query = str(payload.get("query") or "")
            result = self._run_cli_result(
                ["innate", "--db", self.db_path, "recall", query, "--format", "prompt"]
            )
            ok = result is not None
            if result:
                trace_id = self._extract_trace_id(result.stdout) or trace_id
                if result.stdout:
                    self.logger.info("session_start recall prompt:\n%s", result.stdout)
            if trace_id:
                self._set_active_trace(conn, watch_path, str(trace_id))
        elif event_type == "session_end":
            ok = self._run_cli(
                ["innate", "--db", self.db_path, "evolve", "--trigger", "manual"]
            )
            self._clear_active_trace(conn, watch_path)
        elif event_type in ("tool_success", "tool_error", "user_feedback"):
            trace_id = trace_id or self._ensure_trace(event_id)
            cmd = ["innate", "--db", self.db_path, "record", str(trace_id)]
            outcome = payload.get("outcome")
            if event_type == "tool_success":
                outcome = outcome or "ok"
            elif event_type == "tool_error":
                outcome = outcome or "fail"
            if outcome:
                cmd.extend(["--outcome", str(outcome)])
            if payload.get("query"):
                cmd.extend(["--query", str(payload["query"])])
            if payload.get("output_summary"):
                cmd.extend(["--output-summary", str(payload["output_summary"])])
            if payload.get("used"):
                cmd.extend(["--used", ",".join(map(str, payload["used"]))])
            if payload.get("nomination"):
                cmd.extend(["--nomination", str(payload["nomination"])])
            if payload.get("priority") is not None:
                cmd.extend(["--priority", str(payload["priority"])])
            feedback = payload.get("feedback") or (payload.get("metadata") or {}).get("feedback")
            if feedback:
                cmd.extend(["--feedback", str(feedback)])
            cmd.extend(["--source", "daemon"])
            ok = self._run_cli(cmd)

        stored_event_type = event_type if ok else f"{event_type}_error"
        conn.execute(
            """INSERT INTO processed_events(event_id, watch_path, trace_id, event_type, ts)
               VALUES (?, ?, ?, ?, ?)""",
            (event_id, watch_path, trace_id, stored_event_type, utc_now_iso()),
        )

    @staticmethod
    def _extract_trace_id(output: str) -> str | None:
        match = re.search(r"<!--\s*innate_trace_id:\s*([A-Za-z0-9_-]+)\s*-->", output)
        return match.group(1) if match else None

    @staticmethod
    def _get_active_trace(conn: sqlite3.Connection, watch_path: str) -> str | None:
        row = conn.execute(
            "SELECT trace_id FROM trace_context WHERE watch_path=?", (watch_path,)
        ).fetchone()
        return row[0] if row else None

    @staticmethod
    def _set_active_trace(conn: sqlite3.Connection, watch_path: str, trace_id: str) -> None:
        conn.execute(
            """INSERT OR REPLACE INTO trace_context(watch_path, trace_id, updated_at)
               VALUES (?, ?, ?)""",
            (watch_path, trace_id, utc_now_iso()),
        )

    @staticmethod
    def _clear_active_trace(conn: sqlite3.Connection, watch_path: str) -> None:
        conn.execute("DELETE FROM trace_context WHERE watch_path=?", (watch_path,))

    def _reset_error_streaks(
        self, watch_path: str, except_error: str | None = None
    ) -> None:
        """仅保留当前文件当前异常类，确保计数表达连续出现."""
        for key in list(self._error_streaks):
            if key[0] == watch_path and key[1] != except_error:
                del self._error_streaks[key]

    def _run_cli(self, cmd: List[str]) -> bool:
        """调用 CLI,失败后退避重试一次."""
        return self._run_cli_result(cmd) is not None

    def _run_cli_result(self, cmd: List[str]) -> subprocess.CompletedProcess[str] | None:
        """调用 CLI,成功时返回结果;失败后退避重试一次."""
        for attempt in range(2):
            try:
                result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
                if result.returncode == 0:
                    return result
                self.logger.error(
                    f"CLI failed ({result.returncode}): {result.stderr.strip()}"
                )
            except Exception as exc:
                self.logger.error(f"CLI failed: {exc}")
            if attempt == 0:
                time.sleep(0.1)
        return None

    def _ensure_trace(self, event_id: str) -> str:
        # Daemon 捕获事件没有上游 trace 时,以 event_id 生成稳定代理.
        return "trace_" + event_id

    @staticmethod
    def stop(pid_file: str) -> None:
        pf = Path(pid_file)
        if not pf.exists():
            print("Daemon not running")
            return
        pid = int(pf.read_text().strip())
        try:
            os.kill(pid, signal.SIGTERM)
            pf.unlink()
            print(f"Sent SIGTERM to {pid}")
        except ProcessLookupError:
            pf.unlink()
            print("Daemon was not running")

    @staticmethod
    def status(pid_file: str, state_db: str | None = None) -> str:
        pf = Path(pid_file)
        if not pf.exists():
            return "stopped"
        pid = pf.read_text().strip()
        if not Path(f"/proc/{pid}").exists():
            return "stopped (stale pid file)"

        state_path = Path(state_db) if state_db else DEFAULT_STATE_DB
        if not state_path.exists():
            return f"running (pid={pid})"
        conn = sqlite3.connect(str(state_path))
        processed = conn.execute("SELECT COUNT(*) FROM processed_events").fetchone()[0]
        errors = conn.execute(
            "SELECT COUNT(*) FROM processed_events WHERE event_type LIKE '%_error'"
        ).fetchone()[0]
        last_row = conn.execute("SELECT MAX(ts) FROM processed_events").fetchone()
        watch_rows = conn.execute(
            "SELECT watch_path FROM watch_state ORDER BY watch_path"
        ).fetchall()
        conn.close()
        last = last_row[0] if last_row and last_row[0] else "-"
        files = ",".join(row[0] for row in watch_rows) or "-"
        return (
            f"running (pid={pid}, watches={len(watch_rows)}, files={files}, "
            f"processed={processed}, last={last}, errors={errors})"
        )
