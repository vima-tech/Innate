"""设计文档与实现契约的校准回归测试."""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import tempfile
from pathlib import Path

import pytest
from click.testing import CliRunner

from innate.cli.main import cli
from innate.core import KnowledgeBase
from innate.core.exceptions import InvalidStateError
from innate.core.kb import CurateScope
from innate.core.refine import Refiner
from innate.core.utils import content_hash, utc_now_iso
from innate.daemon.server import DaemonServer


@pytest.fixture
def kb():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path)
    yield k
    k.close()
    os.unlink(path)


def test_spark_recall_is_traced_and_hint_survives_curate(kb):
    """spark 唤起次数应累计,且 Curate 后仍可用于软孵化提示."""
    sid = kb.spark("dialect voice ordering", trigger_desc="voice ordering")
    for _ in range(4):
        kb.recall("voice ordering", include_sparks=True)

    traces = kb.storage.conn.execute(
        "SELECT COUNT(*) AS c FROM usage_trace WHERE chunk_id=? AND event='retrieved'",
        (sid,),
    ).fetchone()["c"]
    assert traces == 4
    assert kb.inspect()["spark_hints"] == [{"id": sid, "recall_count": 4}]

    kb._builtin_curate(CurateScope())
    assert kb.inspect()["spark_hints"] == [{"id": sid, "recall_count": 4}]


def test_invalidated_spark_is_not_recalled(kb):
    """invalidate 是立即作废,被作废 spark 不得继续唤起."""
    sid = kb.spark("invalid idea", trigger_desc="invalid idea")
    kb.invalidate(sid, reason="disproved")
    result = kb.recall("invalid idea", include_sparks=True, trace=False)
    assert not any(spark["id"] == sid for spark in result.sparks)


def test_incubating_spark_remains_eligible_for_hint(kb):
    """未离场 maturity 仍应参与软孵化提示."""
    sid = kb.spark("incubating idea", trigger_desc="incubating idea")
    kb.storage.conn.execute(
        "UPDATE chunks SET maturity='incubating' WHERE id=?", (sid,)
    )
    kb.storage.conn.commit()
    for _ in range(4):
        kb.recall("incubating idea", include_sparks=True)
    assert kb.inspect()["spark_hints"] == [{"id": sid, "recall_count": 4}]


def test_curate_dedupe_never_archives_spark(kb):
    """spark 只能显式 drop/promote 离场,不参与 Curate 去重."""
    sid = kb.spark("same idea")
    spark = kb.storage.get_chunk(sid)
    kb.storage.insert_chunk({
        "id": "knowledge",
        "content": spark["content"],
        "content_hash": spark["content_hash"],
        "origin": "captured",
        "state": "active",
        "confidence": 0.9,
        "created_at": utc_now_iso(),
        "updated_at": utc_now_iso(),
    })
    kb.storage.conn.commit()

    report = kb._builtin_curate(CurateScope())
    assert sid not in report.deduped
    assert kb.storage.get_chunk(sid)["state"] == "active"


def test_curate_dedupe_prefers_protected_chunk(kb):
    """相同 hash 去重时 protected 必须优先成为 canonical."""
    now = utc_now_iso()
    h = content_hash("same")
    kb.storage.insert_chunk({
        "id": "captured",
        "content": "same",
        "content_hash": h,
        "origin": "captured",
        "state": "active",
        "protected": 0,
        "confidence": 0.99,
        "created_at": now,
        "updated_at": now,
    })
    kb.storage.insert_chunk({
        "id": "installed",
        "content": "same",
        "content_hash": h,
        "origin": "installed",
        "state": "active",
        "protected": 1,
        "confidence": 0.1,
        "created_at": now,
        "updated_at": now,
    })
    kb.storage.conn.commit()

    report = kb._builtin_curate(CurateScope())
    assert "captured" in report.deduped
    assert kb.storage.get_chunk("captured")["state_reason"] == "duplicate:installed"
    assert kb.storage.get_chunk("installed")["state"] == "active"


