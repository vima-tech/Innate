"""Storage 层: sqlite-vec 默认实现,预留扩展点."""

from __future__ import annotations

import json
import sqlite3
import warnings
from pathlib import Path
from typing import Any, Dict, List, Optional, Protocol, Tuple, runtime_checkable

import sqlite_vec

from .utils import utc_now_iso


# 期望的 schema 版本(必须与 migrations/ 链末端一致)
EXPECTED_SCHEMA_VERSION = "4.5.1"
MIGRATIONS_DIR = Path(__file__).parent.parent / "migrations"


@runtime_checkable
class VectorStore(Protocol):
    """可注入存储后端的最小合同.

    KnowledgeBase 当前仍依赖 SQL 事务与聚合 helper，因此替代实现需兼容
    Storage 的公开方法。storage_factory 是出生版的注入入口，不在此扩展
    为插件框架。
    """

    db_path: Path
    conn: sqlite3.Connection

    def get_meta(self, key: str, default: str | None = None) -> str | None: ...

    def get_chunk(self, chunk_id: str) -> Dict[str, Any] | None: ...

    def search_vec_content(
        self, query_embedding: List[float], limit: int
    ) -> List[Tuple[str, float]]: ...

    def search_vec_trigger(
        self, query_embedding: List[float], limit: int
    ) -> List[Tuple[str, float]]: ...

    def close(self) -> None: ...


def _ver_tuple(v: str) -> Tuple[int, ...]:
    """'4.5.1' → (4, 5, 1); '4.0' → (4, 0, 0). 固定 3 段便于比较."""
    parts = [int(x) for x in v.split(".") if x.isdigit()]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts[:3])


