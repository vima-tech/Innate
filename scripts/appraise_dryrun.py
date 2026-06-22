#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""appraise_dryrun.py — 方案 A(四道弃权门)的离线影子回放评估器。

目的
====
在**不破坏任何契约、不动一行 appraise 代码**的前提下,回答一个问题:

    «如果上了方案 A 的四道弃权门,被弃权掉的那些 verdict,到底是不是
      "本该弃权"(表态本会错)的那些?»

它复用已经落库的三类数据,把弃权门"只算不拦",再和后续实际结果对照:

  1. episodic_log.recall_snapshot —— appraise 写入的 {valence,tier,strength,flagged}
  2. usage_trace(refine_mode='appraise')—— 每个贡献 chunk 的 fused 分(similarity 列)
  3. episodic_log.outcome —— record 回填的 'ok' / 'fail'

输出"弃权率 + 弃权精度 + A 上线后残余误报"的对照表,让你**看着数据**
再决定 A 的最终形态,而不是凭感觉相信"弃权变好了"。

四道门的可重构性(诚实声明)
============================
  门1 共振地板    : 可重构(代理)。文档要 raw top_similarity,落库只有 fused;
                    用 strength(= max fused)作代理,偏严。
  门2 双通道一致  : **不可重构**。当前没有 rich/signature 双粒度通道,
                    落库无 signature。本脚本对门2恒不弃权,并在报告中标 N/A。
  门3 证据充分    : 可重构。对每个贡献 chunk 数它在 appraise 之前已有的
                    task_ok/task_fail 观测数,够数的贡献者 < min_evidence 即弃权。
  门4 邻居离散    : 可重构(代理)。用贡献者 fused 分的离散度(max-min)作
                    "邻居分歧"的代理,> conflict_ceiling 即弃权。

门按短路顺序求值(门1→门3→门4),记录**第一道**触发弃权的门。

用法
====
  # 跑真实历史(默认 DB)
  python3 scripts/appraise_dryrun.py

  # 指定 DB / 阈值 / 时间窗
  python3 scripts/appraise_dryrun.py --db ~/.innate/data/personal.db \\
      --resonance-floor 0.40 --min-evidence 2 --conflict-ceiling 0.25 --window-days 90

  # 没有真实数据时,用合成标注集验证指标定义与报告格式
  python3 scripts/appraise_dryrun.py --demo

  # 机器可读
  python3 scripts/appraise_dryrun.py --json
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import statistics
import sys
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Optional

DEFAULT_DB = os.path.expanduser("~/.innate/data/personal.db")

# 门阈值默认值对齐 meta(appraise.min_strength=0.40);门3/门4 文档未给硬默认,取保守值。
DEFAULT_RESONANCE_FLOOR = 0.40   # 门1:strength(max fused)地板
DEFAULT_MIN_EVIDENCE = 2         # 门3:有先验观测结果的贡献者下限
DEFAULT_CONFLICT_CEILING = 0.25  # 门4:贡献者 fused 离散度(max-min)上限


# ---------------------------------------------------------------------------
# 数据载入
# ---------------------------------------------------------------------------

@dataclass
class Appraisal:
    """一条 appraise 记录 + 重构弃权门所需的全部信号。"""
    trace_id: str
    ts: str
    context_key: Optional[str]
    valence: str                       # affirm | caution | mixed | neutral
    tier: str                          # weak | medium | strong
    strength: float                    # max fused(门1 代理)
    contributors: list[tuple[str, float]] = field(default_factory=list)  # (chunk_id, fused)
    observed_neighbor_count: int = 0   # 门3:有 ≥1 先验观测的贡献者数
    outcome: Optional[str] = None      # ok | fail | None

    @property
    def has_outcome(self) -> bool:
        return self.outcome in ("ok", "fail")

    @property
    def dispersion(self) -> float:
        """门4 代理:贡献者 fused 分的极差。单贡献者=0(无分歧)。"""
        fused = [f for _, f in self.contributors]
        return (max(fused) - min(fused)) if len(fused) >= 2 else 0.0

    @property
    def is_directional(self) -> bool:
        """今天实际给出方向性表态的(非 neutral)。neutral 今天本就沉默。"""
        return self.valence != "neutral"

    @property
    def verdict_wrong(self) -> Optional[bool]:
        """今天的方向性 verdict 是否与实际结果矛盾。无结果或中立则 None。"""
        if not self.has_outcome or not self.is_directional:
            return None
        # caution 警告了却 ok → 误报;affirm/mixed 给了底气却 fail → 误信。
        if self.valence == "caution":
            return self.outcome == "ok"
        return self.outcome == "fail"  # affirm | mixed