def test_inspect_pending_embed_includes_stale_versions_and_debt_excludes_sparks(kb):
    """inspect 应统计落后向量版本,spark 不得进入知识债务比分母."""
    kb.add("knowledge", kind="note")
    kb.spark("idea")
    kb.storage.set_meta("embed_version", "2")

    info = kb.inspect()
    assert info["pending_embed_rebuild"] == 2
    assert info["knowledge_debt_ratio"] == 0.0


def test_curate_dry_run_does_not_write(kb):
    """CurateScope(dry_run=True) 必须完全只读."""
    old = "2020-01-01T00:00:00.000Z"
    kb.storage.insert_log({
        "id": "stale-log",
        "trace_id": "stale-trace",
        "lib_id": "lib",
        "ts": old,
        "distill_state": "screening",
        "distill_run_id": "worker",
        "distill_locked_at": old,
        "event_source": "sdk",
    })
    kb.storage.conn.commit()
    before_watermark = kb.storage.get_meta("last_agg_ts")

    kb._builtin_curate(CurateScope(dry_run=True))

    log = kb.storage.conn.execute(
        "SELECT distill_state FROM episodic_log WHERE id='stale-log'"
    ).fetchone()
    assert log["distill_state"] == "screening"
    assert kb.storage.get_meta("last_agg_ts") == before_watermark


def test_trim_cannot_drop_hard_dependency(kb):
    """Refiner 返回不完整 hard 闭包时,SDK 必须拒绝该 trim 结果."""
    class DropsDependency(Refiner):
        @property
        def available(self):
            return True

        def refine(self, blocks, query, mode):
            return blocks[:1]

    seed = kb.add("s" * 400, trigger_desc="seed")
    dep = kb.add("d" * 400, trigger_desc="dep")
    kb.storage.insert_dep(seed, dep, kind="hard")
    kb.storage.conn.commit()
    kb.refiner = DropsDependency()

    chunk = kb.storage.get_chunk(seed)
    selected, _, _ = kb._pack(
        [(1.0, chunk)], budget=150, expand_deps="direct", allow_trim=True, query="seed"
    )
    assert selected == []


def test_refiner_failure_falls_back_without_breaking_recall(kb):
    """Refine 是增强路径,异常时应回落到 off 行为."""
    class FailingRefiner(Refiner):
        @property
        def available(self):
            return True

        def refine(self, blocks, query, mode):
            raise RuntimeError("model unavailable")

    cid = kb.add("x" * 800, trigger_desc="large")
    kb.refiner = FailingRefiner()
    selected, _, _ = kb._pack(
        [(1.0, kb.storage.get_chunk(cid))],
        budget=10,
        expand_deps=False,
        allow_trim=True,
        query="large",
    )
    assert selected == []


def test_trim_only_charges_new_members_when_dependency_is_already_selected(kb):
    """共享 hard dep 已入包后,trim 预算只应计算新加入的闭包成员."""
    class TrimToOneToken(Refiner):
        @property
        def available(self):
            return True

        def refine(self, blocks, query, mode):
            return [{**block, "content": "x"} for block in blocks]

    kb.refiner = TrimToOneToken()
    dependency = kb.add("d" * 40)
    seed = kb.add("s" * 40)
    kb.storage.insert_dep(seed, dependency, kind="hard")
    kb.storage.conn.commit()

    selected, _, _ = kb._pack(
        [
            (1.0, kb.storage.get_chunk(dependency)),
            (0.9, kb.storage.get_chunk(seed)),
        ],
        budget=11,
        expand_deps="direct",
        allow_trim=True,
        query="seed",
    )

    assert {chunk["id"] for chunk in selected} == {dependency, seed}


def test_top_limit_keeps_hard_dependency_closure(kb):
    """--top 限制 seed 数量,不能把 hard dependency 从返回结果切掉."""
    seed = kb.add("seed", trigger_desc="seed")
    dep = kb.add("dependency", trigger_desc="dependency")
    kb.storage.insert_dep(seed, dep, kind="hard")
    kb.storage.conn.commit()

    block, exceeded = kb._build_block(seed, "direct")
    assert exceeded is False
    visible = kb._limit_knowledge(block, top=1, expand_deps="direct")
    assert {chunk["id"] for chunk in visible} == {seed, dep}


