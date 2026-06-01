"""跨库召回测试."""

import os
import tempfile

import pytest

from innate.core import KnowledgeBase


@pytest.fixture
def shared_kb():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    k = KnowledgeBase(path)
    k.add("共享库知识: SQL 优化技巧", kind="note", trigger_desc="sql optimization")
    yield k, path
    k.storage.close()


def test_cross_lib_recall(shared_kb):
    """主库召回时包含共享库内容."""
    shared, shared_path = shared_kb
    shared.storage.conn.commit()
    shared.storage.close()

    fd, main_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    main = KnowledgeBase(main_path, shared=[shared_path])
    main.add("主库知识: Python 装饰器", kind="note", trigger_desc="python decorator")
    main.storage.conn.commit()

    # 召回应包含共享库内容
    result = main.recall("sql optimization", budget=2000, libs=[main_path, shared_path], trace=False)
    contents = [c["content"] for c in result.knowledge]
    assert any("SQL" in c for c in contents)

    main.storage.close()
    os.unlink(main_path)
    os.unlink(shared_path)