class Storage:
    """默认 sqlite-vec 存储后端."""

    def __init__(
        self,
        db_path: str | Path,
        content_dim: int = 1024,
        trigger_dim: int = 256,
        read_only: bool = False,
    ):
        self.db_path = Path(db_path)
        self.content_dim = content_dim
        self.trigger_dim = trigger_dim
        self.read_only = read_only
        if not self.read_only:
            self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._conn: sqlite3.Connection | None = None
        self._connect()
        self._init_schema()

    # ------------------------------------------------------------------
    # 连接管理
    # ------------------------------------------------------------------
    def _connect(self) -> None:
        if self.read_only:
            uri = self.db_path.resolve().as_uri() + "?mode=ro"
            self._conn = sqlite3.connect(uri, uri=True, check_same_thread=False)
        else:
            self._conn = sqlite3.connect(str(self.db_path), check_same_thread=False)
        self._conn.enable_load_extension(True)
        sqlite_vec.load(self._conn)
        self._conn.enable_load_extension(False)
        self._conn.row_factory = sqlite3.Row
        if self.read_only:
            self._conn.execute("PRAGMA query_only=ON")
        else:
            self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA foreign_keys=ON")
        if not self.read_only:
            self._conn.execute("PRAGMA synchronous=NORMAL")

    def _init_schema(self) -> None:
        """初始化或迁移到目标 schema 版本.

        启动流程(对齐 §四·五 Schema 版本管理):
          1. 库空 → 直接 exec schema.sql
          2. 已有库且 schema_version 与 EXPECTED 一致 → no-op
          3. 已有库 schema_version < EXPECTED → 按顺序 apply migrations/*.sql
          4. 已有库 schema_version > EXPECTED → 警告但允许(向前兼容)
        """
        schema_file = Path(__file__).with_name("schema.sql")
        if not schema_file.exists():
            schema_file = MIGRATIONS_DIR / "schema.sql"

        # 1. 库是否已存在 meta 表
        has_meta = self._conn.execute(  # type: ignore[union-attr]
            "SELECT name FROM sqlite_master WHERE type='table' AND name='meta'"
        ).fetchone() is not None

        if not has_meta:
            if self.read_only:
                raise RuntimeError(f"shared database has no Innate schema: {self.db_path}")
            # 全新库 — 直接 exec 完整 schema
            if not schema_file.exists():
                raise RuntimeError("schema.sql not found")
            sql = schema_file.read_text(encoding="utf-8")
            sql = sql.replace("embedding float[1024]", f"embedding float[{self.content_dim}]")
            sql = sql.replace("embedding float[256]", f"embedding float[{self.trigger_dim}]")
            self._conn.executescript(sql)  # type: ignore[union-attr]
            self._conn.commit()  # type: ignore[union-attr]
            return

        # 2. 读取当前版本
        row = self._conn.execute(  # type: ignore[union-attr]
            "SELECT value FROM meta WHERE key='schema_version'"
        ).fetchone()
        current = row["value"] if row else None

        if current is None:
            raise RuntimeError(
                "Database has a meta table but no schema_version; "
                "cannot safely infer a migration path"
            )

        cur_t = _ver_tuple(current)
        exp_t = _ver_tuple(EXPECTED_SCHEMA_VERSION)

        if cur_t == exp_t:
            return  # 一致,no-op

        if cur_t > exp_t:
            warnings.warn(
                f"Database schema_version={current} is newer than SDK expected "
                f"{EXPECTED_SCHEMA_VERSION}; forward-compat mode (some new columns "
                f"will be ignored).",
                stacklevel=2,
            )
            return

        if self.read_only:
            raise RuntimeError(
                f"shared database schema_version={current} requires migration to "
                f"{EXPECTED_SCHEMA_VERSION}; open it as a writable library first"
            )

        # 3. cur < exp — 顺序 apply 迁移
        self._apply_migrations(current, EXPECTED_SCHEMA_VERSION)

    def _apply_migrations(self, current: str, target: str) -> None:
        """按 4.x_to_4.y.sql 文件名顺序,apply 从 current 到 target 的所有迁移."""
        if not MIGRATIONS_DIR.exists():
            raise RuntimeError(f"migrations dir not found: {MIGRATIONS_DIR}")

        # 列出所有迁移文件
        files = sorted(MIGRATIONS_DIR.glob("*.sql"))
        # 仅处理命名规范 *_to_*.sql 的文件
        migrations = []
        for f in files:
            name = f.stem
            if "_to_" not in name:
                continue
            f_v, t_v = name.split("_to_", 1)
            migrations.append((_ver_tuple(f_v), _ver_tuple(t_v), f))
        migrations.sort(key=lambda x: x[0])

        cur_t = _ver_tuple(current)
        tgt_t = _ver_tuple(target)
        by_source = {f_v: (t_v, f) for f_v, t_v, f in migrations}
        while cur_t < tgt_t:
            step = by_source.get(cur_t)
            if step is None:
                raise RuntimeError(
                    f"Missing migration script for {'.'.join(map(str, cur_t))} -> {target}"
                )
            t_v, f = step
            if t_v > tgt_t:
                raise RuntimeError(f"Migration {f.name} overshoots target {target}")
            sql = f.read_text(encoding="utf-8")
            # migrations 自身不带事务包裹;executescript 隐式管理
            self._conn.executescript(sql)  # type: ignore[union-attr]
            cur_t = t_v
        self._conn.commit()  # type: ignore[union-attr]
        final = self.get_meta("schema_version")
        if _ver_tuple(final or "0") != tgt_t:
            raise RuntimeError(
                f"Migration chain ended at schema_version={final}, expected {target}"
            )

    @property
    def conn(self) -> sqlite3.Connection:
        if self._conn is None:
            raise RuntimeError("Storage not connected")
        return self._conn

    def close(self) -> None:
        if self._conn:
            self._conn.close()
            self._conn = None

    # ------------------------------------------------------------------
    # 元数据 helper
    # ------------------------------------------------------------------
    def get_meta(self, key: str, default: str | None = None) -> str | None:
        row = self.conn.execute(
            "SELECT value FROM meta WHERE key=?", (key,)
        ).fetchone()
        return row["value"] if row else default

    def set_meta(self, key: str, value: str, *, commit: bool = True) -> None:
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES (?, ?)", (key, value)
        )
        if commit:
            self.conn.commit()

    # ------------------------------------------------------------------
    # chunk CRUD
    # ------------------------------------------------------------------
    def insert_chunk(self, chunk: Dict[str, Any]) -> None:
        cols = ", ".join(chunk.keys())
        placeholders = ", ".join(["?"] * len(chunk))
        sql = f"INSERT INTO chunks ({cols}) VALUES ({placeholders})"
        self.conn.execute(sql, tuple(chunk.values()))

    def insert_vec_content(self, chunk_id: str, embedding: List[float]) -> None:
        self.conn.execute(
            "INSERT INTO vec_content(chunk_id, embedding) VALUES (?, ?)",
            (chunk_id, sqlite_vec.serialize_float32(embedding)),
        )

    def insert_vec_trigger(self, chunk_id: str, embedding: List[float]) -> None:
        self.conn.execute(
            "INSERT INTO vec_trigger(chunk_id, embedding) VALUES (?, ?)",
            (chunk_id, sqlite_vec.serialize_float32(embedding)),
        )

    def insert_chunk_with_vectors(
        self,
        chunk: Dict[str, Any],
        content_embedding: List[float],
        trigger_embedding: List[float],
    ) -> None:
        """原子写入 chunk 与双向量."""
        self.conn.execute("SAVEPOINT insert_chunk_vectors")
        try:
            self.insert_chunk(chunk)
            self.insert_vec_content(chunk["id"], content_embedding)
            self.insert_vec_trigger(chunk["id"], trigger_embedding)
            self.conn.execute("RELEASE SAVEPOINT insert_chunk_vectors")
        except Exception:
            self.conn.execute("ROLLBACK TO SAVEPOINT insert_chunk_vectors")
            self.conn.execute("RELEASE SAVEPOINT insert_chunk_vectors")
            raise

    def get_chunk(self, chunk_id: str) -> Dict[str, Any] | None:
        row = self.conn.execute(
            "SELECT * FROM chunks WHERE id=?", (chunk_id,)
        ).fetchone()
        return dict(row) if row else None

    def update_chunk_state(
        self, chunk_id: str, state: str, state_reason: str | None = None, *, commit: bool = True
    ) -> None:
        now = utc_now_iso()
        if state_reason:
            self.conn.execute(
                "UPDATE chunks SET state=?, state_reason=?, state_updated_at=?, updated_at=? WHERE id=?",
                (state, state_reason, now, now, chunk_id),
            )
        else:
            self.conn.execute(
                "UPDATE chunks SET state=?, state_updated_at=?, updated_at=? WHERE id=?",
                (state, now, now, chunk_id),
            )
        if commit:
            self.conn.commit()

    def update_chunk_confidence(
        self, chunk_id: str, confidence: float, confidence_reason: str
    ) -> None:
        now = utc_now_iso()
        self.conn.execute(
            "UPDATE chunks SET confidence=?, confidence_reason=?, updated_at=? WHERE id=?",
            (confidence, confidence_reason, now, chunk_id),
        )

    def update_chunk_counters(self, chunk_id: str, **kwargs: int | str | None) -> None:
        if not kwargs:
            return
        sets = ", ".join([f"{k}=?" for k in kwargs])
        values = list(kwargs.values()) + [chunk_id]
        self.conn.execute(f"UPDATE chunks SET {sets} WHERE id=?", values)

    def update_chunk_last_used(self, chunk_id: str) -> None:
        now = utc_now_iso()
        self.conn.execute(
            "UPDATE chunks SET last_used_at=?, updated_at=? WHERE id=?",
            (now, now, chunk_id),
        )

    def list_chunks(
        self,
        state: str | None = None,
        origin: str | None = None,
        embed_version: int | None = None,
    ) -> List[Dict[str, Any]]:
        where: List[str] = []
        params: List[Any] = []
        if state:
            where.append("state=?")
            params.append(state)
        if origin:
            where.append("origin=?")
            params.append(origin)
        if embed_version is not None:
            where.append("embed_version=?")
            params.append(embed_version)
        sql = "SELECT * FROM chunks"
        if where:
            sql += " WHERE " + " AND ".join(where)
        rows = self.conn.execute(sql, params).fetchall()
        return [dict(r) for r in rows]

    # ------------------------------------------------------------------
    # 向量检索
    # ------------------------------------------------------------------
    def search_vec_content(self, embedding: List[float], k: int) -> List[Tuple[str, float]]:
        """返回 [(chunk_id, distance), ...] 按距离升序."""
        rows = self.conn.execute(
            "SELECT chunk_id, distance FROM vec_content WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
            (sqlite_vec.serialize_float32(embedding), k),
        ).fetchall()
        return [(r["chunk_id"], r["distance"]) for r in rows]

    def search_vec_trigger(self, embedding: List[float], k: int) -> List[Tuple[str, float]]:
        rows = self.conn.execute(
            "SELECT chunk_id, distance FROM vec_trigger WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
            (sqlite_vec.serialize_float32(embedding), k),
        ).fetchall()
        return [(r["chunk_id"], r["distance"]) for r in rows]

    def delete_vec_content(self, chunk_id: str) -> None:
        self.conn.execute("DELETE FROM vec_content WHERE chunk_id=?", (chunk_id,))

    def delete_vec_trigger(self, chunk_id: str) -> None:
        self.conn.execute("DELETE FROM vec_trigger WHERE chunk_id=?", (chunk_id,))

    def replace_vectors(
        self,
        chunk_id: str,
        content_embedding: List[float],
        trigger_embedding: List[float],
    ) -> None:
        """原子替换双向量;失败时保留旧值."""
        self.conn.execute("SAVEPOINT replace_vectors")
        try:
            self.delete_vec_content(chunk_id)
            self.delete_vec_trigger(chunk_id)
            self.insert_vec_content(chunk_id, content_embedding)
            self.insert_vec_trigger(chunk_id, trigger_embedding)
            self.conn.execute("RELEASE SAVEPOINT replace_vectors")
        except Exception:
            self.conn.execute("ROLLBACK TO SAVEPOINT replace_vectors")
            self.conn.execute("RELEASE SAVEPOINT replace_vectors")
            raise

    # ------------------------------------------------------------------
    # usage_trace
    # ------------------------------------------------------------------
    def append_trace(self, trace: Dict[str, Any]) -> None:
        cols = ", ".join(trace.keys())
        placeholders = ", ".join(["?"] * len(trace))
        self.conn.execute(
            f"INSERT OR IGNORE INTO usage_trace ({cols}) VALUES ({placeholders})",
            tuple(trace.values()),
        )

    # ------------------------------------------------------------------
    # episodic_log
    # ------------------------------------------------------------------
    def insert_log(self, log: Dict[str, Any]) -> None:
        cols = ", ".join(log.keys())
        placeholders = ", ".join(["?"] * len(log))
        self.conn.execute(
            f"INSERT INTO episodic_log ({cols}) VALUES ({placeholders})",
            tuple(log.values()),
        )

    def get_log_by_trace(self, trace_id: str) -> Dict[str, Any] | None:
        row = self.conn.execute(
            "SELECT * FROM episodic_log WHERE trace_id=?", (trace_id,)
        ).fetchone()
        return dict(row) if row else None

    def update_log(self, trace_id: str, updates: Dict[str, Any]) -> None:
        if not updates:
            return
        sets = ", ".join([f"{k}=?" for k in updates])
        values = list(updates.values()) + [trace_id]
        self.conn.execute(
            f"UPDATE episodic_log SET {sets} WHERE trace_id=?", values
        )

    def claim_logs_for_distill(self, run_id: str, batch_size: int, locked_at: str) -> List[Dict[str, Any]]:
        self.conn.execute("BEGIN IMMEDIATE")
        try:
            self.conn.execute(
                """UPDATE episodic_log
                   SET distill_state='screening',
                       distill_run_id=?,
                       distill_locked_at=?
                   WHERE id IN (
                       SELECT id FROM episodic_log
                       WHERE distill_state='new'
                       ORDER BY priority DESC, ts ASC
                       LIMIT ?
                   )""",
                (run_id, locked_at, batch_size),
            )
            self.conn.commit()
        except Exception:
            self.conn.rollback()
            raise
        rows = self.conn.execute(
            "SELECT * FROM episodic_log WHERE distill_run_id=? AND distill_state='screening'",
            (run_id,),
        ).fetchall()
        return [dict(r) for r in rows]

    # ------------------------------------------------------------------
    # deps
    # ------------------------------------------------------------------
    def get_deps(self, src: str, kind: str | None = None) -> List[Dict[str, Any]]:
        if kind:
            rows = self.conn.execute(
                "SELECT * FROM deps WHERE src=? AND kind=?", (src, kind)
            ).fetchall()
        else:
            rows = self.conn.execute(
                "SELECT * FROM deps WHERE src=?", (src,)
            ).fetchall()
        return [dict(r) for r in rows]

    def insert_dep(
        self,
        src: str,
        dst: str,
        kind: str = "hard",
        dst_lib: str | None = None,
        dst_ref: str | None = None,
    ) -> None:
        self.conn.execute(
            """INSERT OR IGNORE INTO deps(src, dst, kind, dst_lib, dst_ref)
               VALUES (?, ?, ?, ?, ?)""",
            (src, dst, kind, dst_lib, dst_ref),
        )

    # ------------------------------------------------------------------
    # invalidated_hashes
    # ------------------------------------------------------------------
    def is_invalidated(self, content_hash: str) -> bool:
        row = self.conn.execute(
            "SELECT 1 FROM invalidated_hashes WHERE content_hash=?", (content_hash,)
        ).fetchone()
        return row is not None

    def insert_invalidated_hash(self, content_hash: str, reason: str | None, ts: str) -> None:
        self.conn.execute(
            "INSERT OR IGNORE INTO invalidated_hashes(content_hash, reason, ts) VALUES (?, ?, ?)",
            (content_hash, reason, ts),
        )

    # ------------------------------------------------------------------
    # chunk_success_traces
    # ------------------------------------------------------------------
    def insert_success_trace(self, chunk_id: str, trace_id: str, ts: str) -> None:
        self.conn.execute(
            "INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts) VALUES (?, ?, ?)",
            (chunk_id, trace_id, ts),
        )

    # ------------------------------------------------------------------
    # aggregate helpers
    # ------------------------------------------------------------------
    def aggregate_success_traces(self, last_ts: str, cutoff_ts: str) -> None:
        self.conn.execute(
            """INSERT OR IGNORE INTO chunk_success_traces(chunk_id, trace_id, ts)
               SELECT u.chunk_id, u.trace_id, MAX(u.ts)
               FROM usage_trace u
               WHERE u.event = 'used'
                 AND u.ts >= ?
                 AND u.ts < ?
                 AND (
                   EXISTS (
                     SELECT 1 FROM usage_trace t
                     WHERE t.trace_id = u.trace_id AND t.event = 'task_ok'
                   )
                   OR EXISTS (
                     SELECT 1 FROM episodic_log l
                     WHERE l.trace_id = u.trace_id AND l.outcome = 'ok'
                   )
                 )
               GROUP BY u.chunk_id, u.trace_id""",
            (last_ts, cutoff_ts),
        )

    def aggregate_counters(self, last_ts: str, cutoff_ts: str) -> None:
        # selected_count / used_count
        self.conn.execute(
            """UPDATE chunks SET
               selected_count = selected_count + (
                 SELECT COUNT(*) FROM usage_trace
                 WHERE chunk_id = chunks.id AND event='selected'
                   AND ts >= ? AND ts < ?
               ),
               used_count = used_count + (
                 SELECT COUNT(*) FROM usage_trace
                 WHERE chunk_id = chunks.id AND event='used'
                   AND ts >= ? AND ts < ?
               )
               WHERE id IN (
                 SELECT DISTINCT chunk_id FROM usage_trace
                 WHERE ts >= ? AND ts < ? AND chunk_id IS NOT NULL
               )""",
            (last_ts, cutoff_ts, last_ts, cutoff_ts, last_ts, cutoff_ts),
        )

    def aggregate_success_counts(self) -> None:
        self.conn.execute(
            """UPDATE chunks SET
               used_success_count = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
               success_trace_ids_count = (SELECT COUNT(*) FROM chunk_success_traces WHERE chunk_id = chunks.id),
               last_success_at = (SELECT MAX(ts) FROM chunk_success_traces WHERE chunk_id = chunks.id)
               WHERE id IN (SELECT DISTINCT chunk_id FROM chunk_success_traces)"""
        )

    # ------------------------------------------------------------------
    # purge helpers
    # ------------------------------------------------------------------
    def purge_stale_screening(self, timeout_minutes: int) -> int:
        cur = self.conn.execute(
            """UPDATE episodic_log
               SET distill_state='failed',
                   distill_note='screening_timeout:' || distill_run_id,
                   distill_run_id=NULL,
                   distill_locked_at=NULL
               WHERE distill_state='screening'
                 AND distill_locked_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)""",
            (f"-{timeout_minutes} minutes",),
        )
        return cur.rowcount

    def purge_old_logs(self, days: int) -> int:
        cur = self.conn.execute(
            """DELETE FROM episodic_log
               WHERE distill_state IN ('distilled','discarded','failed')
                 AND ts < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)""",
            (f"-{days} days",),
        )
        return cur.rowcount

    def purge_open_timeout(self, ttl_days: int, reason: str) -> int:
        cur = self.conn.execute(
            """UPDATE episodic_log
               SET distill_state='discarded',
                   distill_note=?
               WHERE distill_state='open'
                 AND ts < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)""",
            (reason, f"-{ttl_days} days"),
        )
        return cur.rowcount

    def purge_usage_trace(self, cutoff_ts: str) -> int:
        cur = self.conn.execute(
            """DELETE FROM usage_trace
               WHERE ts < ?
                 AND NOT (
                   event='retrieved'
                   AND chunk_id IN (SELECT id FROM chunks WHERE origin='spark')
                 )""",
            (cutoff_ts,),
        )
        return cur.rowcount

    # ------------------------------------------------------------------
    # transaction
    # ------------------------------------------------------------------
    def begin_immediate(self) -> None:
        self.conn.execute("BEGIN IMMEDIATE")

    def commit(self) -> None:
        self.conn.commit()

    def rollback(self) -> None:
        self.conn.rollback()