def load_from_db(db_path: str, window_days: int) -> list[Appraisal]:
    if not os.path.exists(db_path):
        sys.exit(f"DB 不存在: {db_path}")
    cutoff = (datetime.now(timezone.utc) - timedelta(days=window_days)).strftime(
        "%Y-%m-%dT%H:%M:%S.000Z"
    )
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row

    rows = conn.execute(
        """SELECT trace_id, ts, context_key, recall_snapshot, outcome
             FROM episodic_log
            WHERE ts >= ? AND recall_snapshot LIKE '%"appraise"%'
            ORDER BY ts""",
        (cutoff,),
    ).fetchall()

    out: list[Appraisal] = []
    for r in rows:
        try:
            snap = json.loads(r["recall_snapshot"] or "{}")
        except json.JSONDecodeError:
            continue
        ap = snap.get("appraise")
        if not ap:
            continue

        # 贡献者 fused:取该 trace 下 refine_mode='appraise' 的 retrieved 行,similarity=fused。
        crows = conn.execute(
            """SELECT chunk_id, similarity FROM usage_trace
                WHERE trace_id = ? AND refine_mode = 'appraise'
                  AND event = 'retrieved' AND chunk_id IS NOT NULL""",
            (r["trace_id"],),
        ).fetchall()
        contributors = [
            (c["chunk_id"], float(c["similarity"]))
            for c in crows
            if c["similarity"] is not None
        ]

        # 门3:每个贡献者在本次 appraise *之前* 已有的观测结果数(task_ok/task_fail)。
        observed = 0
        for cid, _ in contributors:
            n = conn.execute(
                """SELECT COUNT(*) FROM usage_trace
                    WHERE chunk_id = ? AND event IN ('task_ok','task_fail')
                      AND ts < ?""",
                (cid, r["ts"]),
            ).fetchone()[0]
            if n >= 1:
                observed += 1

        out.append(
            Appraisal(
                trace_id=r["trace_id"],
                ts=r["ts"],
                context_key=r["context_key"],
                valence=ap.get("valence", "neutral"),
                tier=ap.get("tier", "weak"),
                strength=float(ap.get("strength", 0.0)),
                contributors=contributors,
                observed_neighbor_count=observed,
                outcome=r["outcome"],
            )
        )
    conn.close()
    return out


# ---------------------------------------------------------------------------
# 弃权门(只算不拦,短路求值)
# ---------------------------------------------------------------------------

GATES = ["gate1_weak_resonance", "gate3_sparse_evidence", "gate4_conflicted"]
GATE2 = "gate2_false_resonance"  # 不可重构,恒不弃权


def first_abstaining_gate(a: Appraisal, cfg: "Config") -> Optional[str]:
    """返回第一道触发弃权的门名;全过返回 None(= A 仍会表态)。"""
    # 门1 共振地板
    if a.strength < cfg.resonance_floor:
        return "gate1_weak_resonance"
    # 门2 双通道一致 —— N/A,跳过(见文件头诚实声明)
    # 门3 证据充分
    if a.observed_neighbor_count < cfg.min_evidence:
        return "gate3_sparse_evidence"
    # 门4 邻居离散
    if a.dispersion > cfg.conflict_ceiling:
        return "gate4_conflicted"
    return None


@dataclass
class Config:
    resonance_floor: float
    min_evidence: int
    conflict_ceiling: float


# ---------------------------------------------------------------------------
# 评估
# ---------------------------------------------------------------------------

