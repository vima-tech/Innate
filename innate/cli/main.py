"""CLI Adapter — SDK Public API 的薄封装."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import click

from innate.core import KnowledgeBase
from innate.core.exceptions import InnateError


DEFAULT_DB = os.environ.get("INNATE_DB", str(Path.home() / ".innate" / "personal.db"))


def get_kb(db: str | None = None) -> KnowledgeBase:
    path = db or DEFAULT_DB
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    return KnowledgeBase(path)


@click.group()
@click.option("--db", default=None, help="知识库路径")
@click.pass_context
def cli(ctx: click.Context, db: str | None):
    ctx.ensure_object(dict)
    ctx.obj["db"] = db


# ------------------------------------------------------------------
# recall
# ------------------------------------------------------------------
@cli.command()
@click.argument("query")
@click.option("--budget", default=6000, help="token 预算")
@click.option("--top", default=None, type=int, help="最多返回 N 个块")
@click.option("--include-sparks", is_flag=True, help="包含相关灵感")
@click.option("--format", "fmt", default="text", type=click.Choice(["text", "json", "prompt"]))
@click.option(
    "--expand-deps",
    default="false",
    type=click.Choice(["false", "direct", "closure"], case_sensitive=False),
    help="依赖展开: false | direct | closure",
)
@click.pass_context
def recall(ctx, query, budget, top, include_sparks, fmt, expand_deps):
    """召回知识."""
    kb = get_kb(ctx.obj.get("db"))
    expand = expand_deps
    if expand_deps.lower() == "false":
        expand = False
    elif expand_deps.lower() == "direct":
        expand = "direct"
    elif expand_deps.lower() == "closure":
        expand = "closure"

    result = kb.recall(
        query,
        budget=budget,
        trace=True,
        expand_deps=expand,
        allow_trim=False,
        include_sparks=include_sparks,
        top=top,
        source="cli",
    )

    if fmt == "json":
        chunks = [
            {"id": c["id"], "content": c["content"], "confidence": c.get("confidence")}
            for c in result.knowledge
        ]
        out = {
            "trace_id": result.trace_id,
            "empty": result.empty,
            "selected": [c["id"] for c in result.knowledge],
            "chunks": chunks,
            "knowledge": chunks,
            "sparks": [
                {"id": c["id"], "content": c["content"]} for c in result.sparks
            ],
        }
        click.echo(json.dumps(out, ensure_ascii=False, indent=2))
    elif fmt == "prompt":
        lines = ["# 召回知识", ""]
        for c in result.knowledge:
            lines.append(f"## {c['id']} (conf={c.get('confidence',0.5):.2f})")
            lines.append(c["content"])
            lines.append("")
        if result.sparks:
            lines.append("# 相关灵感 💡")
            for c in result.sparks:
                lines.append(f"- {c['content']}")
        lines.append(f"\n<!-- innate_trace_id: {result.trace_id} -->")
        lines.append(f"<!-- innate_selected: {','.join([c['id'] for c in result.knowledge])} -->")
        click.echo("\n".join(lines))
    else:
        click.echo("📚 召回结果")
        for c in result.knowledge:
            click.echo(f"  [{c['id']}] conf={c.get('confidence',0.5):.2f} | {c['content'][:120]}...")
        if result.sparks:
            click.echo("  💡 灵感:")
            for c in result.sparks:
                click.echo(f"    - {c['content'][:80]}...")


# ------------------------------------------------------------------
# record
# ------------------------------------------------------------------
@cli.command()
@click.argument("trace_id")
@click.option("--query", default=None, help="无 recall 预写行时的任务描述")
@click.option("--outcome", default=None, type=click.Choice(["ok", "fail", "unknown"]))
@click.option("--output-summary", default=None)
@click.option("--used", default=None, help="逗号分隔 chunk_id")
@click.option("--feedback", default=None, type=click.Choice(["up", "down"]))
@click.option("--nomination", default=None)
@click.option("--priority", default=0, type=int, help="蒸馏队列优先级")
@click.option("--source", default="cli", type=click.Choice(["cli", "hook", "daemon", "augmented"]))
@click.pass_context
def record(ctx, trace_id, query, outcome, output_summary, used, feedback, nomination, priority, source):
    """补全 trace."""
    kb = get_kb(ctx.obj.get("db"))
    used_list = used.split(",") if used else None
    try:
        kb.record(
            trace_id,
            query=query,
            outcome=outcome,
            output_summary=output_summary,
            used=used_list,
            feedback=feedback,
            nomination=nomination,
            priority=priority,
            source=source,
        )
        click.echo("✅ recorded")
    except InnateError as exc:
        click.echo(f"❌ {exc}", err=True)
        sys.exit(1)


# ------------------------------------------------------------------
# evolve
# ------------------------------------------------------------------
@cli.command()
@click.option("--trigger", default="manual", type=click.Choice(["manual", "scheduled", "threshold"]))
@click.option("--rebuild-embeddings", is_flag=True)
@click.pass_context
def evolve(ctx, trigger, rebuild_embeddings):
    """触发成长."""
    kb = get_kb(ctx.obj.get("db"))
    if rebuild_embeddings:
        n = kb.rebuild_embeddings()
        click.echo(f"🔧 rebuilt {n} embeddings")
        return
    result = kb.evolve(trigger=trigger)
    click.echo(f"🌱 distilled: {result['distilled']}")
    report = result.get("curate")
    if report:
        click.echo(f"🧹 archived={len(report.archived)} decayed={len(report.decayed)} deduped={len(report.deduped)}")


# ------------------------------------------------------------------
# governance
# ------------------------------------------------------------------
@cli.command()
@click.argument("chunk_id")
@click.pass_context
def approve(ctx, chunk_id):
    kb = get_kb(ctx.obj.get("db"))
    kb.approve(chunk_id)
    click.echo("✅ approved")


@cli.command()
@click.argument("chunk_id")
@click.option("--reason", default="stale")
@click.pass_context
def archive(ctx, chunk_id, reason):
    kb = get_kb(ctx.obj.get("db"))
    kb.archive(chunk_id, reason)
    click.echo("✅ archived")


@cli.command()
@click.argument("chunk_id")
@click.option("--reason", default="")
@click.pass_context
def invalidate(ctx, chunk_id, reason):
    kb = get_kb(ctx.obj.get("db"))
    kb.invalidate(chunk_id, reason)
    click.echo("✅ invalidated")


@cli.command()
@click.argument("chunk_id")
@click.pass_context
def restore(ctx, chunk_id):
    kb = get_kb(ctx.obj.get("db"))
    kb.restore(chunk_id)
    click.echo("✅ restored")


# ------------------------------------------------------------------
# add / spark
# ------------------------------------------------------------------
@cli.command()
@click.argument("content")
@click.option("--kind", default="note", type=click.Choice(["note", "skill"]))
@click.option("--trigger", default=None)
@click.option("--anti-trigger", default=None)
@click.option("--skill-name", default=None)
@click.option("--source", default="manual", type=click.Choice(["chat", "manual", "doc", "agent"]))
@click.pass_context
def add(ctx, content, kind, trigger, anti_trigger, skill_name, source):
    kb = get_kb(ctx.obj.get("db"))
    cid = kb.add(
        content,
        kind=kind,
        trigger_desc=trigger,
        anti_trigger_desc=anti_trigger,
        source=source,
        skill_name=skill_name,
    )
    click.echo(f"✅ added {cid}" if cid else "⚠️ discarded by sanitize")


@cli.command()
@click.argument("content")
@click.option("--trigger", default=None)
@click.option("--anti-trigger", default=None)
@click.pass_context
def spark(ctx, content, trigger, anti_trigger):
    """记录灵感."""
    kb = get_kb(ctx.obj.get("db"))
    cid = kb.spark(content, trigger_desc=trigger, anti_trigger_desc=anti_trigger)
    click.echo(f"💡 spark {cid}" if cid else "⚠️ discarded by sanitize")


@cli.command("promote-spark")
@click.argument("spark_id")
@click.option("--to", default="note", type=click.Choice(["note", "skill"]))
@click.pass_context
def promote_spark(ctx, spark_id, to):
    kb = get_kb(ctx.obj.get("db"))
    cid = kb.promote_spark(spark_id, to=to)
    click.echo(f"✅ promoted to {cid}")


@cli.command("drop-spark")
@click.argument("spark_id")
@click.option("--reason", default="")
@click.pass_context
def drop_spark(ctx, spark_id, reason):
    kb = get_kb(ctx.obj.get("db"))
    kb.drop_spark(spark_id, reason=reason)
    click.echo("🗑️ dropped")


@cli.command("mature-spark")
@click.argument("spark_id")
@click.option("--to", required=True, type=click.Choice(["sprouting", "incubating"]))
@click.pass_context
def mature_spark(ctx, spark_id, to):
    kb = get_kb(ctx.obj.get("db"))
    kb.mature_spark(spark_id, to=to)
    click.echo(f"✅ spark maturity={to}")


# ------------------------------------------------------------------
# inspect
# ------------------------------------------------------------------
@cli.command()
@click.argument("target", required=False, default=None)
@click.pass_context
def inspect(ctx, target):
    """库体检 / 详情查看."""
    kb = get_kb(ctx.obj.get("db"))
    if target:
        _inspect_detail(kb, target)
    else:
        _inspect_library(kb)


def _inspect_detail(kb, target: str) -> None:
    """chunk_id / trace_id 详情 — 末尾附相关操作提示(§五)."""
    if kb.storage.get_chunk(target):
        info = kb.inspect(chunk_id=target)
    elif kb.storage.get_log_by_trace(target):
        info = kb.inspect(trace_id=target)
    else:
        info = kb.inspect(chunk_id=target)
    click.echo(json.dumps(info, ensure_ascii=False, indent=2, default=str))

    # 末尾建议操作(§五 'innate inspect <chunk_id>/<trace_id> 末尾同样附相关操作')
    if "chunk" in info:
        cid = info["chunk"]["id"]
        state = info["chunk"]["state"]
        origin = info["chunk"]["origin"]
        maturity = info["chunk"].get("maturity")
        click.echo("")
        click.echo("── 相关操作 ──")
        if origin == "spark":
            if state == "archived":
                click.echo(f"  → innate restore {cid}       # 恢复被作废的 spark")
            elif maturity not in ("promoted", "dropped"):
                if maturity == "seed":
                    click.echo(f"  → innate mature-spark {cid} --to sprouting")
                if maturity in ("seed", "sprouting"):
                    click.echo(f"  → innate mature-spark {cid} --to incubating")
                click.echo(f"  → innate promote-spark {cid} --to note|skill")
                click.echo(f"  → innate drop-spark {cid} --reason '...'")
                click.echo(f"  → innate invalidate {cid} --reason '...' # 立即作废")
            return
        if state == "pending":
            click.echo(f"  → innate approve {cid}      # 批准晋升为 active")
        if state == "active":
            click.echo(f"  → innate archive {cid} --reason '...'    # 归档降权")
            click.echo(f"  → innate invalidate {cid} --reason '...' # 立即作废(写黑名单)")
        if state == "archived":
            click.echo(f"  → innate restore {cid}       # 恢复为 active")
        if info.get("related"):
            click.echo(f"  → 共 {len(info['related'])} 个衍生 chunk (parent_id={cid})")
    elif "log" in info and info.get("log"):
        tid = target
        click.echo("")
        click.echo("── 相关操作 ──")
        click.echo(f"  → innate record {tid} --outcome ok|fail --output-summary '...'    # 补全 trace")
        click.echo(f"  → innate evolve --trigger manual   # 触发蒸馏")


def _inspect_library(kb) -> None:
    """库体检 — 5 个健康信号(§五)+ 建议命令(§五 'inspect 输出格式与建议命令')."""
    info = kb.inspect()
    active = info["chunks"]["active"]
    pending = info["chunks"]["pending"]
    archived = info["chunks"]["archived"]
    zombie = info["chunks"].get("zombie", 0)
    click.echo("─────────────────────────────────────────────")
    click.echo(f"📚 知识库: {info['db']}  (chunks: {active} active / {pending} pending / {archived} archived)")
    click.echo("─────────────────────────────────────────────")

    # 1. 知识债务比
    debt = info["knowledge_debt_ratio"]
    icon = "✅" if debt < 0.3 else ("⚠️ " if debt < 0.6 else "🔴")
    click.echo(f"{icon} 知识债务比         {debt}  (含 {zombie} 僵尸块, < 0.3 正常)")

    # 2. embed 重建状态
    peb = info["pending_embed_rebuild"]
    if peb:
        click.echo(f"⚠️  embed 重建队列    {peb} chunks 待补向量")
        click.echo(f"   → 建议执行: innate evolve --rebuild-embeddings")

    # 3. 灵感提示
    sparks = info["spark_hints"]
    if sparks:
        click.echo(f"💡 灵感提示           {len(sparks)} 个 spark 反复浮现")
        if len(sparks) == 1:
            click.echo(f"   → 建议执行: innate inspect {sparks[0]['id']}  查看详情")
        else:
            click.echo(f"   → 建议执行: innate inspect <spark_id>  查看详情")

    # 4. stale screening
    stale = info["stale_screening_count"]
    if stale:
        timeout = info["curate_params"]["screening_timeout_minutes"]
        click.echo(f"🔴 stale screening    {stale} 条日志卡死 (> {timeout}min)")
        click.echo(f"   → 建议执行: innate evolve --trigger manual  触发 Curate 清理")

    # 5. distill 成本
    tok = info["distill_tokens"]
    total_tok = tok["prompt"] + tok["completion"]
    max_tok = tok.get("max", 0)
    if max_tok:
        click.echo(f"💰 本周期蒸馏成本     ~{total_tok} tokens (上限 {max_tok:,})")
    else:
        click.echo(f"💰 本周期蒸馏成本     ~{total_tok} tokens")

    # §二·五A "Recall 可配置参数": 库体检时打印当前值(帮助调参)
    if "recall_params" in info:
        rp = info["recall_params"]
        click.echo("─────────────────────────────────────────────")
        click.echo("⚙️  Recall 参数")
        click.echo(f"  w_c={rp['w_content']} w_t={rp['w_trigger']} w_f={rp['w_confidence']}"
                   f"  top_k={rp['top_k_candidates']}")
        click.echo(f"  anti_trigger_penalty={rp['anti_trigger_penalty']}  density_refill={rp['density_refill']}")
    if "curate_params" in info:
        cp = info["curate_params"]
        click.echo("⚙️  Curate 参数")
        click.echo(f"  low_conf<{cp['low_conf_threshold']} idle>{cp['low_conf_idle_days']}d"
                   f"  repeat_select>={cp['repeat_select_min']} conf<{cp['repeat_select_conf_max']}")
        click.echo(f"  never_used>{cp['never_used_age_days']}d  open_ttl={cp['open_ttl_days']}d"
                   f"  screening_timeout={cp['screening_timeout_minutes']}min")
        click.echo(f"  promote: used_success>={cp['promote_used_success_min']}"
                   f"  conf>={cp['promote_confidence_min']}")
    click.echo("─────────────────────────────────────────────")


# ------------------------------------------------------------------
# daemon
# ------------------------------------------------------------------
@cli.group()
def daemon():
    """Daemon 控制."""
    pass


@daemon.command()
@click.option("--watch", multiple=True, help="监听日志目录,可重复传入")
@click.option("--db", "db_path", default=None)
@click.option("--pid-file", default="/tmp/innate-daemon.pid")
@click.option("--log-file", default=None)
@click.option("--state-db", default=None)
@click.pass_context
def start(ctx, watch, db_path, pid_file, log_file, state_db):
    from innate.daemon.server import DaemonServer
    srv = DaemonServer(
        db_path=db_path or DEFAULT_DB,
        watch_dirs=list(watch),
        pid_file=pid_file,
        log_file=log_file,
        state_db=state_db,
    )
    srv.start()
    click.echo("🚀 Daemon started")


@daemon.command()
@click.option("--pid-file", default="/tmp/innate-daemon.pid")
def stop(pid_file):
    from innate.daemon.server import DaemonServer
    DaemonServer.stop(pid_file)
    click.echo("🛑 Daemon stopped")


@daemon.command()
@click.option("--pid-file", default="/tmp/innate-daemon.pid")
@click.option("--state-db", default=None)
def status(pid_file, state_db):
    from innate.daemon.server import DaemonServer
    st = DaemonServer.status(pid_file, state_db=state_db)
    click.echo(st)


def main():
    try:
        cli()
    except InnateError as exc:
        # §九 CLI 失败处理约定: 退出码非 0 时写 stderr; 调用方读 stderr 尝试修正一次.
        click.echo(f"❌ {exc}", err=True)
        sys.exit(1)


if __name__ == "__main__":
    main()
