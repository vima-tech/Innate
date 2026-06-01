"""Runtime Daemon — 外部独立进程,监听日志/事件 → 调 CLI."""

from __future__ import annotations

import hashlib
import json
import logging
import logging.handlers
import os
import signal
import sqlite3
import subprocess
import sys
import time
from pathlib import Path
from typing import List

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
        log_max_bytes: int = DEFAULT_LOG_MAX_BYTES,
        log_backup_count: int = DEFAULT_LOG_BACKUP_COUNT,
    ):
        self.db_path = db_path
        self.watch_dirs = [Path(d) for d in watch_dirs if d and Path(d).exists()]
        self.pid_file = Path(pid_file)
        self.log_file = Path(log_file) if log_file else DEFAULT_LOG
        self.log_max_bytes = log_max_bytes
        self.log_backup_count = log_backup_count
        self.state_db = Path(DEFAULT_STATE_DB)
        self._running = False

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

        # daemonize (简化:后台运行)
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
        row = conn.execute(
            "SELECT last_processed_offset FROM watch_state WHERE watch_path=?",
            (str(wd),)
        ).fetchone()
        offset = row[0] if row else 0

        for log_file in sorted(wd.glob("*.log")):
            try:
                st = log_file.stat()
                inode = f"{st.st_ino}"
                with open(log_file, "r", encoding="utf-8", errors="ignore") as f:
                    f.seek(offset)
                    for line in f:
                        offset = f.tell()
                        self._handle_line(line, str(wd), str(log_file), conn)
                conn.execute(
                    """INSERT OR REPLACE INTO watch_state(watch_path, last_processed_offset, last_processed_inode, updated_at)
                       VALUES (?, ?, ?, ?)""",
                    (str(wd), offset, inode, utc_now_iso()),
                )
            except Exception as exc:
                self.logger.warning(f"process {log_file} error: {exc}")
        conn.commit()
        conn.close()

    def _handle_line(self, line: str, watch_path: str, file_path: str, conn: sqlite3.Connection) -> None:
        line = line.strip()
        if not line:
            return
        event_id = hashlib.sha256(f"{file_path}:{line}".encode()).hexdigest()[:16]
        row = conn.execute("SELECT 1 FROM processed_events WHERE event_id=?", (event_id,)).fetchone()
        if row:
            return

        # 简单模式匹配
        outcome = None
        if any(k in line for k in ("Build successful", "Tests passed", "✓ passed")):
            outcome = "ok"
        elif any(k in line for k in ("SyntaxError", "Error:", "FAIL")):
            outcome = "fail"

        if outcome:
            trace_id = self._ensure_trace(line)
            cmd = ["innate", "record", trace_id, "--outcome", outcome, "--source", "daemon"]
            try:
                subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            except Exception as exc:
                self.logger.error(f"record failed: {exc}")

        conn.execute(
            "INSERT INTO processed_events(event_id, watch_path, event_type, ts) VALUES (?, ?, ?, ?)",
            (event_id, watch_path, outcome or "unknown", utc_now_iso()),
        )

    def _ensure_trace(self, line: str) -> str:
        # 简化:用 hash 做 trace_id 代理
        return "trace_" + hashlib.sha256(line.encode()).hexdigest()[:16]

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
    def status(pid_file: str) -> str:
        pf = Path(pid_file)
        if not pf.exists():
            return "stopped"
        pid = pf.read_text().strip()
        if Path(f"/proc/{pid}").exists():
            return f"running (pid={pid})"
        return "stopped (stale pid file)"
