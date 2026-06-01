"""CLI 端到端测试."""

import os
import subprocess
import tempfile

import pytest


@pytest.fixture
def db_path():
    fd, path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    yield path
    os.unlink(path)


def run(args, env=None):
    cmd = ["python", "-m", "innate.cli.main"] + args
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


def test_cli_add_recall_record(db_path):
    env = {**os.environ, "INNATE_DB": db_path}

    # add
    r = run(["add", "测试内容", "--kind", "note", "--trigger", "测试触发"], env=env)
    assert r.returncode == 0, r.stderr

    # recall
    r = run(["recall", "测试触发", "--format", "json"], env=env)
    assert r.returncode == 0, r.stderr
    import json
    out = json.loads(r.stdout)
    trace_id = out["trace_id"]
    assert trace_id

    # record
    r = run(["record", trace_id, "--outcome", "ok", "--feedback", "up"], env=env)
    assert r.returncode == 0, r.stderr

    # inspect
    r = run(["inspect"], env=env)
    assert r.returncode == 0, r.stderr
    assert "知识库" in r.stdout


def test_cli_spark_promote_drop(db_path):
    env = {**os.environ, "INNATE_DB": db_path}

    r = run(["spark", "一个灵感"], env=env)
    assert r.returncode == 0, r.stderr
    spark_id = r.stdout.strip().split()[-1]

    r = run(["promote-spark", spark_id, "--to", "note"], env=env)
    assert r.returncode == 0, r.stderr

    r = run(["spark", "另一个灵感"], env=env)
    assert r.returncode == 0, r.stderr
    drop_id = r.stdout.strip().split()[-1]

    r = run(["drop-spark", drop_id, "--reason", "不可行"], env=env)
    assert r.returncode == 0, r.stderr


def test_cli_governance(db_path):
    env = {**os.environ, "INNATE_DB": db_path}

    r = run(["add", "bad content", "--kind", "note"], env=env)
    assert r.returncode == 0, r.stderr
    cid = r.stdout.strip().split()[-1]

    r = run(["archive", cid, "--reason", "stale"], env=env)
    assert r.returncode == 0, r.stderr

    r = run(["restore", cid], env=env)
    assert r.returncode == 0, r.stderr

    r = run(["invalidate", cid, "--reason", "wrong"], env=env)
    assert r.returncode == 0, r.stderr