def test_closure_allows_exactly_three_dependency_hops(kb):
    """完整闭包深度 <=3 可用,第 4 跳才触发丢弃."""
    ids = [kb.add(letter) for letter in "abcde"]
    for source, target in zip(ids, ids[1:]):
        kb.storage.insert_dep(source, target, kind="hard")
    kb.storage.conn.commit()

    block, exceeded = kb._build_block(ids[1], "closure")
    assert exceeded is False
    assert [chunk["id"] for chunk in block] == ids[1:]
    assert kb._build_block(ids[0], "closure") == ([], True)


def test_agent_skill_is_forced_to_pending(kb):
    """--source agent 无论 kind 都不得直接污染 active 池."""
    cid = kb.add("agent generated skill", kind="skill", source="agent")
    chunk = kb.storage.get_chunk(cid)
    assert chunk["origin"] == "captured"
    assert chunk["state"] == "pending"
    assert chunk["protected"] == 0


def test_curate_scope_limits_archive_targets(kb):
    """CurateScope(origin=...) 仅治理指定来源."""
    old = "2020-01-01T00:00:00.000Z"
    captured = kb.add("captured")
    installed = kb.add("installed", kind="skill")
    kb.storage.conn.execute(
        """UPDATE chunks SET protected=0, confidence=0.1, last_used_at=?
           WHERE id IN (?, ?)""",
        (old, captured, installed),
    )
    kb.storage.conn.commit()

    kb._builtin_curate(CurateScope(origin="captured"))
    assert kb.storage.get_chunk(captured)["state"] == "archived"
    assert kb.storage.get_chunk(installed)["state"] == "active"


def test_custom_embedding_dimensions_are_injected_for_new_database(tmp_path):
    """新库 vec0 维度来自 provider,不是固定写死 1024/256."""
    class TinyEmbedding:
        content_dim = 4
        trigger_dim = 2

        def embed_content(self, text):
            return [1.0, 0.0, 0.0, 0.0]

        def embed_trigger(self, text):
            return [1.0, 0.0]

    kb = KnowledgeBase(str(tmp_path / "tiny.db"), embedding=TinyEmbedding())
    kb.add("tiny")
    assert kb.recall("tiny").knowledge
    assert kb.storage.get_meta("content_dim") == "4"
    assert kb.storage.get_meta("trigger_dim") == "2"
    kb.close()


def test_storage_rejects_unknown_schema_without_version(tmp_path):
    """已有 meta 但无 schema_version 时不能猜测为最新版本."""
    path = tmp_path / "unknown.db"
    conn = sqlite3.connect(path)
    conn.execute("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)")
    conn.commit()
    conn.close()
    with pytest.raises(RuntimeError, match="no schema_version"):
        KnowledgeBase(str(path))


def test_promoted_spark_cannot_be_dropped_or_promoted_again(kb):
    """promoted/dropped 是 spark 离场终态."""
    sid = kb.spark("idea")
    kb.promote_spark(sid)
    with pytest.raises(InvalidStateError):
        kb.promote_spark(sid)
    with pytest.raises(InvalidStateError):
        kb.drop_spark(sid)


def test_record_updates_event_source_without_reopening_terminal_log(kb):
    """补写来源审计字段时,已蒸馏终态不得重新入队."""
    result = kb.recall("q")
    kb.record(result.trace_id, outcome="ok", output_summary="summary")
    kb.evolve()
    assert kb.storage.get_log_by_trace(result.trace_id)["distill_state"] == "distilled"

    kb.record(result.trace_id, source="hook", nomination="late audit")
    log = kb.storage.get_log_by_trace(result.trace_id)
    assert log["event_source"] == "hook"
    assert log["distill_state"] == "distilled"


