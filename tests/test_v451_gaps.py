"""v4.5.1 补缺路径 + 新增扩展点测试."""

from __future__ import annotations

import os
import tempfile
import warnings

import pytest

from innate.core import KnowledgeBase
from innate.core.kb import CurateScope
from innate.core.refine import (
    Distiller,
    HeuristicDistiller,
    NullRefiner,
    Refiner,
)
from innate.core.storage import EXPECTED_SCHEMA_VERSION, Storage
from innate.core.utils import content_hash, utc_now_iso


@pytest.fixture
def kb():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path)
    yield k
    k.storage.close()
    os.unlink(path)


# =====================================================================
# 迁移 runner
# =====================================================================

def test_migration_runner_fresh_db(kb):
    """新库应写入 schema_version=EXPECTED."""
    v = kb.storage.get_meta("schema_version")
    assert v == EXPECTED_SCHEMA_VERSION


def test_migration_runner_picks_up_mid_version():
    """已有 v4.0.0 schema 的库打开时自动 apply v4.0→4.1 ... →4.5.1.

    构造一个 v4.0 schema: 包含所有 v4.0 已有列(满足后续 migrations 的 ALTER 前提),
    但缺少 v4.1+ 新增列, 验证 runner 能按顺序补齐.
    """
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    # v4.0 schema — 含所有 v4.0 已有列, 缺 v4.1+ 新增列.
    # 设计 §四·五 + §四 完整 DDL 反推的 v4.0 baseline.
    v4_0_sql = """
    PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS chunks (
        id TEXT PRIMARY KEY,
        skill_name TEXT,
        seq INTEGER DEFAULT 0,
        content TEXT NOT NULL,
        trigger_desc TEXT,
        anti_trigger_desc TEXT,
        content_hash TEXT NOT NULL,
        token_count INTEGER,
        origin TEXT NOT NULL CHECK(origin IN ('installed','distilled','captured','spark')),
        source TEXT,
        maturity TEXT,
        related_ids TEXT,
        protected INTEGER NOT NULL DEFAULT 0,
        state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('pending','active','archived')),
        state_reason TEXT,
        state_updated_at TEXT,
        confidence REAL NOT NULL DEFAULT 0.5,
        confidence_reason TEXT,
        version INTEGER NOT NULL DEFAULT 1,
        distilled_from TEXT,
        parent_id TEXT,
        selected_count INTEGER NOT NULL DEFAULT 0,
        used_count INTEGER NOT NULL DEFAULT 0,
        last_agg_ts TEXT,
        embed_version INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        last_used_at TEXT
    );
    CREATE TABLE IF NOT EXISTS usage_trace (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        trace_id TEXT NOT NULL,
        chunk_id TEXT,
        event TEXT NOT NULL CHECK(event IN ('retrieved','selected','refined','used','task_ok','task_fail')),
        strength REAL DEFAULT 1.0,
        similarity REAL,
        tokens INTEGER,
        rank INTEGER,
        refine_mode TEXT,
        ts TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS episodic_log (
        id TEXT PRIMARY KEY,
        trace_id TEXT NOT NULL,
        lib_id TEXT NOT NULL,
        ts TEXT NOT NULL,
        query TEXT,
        recall_snapshot TEXT,
        output TEXT,
        outcome TEXT,
        source TEXT,  -- v4.4 字段, v4.5 迁移为 event_source
        nomination TEXT,
        priority INTEGER NOT NULL DEFAULT 0,
        distill_state TEXT NOT NULL DEFAULT 'open',
        distill_note TEXT,
        distill_prompt_tokens INTEGER,
        distill_completion_tokens INTEGER
    );
    INSERT INTO meta(key, value) VALUES('schema_version', '4.0.0');
    """
    import sqlite3
    conn = sqlite3.connect(path)
    conn.executescript(v4_0_sql)
    conn.commit()
    conn.close()

    # 打开 — 应自动 apply 所有迁移
    k = KnowledgeBase(path)
    v = k.storage.get_meta("schema_version")
    assert v == EXPECTED_SCHEMA_VERSION
    # v4.1 加的字段应存在
    cols = [row[1] for row in k.storage.conn.execute("PRAGMA table_info(chunks)").fetchall()]
    assert "used_success_count" in cols
    assert "success_trace_ids_count" in cols
    assert "last_success_at" in cols
    # v4.1 加到 usage_trace
    cols_ut = [row[1] for row in k.storage.conn.execute("PRAGMA table_info(usage_trace)").fetchall()]
    assert "source" in cols_ut
    # v4.2 加到 episodic_log
    cols_ep = [row[1] for row in k.storage.conn.execute("PRAGMA table_info(episodic_log)").fetchall()]
    assert "output_summary" in cols_ep
    # chunk_success_traces 表 (v4.2)
    tbls = [r[0] for r in k.storage.conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='chunk_success_traces'"
    ).fetchall()]
    assert "chunk_success_traces" in tbls
    # v4.5 加的字段
    assert "event_source" in cols_ep
    assert "distill_run_id" in cols_ep
    assert "distill_locked_at" in cols_ep
    # v4.5.1 加的索引存在
    idx_names = [r[0] for r in k.storage.conn.execute(
        "SELECT name FROM sqlite_master WHERE type='index' AND name='idx_log_distill_run'"
    ).fetchall()]
    assert "idx_log_distill_run" in idx_names
    k.storage.close()
    os.unlink(path)


