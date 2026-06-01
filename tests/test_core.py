"""基础正确性测试."""

import os
import tempfile

import pytest

from innate.core import KnowledgeBase
from innate.core.exceptions import OutcomeConflictError


@pytest.fixture
def kb():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path)
    yield k
    k.storage.close()
    os.unlink(path)


def test_add_note(kb):
    cid = kb.add("测试笔记内容", kind="note", trigger_desc="测试触发")
    assert cid
    chunk = kb.storage.get_chunk(cid)
    assert chunk["origin"] == "captured"
    assert chunk["state"] == "active"
    assert chunk["confidence"] == 0.60


def test_add_skill(kb):
    cid = kb.add("skill内容", kind="skill")
    chunk = kb.storage.get_chunk(cid)
    assert chunk["origin"] == "installed"
    assert chunk["state"] == "active"
    assert chunk["protected"] == 1
    assert chunk["confidence"] == 0.85


def test_spark(kb):
    sid = kb.spark("灵感内容")
    chunk = kb.storage.get_chunk(sid)
    assert chunk["origin"] == "spark"
    assert chunk["maturity"] == "seed"


def test_recall_empty(kb):
    result = kb.recall("query", budget=1000)
    assert result.empty is True
    assert result.trace_id


def test_recall_with_chunks(kb):
    kb.add("Python 列表推导式优化", kind="note", trigger_desc="python list comprehension")
    kb.add("SQL 索引最佳实践", kind="note", trigger_desc="sql index optimization")
    result = kb.recall("python 优化", budget=1000)
    # dummy embedding 是确定性随机,可能召回也可能不召回,这里只验证结构
    assert result.trace_id
    assert isinstance(result.knowledge, list)


def test_record_and_feedback(kb):
    result = kb.recall("test", budget=1000)
    kb.record(result.trace_id, outcome="ok", used=[], feedback="up")
    log = kb.storage.get_log_by_trace(result.trace_id)
    assert log["outcome"] == "ok"


def test_outcome_conflict(kb):
    result = kb.recall("test", budget=1000)
    kb.record(result.trace_id, outcome="ok")
    with pytest.raises(OutcomeConflictError):
        kb.record(result.trace_id, outcome="fail")


def test_approve_archive(kb):
    cid = kb.add("note", kind="note")
    kb.approve(cid)
    chunk = kb.storage.get_chunk(cid)
    assert chunk["state"] == "active"
    kb.archive(cid)
    chunk = kb.storage.get_chunk(cid)
    assert chunk["state"] == "archived"
    kb.restore(cid)
    chunk = kb.storage.get_chunk(cid)
    assert chunk["state"] == "active"


def test_invalidate(kb):
    cid = kb.add("bad advice", kind="note")
    kb.invalidate(cid, reason="wrong")
    chunk = kb.storage.get_chunk(cid)
    assert chunk["state"] == "archived"
    assert chunk["confidence"] == 0.0
    h = chunk["content_hash"]
    assert kb.storage.is_invalidated(h)


def test_evolve_curate(kb):
    # 先产生一些 trace
    r = kb.recall("q", budget=1000)
    kb.record(r.trace_id, outcome="ok", output_summary="summary")
    # evolve
    result = kb.evolve(trigger="manual")
    assert "distilled" in result
    assert "curate" in result


def test_inspect(kb):
    info = kb.inspect()
    assert "chunks" in info
    assert "knowledge_debt_ratio" in info


def test_promote_spark(kb):
    sid = kb.spark("灵感")
    cid = kb.promote_spark(sid, to="note")
    new = kb.storage.get_chunk(cid)
    assert new["origin"] == "captured"
    spark = kb.storage.get_chunk(sid)
    assert spark["maturity"] == "promoted"


def test_drop_spark(kb):
    sid = kb.spark("灵感")
    kb.drop_spark(sid, reason="不可行")
    spark = kb.storage.get_chunk(sid)
    assert spark["maturity"] == "dropped"