def test_dummy_embedding_is_stable_across_processes():
    """默认 embedding 不能依赖进程随机化的 Python hash()."""
    code = (
        "import json; "
        "from innate.core.embedding import DummyEmbeddingProvider; "
        "print(json.dumps(DummyEmbeddingProvider().embed_content('stable')[:4]))"
    )
    first = subprocess.run(["python", "-c", code], capture_output=True, text=True, check=True)
    second = subprocess.run(["python", "-c", code], capture_output=True, text=True, check=True)
    assert json.loads(first.stdout) == json.loads(second.stdout)


def test_cli_text_hides_trace_id_and_uuid_trace_can_be_inspected(kb):
    """text 仅供人读不交接 trace_id;inspect 应识别 recall 生成的 UUID."""
    runner = CliRunner()
    result = runner.invoke(cli, ["--db", kb.db_path, "recall", "q", "--format", "text"])
    assert result.exit_code == 0
    assert "trace_id=" not in result.output

    recall_result = kb.recall("q")
    detail = runner.invoke(cli, ["--db", kb.db_path, "inspect", recall_result.trace_id])
    assert detail.exit_code == 0
    assert recall_result.trace_id in detail.output


def test_daemon_tracks_each_log_file_and_replays_duplicate_lines_at_distinct_offsets(tmp_path, monkeypatch):
    """Daemon 偏移按文件保存;相同行出现在不同 offset 时是两个事件."""
    watch = tmp_path / "logs"
    watch.mkdir()
    (watch / "a.log").write_text("Tests passed\nTests passed\n", encoding="utf-8")
    (watch / "b.log").write_text("Build successful\n", encoding="utf-8")
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[str(watch)],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    srv._process_watch(watch)

    conn = sqlite3.connect(srv.state_db)
    states = conn.execute("SELECT watch_path FROM watch_state ORDER BY watch_path").fetchall()
    events = conn.execute("SELECT COUNT(*) FROM processed_events").fetchone()[0]
    conn.close()
    assert states == [(str(watch / "a.log"),), (str(watch / "b.log"),)]
    assert events == 3
    assert len(calls) == 3
    assert all(cmd[:3] == ["innate", "--db", str(tmp_path / "knowledge.db")] for cmd in calls)


def test_daemon_retries_cli_failure_once(tmp_path, monkeypatch):
    """CLI 非零退出时最多重试一次,仍失败也不阻塞日志消费."""
    attempts = []

    def fake_run(cmd, **kwargs):
        attempts.append(cmd)
        return subprocess.CompletedProcess(cmd, 1, "", "failed")

    monkeypatch.setattr(subprocess, "run", fake_run)
    monkeypatch.setattr("time.sleep", lambda _: None)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    srv._handle_line("Tests passed", "watch", "a.log", "inode", 0, conn)
    conn.commit()
    event_type = conn.execute("SELECT event_type FROM processed_events").fetchone()[0]
    conn.close()
    assert len(attempts) == 2
    assert event_type == "ok_error"


def test_daemon_matches_numbered_pass_line(tmp_path, monkeypatch):
    """通用 watcher 应识别设计文档中的 `✓ [N] passed` 模式."""
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    srv._handle_line("✓ [12] passed", "watch", "a.log", "inode", 0, conn)
    conn.commit()
    event_type = conn.execute("SELECT event_type FROM processed_events").fetchone()[0]
    conn.close()
    assert event_type == "ok"
    assert len(calls) == 1