def evaluate(appraisals: list[Appraisal], cfg: Config) -> dict:
    total = len(appraisals)
    per_gate = {g: {"abstained": 0, "with_outcome": 0, "would_be_wrong": 0} for g in GATES}

    abstained = 0
    # A 上线后,被弃权样本里今天表态的命运拆分:
    tp_abstain = 0   # 弃掉的是"本会错"的方向性 verdict(好:挡住误报)
    fp_abstain = 0   # 弃掉的是"本会对"的方向性 verdict(坏:误伤好提醒)
    # 留存(A 不弃权,仍表态)的方向性 verdict 命运:
    kept_directional_wrong = 0
    kept_directional_right = 0
    # 基线(今天,无 A):方向性 verdict 的对错(仅有结果者)
    base_directional_wrong = 0
    base_directional_right = 0

    for a in appraisals:
        gate = first_abstaining_gate(a, cfg)

        if a.is_directional and a.has_outcome:
            if a.verdict_wrong:
                base_directional_wrong += 1
            else:
                base_directional_right += 1

        if gate is not None:
            abstained += 1
            per_gate[gate]["abstained"] += 1
            if a.has_outcome:
                per_gate[gate]["with_outcome"] += 1
                # 弃权精度只对"今天本会方向性表态"的样本有意义。
                if a.is_directional:
                    if a.verdict_wrong:
                        per_gate[gate]["would_be_wrong"] += 1
                        tp_abstain += 1
                    else:
                        fp_abstain += 1
        else:
            if a.is_directional and a.has_outcome:
                if a.verdict_wrong:
                    kept_directional_wrong += 1
                else:
                    kept_directional_right += 1

    def rate(n, d):
        return round(n / d, 4) if d else None

    abstain_precision = rate(tp_abstain, tp_abstain + fp_abstain)
    base_total = base_directional_wrong + base_directional_right
    kept_total = kept_directional_wrong + kept_directional_right

    return {
        "totals": {
            "appraisals": total,
            "with_outcome": sum(1 for a in appraisals if a.has_outcome),
            "directional": sum(1 for a in appraisals if a.is_directional),
            "neutral_today": sum(1 for a in appraisals if not a.is_directional),
        },
        "abstain": {
            "abstained": abstained,
            "abstain_rate": rate(abstained, total),
            "abstain_precision": abstain_precision,  # 挡住的里有多少本会错 → 越高越好
            "prevented_wrong_signals": tp_abstain,    # 好处:挡掉的误报/误信
            "suppressed_good_signals": fp_abstain,    # 代价:误伤的好提醒
        },
        "per_gate": per_gate,
        "false_alarm_rate": {
            # 核心对照:A 前后,方向性 verdict 的误报率
            "baseline_directional_wrong": base_directional_wrong,
            "baseline_directional_total": base_total,
            "baseline_wrong_rate": rate(base_directional_wrong, base_total),
            "after_A_directional_wrong": kept_directional_wrong,
            "after_A_directional_total": kept_total,
            "after_A_wrong_rate": rate(kept_directional_wrong, kept_total),
        },
        "gate2_status": "N/A — 双通道 signature 不可从落库数据重构,本回放对门2恒不弃权",
    }


# ---------------------------------------------------------------------------
# 报告
# ---------------------------------------------------------------------------

def render(report: dict, cfg: Config) -> str:
    t = report["totals"]
    ab = report["abstain"]
    fa = report["false_alarm_rate"]
    L = []
    L.append("=" * 68)
    L.append("方案 A 弃权门 — 离线影子回放(dry-run)")
    L.append("=" * 68)
    L.append(f"阈值: 门1<{cfg.resonance_floor}  门3<{cfg.min_evidence}观测  门4离散>{cfg.conflict_ceiling}")
    L.append("")
    L.append(f"appraise 总数      : {t['appraisals']}")
    L.append(f"  其中有结果回填   : {t['with_outcome']}")
    L.append(f"  今天方向性表态   : {t['directional']}    今天已沉默(neutral): {t['neutral_today']}")
    L.append("")
    if t["appraisals"] == 0:
        L.append("⚠ 没有 appraise 历史数据 —— 先让 appraise 实际跑起来积累 trace,")
        L.append("  或用 --demo 查看指标定义与报告格式。")
        L.append("=" * 68)
        return "\n".join(L)

    L.append("-- 弃权概览 ------------------------------------------------------")
    L.append(f"弃权数 / 总数      : {ab['abstained']} / {t['appraisals']}   (弃权率 {ab['abstain_rate']})")
    L.append(f"弃权精度           : {ab['abstain_precision']}   (挡住的里本会错的占比,越高越好)")
    L.append(f"  ✓ 挡掉的误报/误信 : {ab['prevented_wrong_signals']}")
    L.append(f"  ✗ 误伤的好提醒    : {ab['suppressed_good_signals']}")
    L.append("")
    L.append("-- 逐门拆分(第一道触发的门)------------------------------------")
    L.append(f"{'门':<26}{'弃权':>6}{'有结果':>8}{'本会错':>8}{'门精度':>10}")
    for g in GATES:
        d = report["per_gate"][g]
        prec = round(d["would_be_wrong"] / d["with_outcome"], 3) if d["with_outcome"] else "-"
        L.append(f"{g:<26}{d['abstained']:>6}{d['with_outcome']:>8}{d['would_be_wrong']:>8}{str(prec):>10}")
    L.append(f"{GATE2:<26}{'N/A':>6}   (signature 通道未落库,本回放跳过)")
    L.append("")
    L.append("-- 误报率对照(A 的净效果)--------------------------------------")
    L.append(f"基线(今天,无 A)  方向性误报: {fa['baseline_directional_wrong']}/{fa['baseline_directional_total']}  = {fa['baseline_wrong_rate']}")
    L.append(f"A 上线后(留存表态)方向性误报: {fa['after_A_directional_wrong']}/{fa['after_A_directional_total']}  = {fa['after_A_wrong_rate']}")
    L.append("")
    L.append("判读: after_A_wrong_rate 应低于 baseline(误报被门挡掉);")
    L.append("      同时 suppressed_good_signals 要小(否则门误伤,调高门阈值)。")
    L.append("=" * 68)
    return "\n".join(L)


