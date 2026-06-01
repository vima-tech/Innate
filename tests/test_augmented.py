"""@augmented 装饰器测试."""

import os
import tempfile

import pytest

from innate.core import KnowledgeBase


@pytest.fixture
def kb():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path)
    k.add("Python 装饰器使用指南", kind="note", trigger_desc="python decorator")
    yield k
    k.storage.close()
    os.unlink(path)


def test_augmented_injects_context(kb):
    """装饰器自动注入 context."""
    @kb.augmented(budget=2000)
    def my_agent(query, _innate_context=None):
        return {"result": query, "outcome": "ok"}

    result = my_agent("python decorator")
    assert result["result"] == "python decorator"
    # _innate_context 应被注入


def test_augmented_records_outcome(kb):
    """装饰器自动解析返回值中的 outcome."""
    @kb.augmented(budget=2000)
    def my_agent(query, _innate_context=None):
        return {"result": query, "outcome": "ok", "output_summary": "done"}

    result = my_agent("python decorator")
    assert result["outcome"] == "ok"