def test_daemon_hook_json_preserves_event_and_trace_ids(tmp_path, monkeypatch):
    """Hook JSON 应保留上游关联键并映射完整 record 参数."""
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    payload = {
        "event_id": "event-1",
        "event_type": "tool_success",
        "trace_id": "trace-upstream",
        "query": "task",
        "output_summary": "summary",
        "outcome": "ok",
        "used": ["c1", "c2"],
        "nomination": "worth distilling",
        "priority": 7,
    }
    line = json.dumps(payload)
    srv._handle_line(line, "watch", "a.log", "inode", 0, conn)
    srv._handle_line(line, "watch", "a.log", "inode", 0, conn)
    conn.commit()

    row = conn.execute(
        "SELECT event_id, trace_id, event_type FROM processed_events"
    ).fetchone()
    conn.close()
    assert row == ("event-1", "trace-upstream", "tool_success")
    assert len(calls) == 1
    assert calls[0] == [
        "innate", "--db", str(tmp_path / "knowledge.db"), "record", "trace-upstream",
        "--outcome", "ok", "--query", "task", "--output-summary", "summary",
        "--used", "c1,c2", "--nomination", "worth distilling", "--priority", "7",
        "--source", "daemon",
    ]


def test_daemon_session_start_context_is_reused_by_log_outcome(tmp_path, monkeypatch):
    """Session Start recall 产生的 trace 应贯穿后续旁路日志."""
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        if "recall" in cmd:
            return subprocess.CompletedProcess(
                cmd, 0, "<!-- innate_trace_id: trace-session -->", ""
            )
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    srv._handle_line(
        json.dumps({
            "event_id": "start",
            "event_type": "session_start",
            "trace_id": "trace-upstream-before-recall",
            "query": "project",
        }),
        "watch", "a.log", "inode", 0, conn,
    )
    srv._handle_line("Tests passed", "watch", "a.log", "inode", 100, conn)
    conn.commit()
    conn.close()

    assert calls[0][-4:] == ["recall", "project", "--format", "prompt"]
    assert calls[1][3:6] == ["record", "trace-session", "--outcome"]


def test_daemon_session_end_triggers_evolve(tmp_path, monkeypatch):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    srv._set_active_trace(conn, "watch", "trace-old")
    srv._handle_line(
        json.dumps({"event_id": "end", "event_type": "session_end"}),
        "watch", "a.log", "inode", 0, conn,
    )
    assert srv._get_active_trace(conn, "watch") is None
    conn.commit()
    conn.close()
    assert calls == [[
        "innate", "--db", str(tmp_path / "knowledge.db"),
        "evolve", "--trigger", "manual",
    ]]


def test_daemon_repeated_exception_class_triggers_failure(tmp_path, monkeypatch):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    for offset in range(3):
        srv._handle_line("ValueError raised", "watch", "a.log", "inode", offset, conn)
    conn.commit()
    conn.close()
    assert len(calls) == 1
    assert "fail" in calls[0]


def test_daemon_exception_streak_is_reset_by_unrelated_line(tmp_path, monkeypatch):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, "", "")

    monkeypatch.setattr(subprocess, "run", fake_run)
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    srv.logger = srv._setup_logging()
    conn = sqlite3.connect(srv.state_db)
    srv._handle_line("ValueError raised", "watch", "a.log", "inode", 0, conn)
    srv._handle_line("ordinary log line", "watch", "a.log", "inode", 1, conn)
    srv._handle_line("ValueError raised", "watch", "a.log", "inode", 2, conn)
    srv._handle_line("ValueError raised", "watch", "a.log", "inode", 3, conn)
    conn.commit()
    conn.close()
    assert calls == []


def test_daemon_status_lists_watched_log_files(tmp_path):
    pid_file = tmp_path / "daemon.pid"
    pid_file.write_text(str(os.getpid()), encoding="utf-8")
    srv = DaemonServer(
        db_path=str(tmp_path / "knowledge.db"),
        watch_dirs=[],
        state_db=str(tmp_path / "state.db"),
        log_file=str(tmp_path / "daemon.log"),
    )
    srv._init_state_db()
    conn = sqlite3.connect(srv.state_db)
    conn.execute(
        """INSERT INTO watch_state(
               watch_path, last_processed_offset, last_processed_inode, updated_at
           ) VALUES (?, 0, 'inode', ?)""",
        ("/tmp/example.log", utc_now_iso()),
    )
    conn.commit()
    conn.close()

    status = DaemonServer.status(str(pid_file), state_db=str(srv.state_db))

    assert "files=/tmp/example.log" in status