def test_migration_runner_noop_when_current():
    """已是当前版本时 no-op(不重复跑 migration)."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k1 = KnowledgeBase(path)
    k1.storage.close()

    # 二次打开 — no-op
    k2 = KnowledgeBase(path)
    assert k2.storage.get_meta("schema_version") == EXPECTED_SCHEMA_VERSION
    k2.storage.close()
    os.unlink(path)


def test_migration_runner_newer_warns():
    """schema_version > EXPECTED 时向前兼容,只警告."""
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    # 初始建库
    k1 = KnowledgeBase(path)
    k1.storage.close()

    # 手动改为未来版本
    conn = __import__("sqlite3").connect(path)
    conn.execute("UPDATE meta SET value='4.6.0' WHERE key='schema_version'")
    conn.commit()
    conn.close()

    with warnings.catch_warnings(record=True) as w:
        warnings.simplefilter("always")
        k2 = KnowledgeBase(path)
        # 警告应出现
        new_warnings = [x for x in w if "schema_version" in str(x.message).lower()]
        assert len(new_warnings) >= 1
    k2.storage.close()
    os.unlink(path)


# =====================================================================
# CTE 深度上限 → 丢弃 seed
# =====================================================================

def test_cte_depth_limit_drops_seed(kb):
    """depth=3 触达 → 丢弃 seed,不送半截 hard 依赖."""
    # 建一个 4 层深的链 a→b→c→d
    a = kb.add("a", kind="note")
    b = kb.add("b", kind="note")
    c = kb.add("c", kind="note")
    d = kb.add("d", kind="note")
    kb.storage.insert_dep(a, b, kind="hard")
    kb.storage.insert_dep(b, c, kind="hard")
    kb.storage.insert_dep(c, d, kind="hard")
    kb.storage.conn.commit()

    # closure 展开,深度上限 3
    # seed=a 走 closure 需要展开 3 层(a→b→c),再下一层(c→d)超限 → 丢弃
    block, exceeded = kb._build_block(a, "closure")
    assert exceeded is True
    assert block == []
    # 'direct' 模式只展开一层,不应超限
    block2, exceeded2 = kb._build_block(a, "direct")
    assert exceeded2 is False
    assert any(c["id"] == a for c in block2)


def test_cte_depth_limit_recorded_in_recall(kb):
    """召回时 depth-skipped 应进 usage_trace (refine_mode=skipped:dep_depth_limit)."""
    a = kb.add("a", kind="note")
    b = kb.add("b", kind="note")
    c = kb.add("c", kind="note")
    d = kb.add("d", kind="note")
    kb.storage.insert_dep(a, b, kind="hard")
    kb.storage.insert_dep(b, c, kind="hard")
    kb.storage.insert_dep(c, d, kind="hard")
    kb.storage.conn.commit()

    # 让 a 成为 recall 命中 — 改用更激进的嵌入
    class SameVec:
        def __init__(self, dim=1024):
            self._dim = dim
        @property
        def content_dim(self): return self._dim
        @property
        def trigger_dim(self): return 256
        def embed_content(self, t): return [1.0] + [0.0] * (self._dim - 1)
        def embed_trigger(self, t): return [1.0] + [0.0] * 255

    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path, embedding=SameVec())
    a2 = k.add("a", kind="note")
    b2 = k.add("b", kind="note")
    c2 = k.add("c", kind="note")
    d2 = k.add("d", kind="note")
    k.storage.insert_dep(a2, b2, kind="hard")
    k.storage.insert_dep(b2, c2, kind="hard")
    k.storage.insert_dep(c2, d2, kind="hard")
    k.storage.conn.commit()

    result = k.recall("anything", budget=100000, expand_deps="closure", trace=True)
    # a 在 result.depth_skipped 中
    assert a2 in result.depth_skipped
    # usage_trace 应有 refine_mode=skipped:dep_depth_limit 的 retrieved 行
    rows = k.storage.conn.execute(
        "SELECT * FROM usage_trace WHERE chunk_id=? AND refine_mode='skipped:dep_depth_limit'",
        (a2,),
    ).fetchall()
    assert len(rows) >= 1
    k.storage.close()
    os.unlink(path)


# =====================================================================
# install 同 hash 去重
# =====================================================================

def test_install_dedup_returns_existing_id(kb):
    """install (kind=skill) 同 content_hash 重复写入应返回已有 id."""
    c1 = kb.add("same skill content", kind="skill")
    c2 = kb.add("same skill content", kind="skill")
    assert c1 == c2  # 去重,返回同一 id
    # chunk 总数应只有 1
    cnt = kb.storage.conn.execute("SELECT COUNT(*) AS c FROM chunks").fetchone()["c"]
    assert cnt == 1


def test_install_dedup_skips_archived(kb):
    """若已有同 hash 但已 archived,允许重新写入."""
    c1 = kb.add("archived-then-reused", kind="note")
    # 用 SQL 强行 archive(绕过 protected 检查; note 本来就不 protected)
    kb.storage.update_chunk_state(c1, "archived", "test_setup")
    c2 = kb.add("archived-then-reused", kind="note")
    # 因 c1 已 archived, c2 是新 id
    assert c2 != c1
    cnt = kb.storage.conn.execute("SELECT COUNT(*) AS c FROM chunks").fetchone()["c"]
    assert cnt == 2


# =====================================================================
# Refiner / Distiller 接口
# =====================================================================

def test_null_refiner_unavailable():
    """NullRefiner 默认 available=False — 装包时不会触发 trim 路径."""
    r = NullRefiner()
    assert r.available is False
    assert r.refine([{"id": "x"}], "q", "trim") == [{"id": "x"}]


def test_custom_refiner_trim():
    """自定义 Refiner 实现 trim."""
    class TrimByHalf(Refiner):
        @property
        def available(self): return True
        def refine(self, blocks, query, mode):
            if mode != "trim":
                return blocks
            # 简化:只保留前一半 block
            return blocks[: max(1, len(blocks) // 2)]

    r = TrimByHalf()
    assert r.available is True
    out = r.refine([{"id": "a"}, {"id": "b"}, {"id": "c"}], "q", "trim")
    assert len(out) == 1


def test_distiller_returns_none_for_empty_log():
    """Distiller 返回 None 时 evolve 标 insufficient_material."""
    d = HeuristicDistiller()
    assert d.distill({"query": ""}, embedder=None) is None
    assert d.distill({"output_summary": "x", "query": ""}, embedder=None) is not None


def test_custom_distiller_used_by_kb(kb):
    """自定义 Distiller 被 _distill_one 调用."""
    called = {"n": 0}

    class CapturingDistiller(Distiller):
        def distill(self, log, embedder):
            called["n"] += 1
            return {"content": "custom: " + (log.get("query") or ""), "trigger_desc": "t", "anti_trigger_desc": ""}

    k = KnowledgeBase(kb.db_path, distiller=CapturingDistiller())
    r = k.recall("q", budget=1000)
    k.record(r.trace_id, outcome="ok", output_summary="s", used=[])
    k.evolve(trigger="manual")
    assert called["n"] >= 1
    # 自定义内容应写入 chunk
    rows = k.storage.conn.execute("SELECT content FROM chunks WHERE origin='distilled'").fetchall()
    assert any("custom:" in row["content"] for row in rows)


# =====================================================================
# 知识债务比公式
# =====================================================================

def test_knowledge_debt_ratio_with_zombie(kb):
    """僵尸块(conf 0.4-0.6)应计入 debt ratio. 7 天内的块不计入."""
    from datetime import datetime, timedelta, timezone
    old = (datetime.now(timezone.utc) - timedelta(days=10)).strftime(
        "%Y-%m-%dT%H:%M:%S."
    ) + "000Z"
    now = utc_now_iso()
    # 1 zombie(10 天前, conf 0.5) + 1 fresh zombie(刚加入, conf 0.5) + 1 normal(老, conf 0.8)
    rows = [
        {"id": "old-zombie", "conf": 0.5, "created": old},
        {"id": "fresh-zombie", "conf": 0.5, "created": now},
        {"id": "old-normal", "conf": 0.8, "created": old},
    ]
    for r in rows:
        kb.storage.insert_chunk({
            "id": r["id"],
            "content": "x",
            "content_hash": f"h-{r['id']}",
            "origin": "captured",
            "state": "active",
            "confidence": r["conf"],
            "selected_count": 0, "used_count": 0,
            "used_success_count": 0, "success_trace_ids_count": 0,
            "created_at": r["created"],
            "updated_at": r["created"],
        })
    kb.storage.conn.commit()
    info = kb.inspect()
    # 只有老僵尸计入,新鲜僵尸不计入
    assert info["chunks"]["zombie"] == 1
    # valid=3 (active), pending=0, zombie=1 → debt = 1/3 ≈ 0.33
    assert 0.3 <= info["knowledge_debt_ratio"] <= 0.4


# =====================================================================
# innnate/__main__
# =====================================================================

def test_python_m_innate_invokable():
    """python -m innate 应可调用(以前报'不能直接执行')."""
    import subprocess
    r = subprocess.run(
        ["python", "-m", "innate", "--help"],
        capture_output=True, text=True,
    )
    assert r.returncode == 0
    assert "Usage:" in r.stdout


# =====================================================================
# Daemon 日志轮转
# =====================================================================

def test_daemon_log_uses_rotating_handler():
    """Daemon 启动时 logger 应使用 RotatingFileHandler."""
    import logging.handlers
    from innate.daemon.server import DaemonServer

    fd, db_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    log_path = tempfile.mktemp(suffix=".log")
    pid_path = tempfile.mktemp(suffix=".pid")
    state_path = tempfile.mktemp(suffix=".sqlite")

    srv = DaemonServer(
        db_path=db_path,
        watch_dirs=[],
        pid_file=pid_path,
        log_file=log_path,
        log_max_bytes=1024,
        log_backup_count=2,
    )
    # 直接调 _setup_logging 检查 handler 类型
    logger = srv._setup_logging()
    found_rotating = any(
        isinstance(h, logging.handlers.RotatingFileHandler) for h in logger.handlers
    )
    assert found_rotating
    # 清理
    for h in logger.handlers:
        h.close()
    os.unlink(db_path)
    if os.path.exists(log_path):
        os.unlink(log_path)
    if os.path.exists(pid_path):
        os.unlink(pid_path)


# =====================================================================
# soft 依赖评分
# =====================================================================

def test_soft_dep_bonus_applied(kb):
    """soft dep 应为对应 seed 加分(5%)."""
    a = kb.add("a", kind="note")
    b = kb.add("b", kind="note")
    kb.storage.insert_dep(a, b, kind="soft")
    kb.storage.conn.commit()
    chunks = {r["id"]: dict(r) for r in kb.storage.conn.execute("SELECT * FROM chunks").fetchall()}
    info = {a: {"chunk": chunks[a], "sim_content": 0.5, "sim_trigger": 0.5},
            b: {"chunk": chunks[b], "sim_content": 0.0, "sim_trigger": 0.0}}
    kb._apply_soft_dep_bonus(info)
    # a 有 soft dep, sim_content 应被加成
    assert info[a]["sim_content"] == pytest.approx(0.5 * 1.05)
    # b 没有 soft dep, 不变
    assert info[b]["sim_content"] == 0.0


# =====================================================================
# RecallResult.depth_skipped 字段
# =====================================================================

def test_recall_result_has_depth_skipped(kb):
    """RecallResult 应有 depth_skipped 字段(默认空 list)."""
    from innate.core import RecallResult
    r = RecallResult()
    assert r.depth_skipped == []
