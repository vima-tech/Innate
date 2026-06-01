"""Runtime Daemon — 外部独立进程,监听日志/事件 → 调 CLI."""

from __future__ import annotations

import hashlib
import logging
import logging.handlers
import os
import re
import signal
import sqlite3
import subprocess
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
        row = conn.execute("SELECT 1 FROM processed_events WHERE event_id=?", (event_id,)).fetchone()
        if row:
            return

        # 简单模式匹配
        outcome = None
        if (
            any(k in line for k in ("Build successful", "Tests passed"))
            or re.search(r"✓\s*(?:\[\d+\]|\d+)?\s*passed", line)
        ):
            outcome = "ok"
        elif any(k in line for k in ("SyntaxError", "Error:", "FAIL")):
            outcome = "fail"

        trace_id = None
        event_type = outcome or "unknown"
        if outcome:
            trace_id = self._ensure_trace(event_id)
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

    def _run_cli(self, cmd: List[str]) -> bool:
        """调用 CLI,失败后退避重试一次."""
        for attempt in range(2):
            try:
                result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
                if result.returncode == 0:
                    return True
                self.logger.error(
                    f"CLI failed ({result.returncode}): {result.stderr.strip()}"
                )
            except Exception as exc:
                self.logger.error(f"CLI failed: {exc}")
            if attempt == 0:
                time.sleep(0.1)
        return False

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
        watches = conn.execute("SELECT COUNT(*) FROM watch_state").fetchone()[0]
        conn.close()
        last = last_row[0] if last_row and last_row[0] else "-"
        return (
            f"running (pid={pid}, watches={watches}, processed={processed}, "
            f"last={last}, errors={errors})"
        )