# ---------------------------------------------------------------------------
# 合成标注集(--demo):验证指标定义,不依赖引擎与真实数据
# ---------------------------------------------------------------------------

def demo_dataset() -> list[Appraisal]:
    """12 条手工标注样本,覆盖每道门的命中/误伤与留存对错。"""
    def mk(tid, val, tier, strength, contribs, observed, outcome):
        return Appraisal(
            trace_id=tid, ts="2026-06-01T00:00:00.000Z", context_key="demo",
            valence=val, tier=tier, strength=strength,
            contributors=contribs, observed_neighbor_count=observed, outcome=outcome,
        )
    return [
        # 门1 命中:弱共振 + 今天却 caution 误报(本会错)→ 好弃权
        mk("g1-tp", "caution", "weak", 0.20, [("c", 0.20)], 3, "ok"),
        # 门1 误伤:弱共振但今天 caution 命中(本会对)→ 误伤
        mk("g1-fp", "caution", "weak", 0.30, [("c", 0.30)], 3, "fail"),
        # 门3 命中:证据稀疏 + affirm 误信(本会错)→ 好弃权
        mk("g3-tp", "affirm", "medium", 0.55, [("c", 0.55)], 0, "fail"),
        # 门3 误伤:证据稀疏但 affirm 命中(本会对)→ 误伤
        mk("g3-fp", "affirm", "medium", 0.55, [("c", 0.55)], 1, "ok"),
        # 门4 命中:贡献者严重分歧 + caution 误报 → 好弃权
        mk("g4-tp", "caution", "strong", 0.80, [("a", 0.80), ("b", 0.40)], 3, "ok"),
        # 门4 误伤:分歧大但 caution 命中 → 误伤
        mk("g4-fp", "caution", "strong", 0.78, [("a", 0.78), ("b", 0.45)], 3, "fail"),
        # 留存 + 对:全门通过、affirm 命中 → A 保留的好表态
        mk("keep-right", "affirm", "strong", 0.75, [("a", 0.75), ("b", 0.70)], 4, "ok"),
        # 留存 + 错:全门通过却仍误报 → A 没挡住的残余误报
        mk("keep-wrong", "caution", "strong", 0.72, [("a", 0.72), ("b", 0.68)], 4, "ok"),
        # 今天已 neutral:A 弃权与否无所谓(不计精度)
        mk("neutral-1", "neutral", "weak", 0.15, [("c", 0.15)], 0, None),
        # 有方向但无结果回填:计弃权率,不计精度
        mk("no-outcome", "caution", "medium", 0.50, [("c", 0.50)], 2, None),
        # 留存 + 对(第二条,放大基线)
        mk("keep-right-2", "affirm", "medium", 0.60, [("a", 0.60), ("b", 0.58)], 3, "ok"),
        # 门1 命中(第二条):弱共振 affirm 误信
        mk("g1-tp-2", "affirm", "weak", 0.18, [("c", 0.18)], 3, "fail"),
    ]


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def main() -> None:
    p = argparse.ArgumentParser(description="方案 A 弃权门离线影子回放评估器")
    p.add_argument("--db", default=DEFAULT_DB, help=f"SQLite DB 路径 (默认 {DEFAULT_DB})")
    p.add_argument("--window-days", type=int, default=90, help="只回放最近 N 天的 appraise")
    p.add_argument("--resonance-floor", type=float, default=DEFAULT_RESONANCE_FLOOR, help="门1 strength 地板")
    p.add_argument("--min-evidence", type=int, default=DEFAULT_MIN_EVIDENCE, help="门3 观测结果贡献者下限")
    p.add_argument("--conflict-ceiling", type=float, default=DEFAULT_CONFLICT_CEILING, help="门4 离散度上限")
    p.add_argument("--demo", action="store_true", help="用合成标注集,验证指标与报告格式")
    p.add_argument("--json", action="store_true", help="输出 JSON")
    args = p.parse_args()

    cfg = Config(args.resonance_floor, args.min_evidence, args.conflict_ceiling)
    appraisals = demo_dataset() if args.demo else load_from_db(args.db, args.window_days)
    report = evaluate(appraisals, cfg)

    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        print(render(report, cfg))


if __name__ == "__main__":
    main()
