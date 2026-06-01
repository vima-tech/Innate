"""Core SDK — KnowledgeBase: 8 个 Public API 的完整实现."""

from __future__ import annotations

import json
import math
import re
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Tuple

from .embedding import DummyEmbeddingProvider, EmbeddingProvider
from .exceptions import ChunkNotFoundError, InvalidStateError, OutcomeConflictError
from .refine import Distiller, HeuristicDistiller, NullRefiner, Refiner
from .storage import Storage
from .utils import content_hash, default_sanitize, estimate_tokens, gen_uuid, utc_now_iso


# ---------------------------------------------------------------------------
# Curator 替换协议
# ---------------------------------------------------------------------------
@dataclass
class CurateScope:
    origin: str | None = None
    skill_name: str | None = None
    dry_run: bool = False


@dataclass
class CurateReport:
    archived: List[str] = field(default_factory=list)
    deduped: List[str] = field(default_factory=list)
    decayed: List[str] = field(default_factory=list)
    cycles: List[List[str]] = field(default_factory=list)
    orphans: List[str] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    stats: Dict[str, Any] = field(default_factory=dict)


class Curator:
    """可整体替换的治理器."""

    def run(self, kb: "KnowledgeBase", scope: CurateScope) -> CurateReport:
        return kb._builtin_curate(scope)


# ---------------------------------------------------------------------------
# Recall 结果
# ---------------------------------------------------------------------------
@dataclass
class RecallResult:
    knowledge: List[Dict[str, Any]] = field(default_factory=list)
    sparks: List[Dict[str, Any]] = field(default_factory=list)
    trace_id: str = ""
    empty: bool = True
    depth_skipped: List[str] = field(default_factory=list)
    _trace: Dict[str, Any] = field(default_factory=dict, repr=False)


# ---------------------------------------------------------------------------
# KnowledgeBase
# ---------------------------------------------------------------------------
class KnowledgeBase:
    """Innate 核心 SDK."""

    # 默认 recall 参数
    RECALL_DEFAULTS = {
        "recall.w_content": 0.65,
        "recall.w_trigger": 0.25,
        "recall.w_confidence": 0.10,
        "recall.top_k_candidates": 20,
        "recall.anti_trigger_penalty": 0.6,
        "recall.density_refill": True,
    }

    # 默认 curate 参数
    CURATE_DEFAULTS = {
        "curate.low_conf_threshold": 0.25,
        "curate.low_conf_idle_days": 60,
        "curate.repeat_select_min": 10,
        "curate.repeat_select_conf_max": 0.5,
        "curate.never_used_age_days": 30,
        "curate.open_ttl_days": 7,
        "curate.screening_timeout_minutes": 30,
        "curate.promote_used_success_min": 3,
        "curate.promote_confidence_min": 0.65,
    }

    def __init__(
        self,
        db_path: str,
        shared: List[str] | None = None,
        embedding: EmbeddingProvider | None = None,
        curator: Curator | None = None,
        refiner: Refiner | None = None,
        distiller: Distiller | None = None,
        sanitize: Callable[[str | None], Tuple[str | None, str]] | None = default_sanitize,
    ):
        self.db_path = db_path
        self.shared_paths = shared or []
        self.embedding = embedding or DummyEmbeddingProvider()
        self.sanitize = sanitize
        self._curator = curator or Curator()
        self.refiner = refiner or NullRefiner()
        self.distiller = distiller or HeuristicDistiller()

        self.storage = Storage(
            db_path,
            content_dim=self.embedding.content_dim,
            trigger_dim=self.embedding.trigger_dim,
        )
        self._shared_storages: Dict[str, Storage] = {}
        for sp in self.shared_paths:
            self._shared_storages[sp] = Storage(
                sp,
                content_dim=self.embedding.content_dim,
                trigger_dim=self.embedding.trigger_dim,
            )
        self._init_meta()
        self._validate_embedding_dims(self.storage)
        for st in self._shared_storages.values():
            self._validate_embedding_dims(st)
        self._load_params()

    def close(self) -> None:
        """关闭所有数据库连接."""
        self.storage.close()
        for st in self._shared_storages.values():
            st.close()

    # ------------------------------------------------------------------
    # 初始化
    # ------------------------------------------------------------------
    def _init_meta(self) -> None:
        """补齐首次建库和迁移库缺失的 meta 默认值."""
        defaults = {
            "lib_id": gen_uuid(),
            "lib_role": "personal",
            "schema_version": "4.5.1",
            "content_dim": str(self.embedding.content_dim),
            "trigger_dim": str(self.embedding.trigger_dim),
            "embed_model": self.embedding.__class__.__name__,
            "embed_version": "1",
            "last_agg_ts": "1970-01-01T00:00:00.000Z",
            **{k: str(v) for k, v in self.RECALL_DEFAULTS.items()},
            **{k: str(v) for k, v in self.CURATE_DEFAULTS.items()},
            "evolve.threshold_new_count": "5",
            "evolve.distill_batch_size": "20",
        }
        for key, value in defaults.items():
            if self.storage.get_meta(key) is None:
                self.storage.set_meta(key, value)
        self.storage.conn.commit()

    def _validate_embedding_dims(self, storage: Storage) -> None:
        """向量表维度变更需显式迁移,不能在查询时静默失败."""
        content_dim = int(storage.get_meta("content_dim") or self.embedding.content_dim)
        trigger_dim = int(storage.get_meta("trigger_dim") or self.embedding.trigger_dim)
        if (
            content_dim != self.embedding.content_dim
            or trigger_dim != self.embedding.trigger_dim
        ):
            raise InvalidStateError(
                "embedding dimensions do not match database schema: "
                f"db=({content_dim},{trigger_dim}) "
                f"provider=({self.embedding.content_dim},{self.embedding.trigger_dim})"
            )

    def _load_params(self) -> None:
        """从 meta 加载可配置参数到实例属性."""
        def _f(key: str, default: float) -> float:
            v = self.storage.get_meta(key)
            return float(v) if v is not None else default

        def _i(key: str, default: int) -> int:
            v = self.storage.get_meta(key)
            return int(v) if v is not None else default

        def _b(key: str, default: bool) -> bool:
            v = self.storage.get_meta(key)
            return v.lower() == "true" if v is not None else default

        self.w_content = _f("recall.w_content", 0.65)
        self.w_trigger = _f("recall.w_trigger", 0.25)
        self.w_confidence = _f("recall.w_confidence", 0.10)
        self.top_k_candidates = _i("recall.top_k_candidates", 20)
        self.anti_trigger_penalty = _f("recall.anti_trigger_penalty", 0.6)
        self.density_refill = _b("recall.density_refill", True)

        self.low_conf_threshold = _f("curate.low_conf_threshold", 0.25)
        self.low_conf_idle_days = _i("curate.low_conf_idle_days", 60)
        self.repeat_select_min = _i("curate.repeat_select_min", 10)
        self.repeat_select_conf_max = _f("curate.repeat_select_conf_max", 0.5)
        self.never_used_age_days = _i("curate.never_used_age_days", 30)
        self.open_ttl_days = _i("curate.open_ttl_days", 7)
        self.screening_timeout_minutes = _i("curate.screening_timeout_minutes", 30)
        self.promote_used_success_min = _i("curate.promote_used_success_min", 3)
        self.promote_confidence_min = _f("curate.promote_confidence_min", 0.65)

        self.evolve_threshold = _i("evolve.threshold_new_count", 5)
        self.distill_batch_size = _i("evolve.distill_batch_size", 20)

    # ------------------------------------------------------------------
    # Public API: recall
    # ------------------------------------------------------------------
    def recall(
        self,
        query: str,
        budget: int = 6000,
        libs: List[str] | None = None,
        trace: bool = True,
        expand_deps: bool | str = False,
        allow_trim: bool = False,
        include_sparks: bool = False,
        top: int | None = None,
        source: str = "sdk",
    ) -> RecallResult:
        """同步召回. 纯数学,零模型调用(allow_trim 除外)."""
        trace_id = gen_uuid()
        now = utc_now_iso()

        # 1. embedding
        try:
            q_content = self.embedding.embed_content(query)
            q_trigger = self.embedding.embed_trigger(query)
        except Exception as exc:
            from .exceptions import EmbeddingUnavailable
            raise EmbeddingUnavailable(f"embedding failed: {exc}") from exc

        # 2. ANN 检索 — knowledge 候选(不含 spark)
        candidates = self._ann_candidates(q_content, q_trigger, libs)

        # 2.5 soft dep 加分(§四 soft 仅作提示, 预算允许时作为普通候选加分)
        self._apply_soft_dep_bonus(candidates)

        # 3. 标量过滤 + anti_trigger 惩罚
        scored = self._score_candidates(candidates, query)

        # 4. 装包
        selected, skipped, depth_skipped = self._pack(scored, budget, expand_deps, allow_trim, query)

        # 5. 价值密度回填
        if self.density_refill:
            selected = self._density_refill(selected, skipped, budget)

        knowledge = selected
        visible_knowledge = self._limit_knowledge(knowledge, top, expand_deps)

        # 6. sparks 独立召回(不占用 knowledge budget)
        sparks: List[Dict[str, Any]] = []
        if include_sparks:
            sparks = self._recall_sparks(q_content, q_trigger, libs)

        # 7. 写 usage_trace + episodic_log
        if trace:
            retrieved = [item for _, item in scored]
            depth_skipped_set = set(depth_skipped)
            for rank, item in enumerate(retrieved, start=1):
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": item["id"],
                    "event": "retrieved",
                    "similarity": item.get("_fused_score"),
                    "rank": rank,
                    "refine_mode": (
                        "skipped:dep_depth_limit"
                        if item["id"] in depth_skipped_set
                        else None
                    ),
                    "source": source,
                    "ts": now,
                })
            # refined trace (trim 发生时)
            refined_ids = {item["id"] for item in selected if item.pop("_refined", False)}
            for cid in refined_ids:
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": cid,
                    "event": "refined",
                    "strength": 0.5,
                    "source": source,
                    "ts": now,
                })
            for rank, item in enumerate(visible_knowledge, start=1):
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": item["id"],
                    "event": "selected",
                    "rank": rank,
                    "source": source,
                    "ts": now,
                })
            # spark 独立记录 retrieved,供 inspect 累计软孵化提示.
            for rank, item in enumerate(sparks, start=1):
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": item["id"],
                    "event": "retrieved",
                    "similarity": item.get("_fused_score"),
                    "rank": rank,
                    "refine_mode": "spark",
                    "source": source,
                    "ts": now,
                })
            # episodic_log 预写
            snapshot = {
                "retrieved": [item["id"] for item in retrieved],
                "selected": [item["id"] for item in visible_knowledge],
                "sparks": [item["id"] for item in sparks],
                "depth_skipped": depth_skipped,
            }
            self.storage.insert_log({
                "id": gen_uuid(),
                "trace_id": trace_id,
                "lib_id": self.storage.get_meta("lib_id") or "",
                "ts": now,
                "query": query,
                "recall_snapshot": json.dumps(snapshot),
                "distill_state": "open",
                "event_source": source,
            })
            self.storage.conn.commit()

        result = RecallResult(
            knowledge=visible_knowledge,
            sparks=sparks,
            trace_id=trace_id,
            depth_skipped=depth_skipped,
            empty=len(knowledge) == 0 and len(sparks) == 0,
        )
        result._trace = {"selected_ids": [i["id"] for i in visible_knowledge]}
        return result

    def _limit_knowledge(
        self,
        knowledge: List[Dict[str, Any]],
        top: int | None,
        expand_deps: bool | str,
    ) -> List[Dict[str, Any]]:
        """限制返回 seed 数量,但绝不截断 hard dependency 闭包."""
        if top is None:
            return knowledge
        if top <= 0:
            return []
        if not expand_deps:
            return knowledge[:top]

        available = {chunk["id"]: chunk for chunk in knowledge}
        visible: List[Dict[str, Any]] = []
        added = set()
        seed_count = 0
        for seed in knowledge:
            if seed["id"] in added:
                continue
            block, depth_exceeded = self._build_block(seed["id"], expand_deps)
            if depth_exceeded:
                continue
            for chunk in block:
                if chunk["id"] in available and chunk["id"] not in added:
                    visible.append(available[chunk["id"]])
                    added.add(chunk["id"])
            seed_count += 1
            if seed_count >= top:
                break
        return visible

    def _ann_candidates(
        self, q_content: List[float], q_trigger: List[float], libs: List[str] | None = None
    ) -> Dict[str, Dict[str, Any]]:
        """双向量 ANN,返回 chunk_id → {chunk, sim_content, sim_trigger}."""
        # 确定要查询的 storages
        storages_to_query: List[Storage] = [self.storage]
        if libs:
            for sp in self.shared_paths:
                if (sp in libs or "shared" in libs) and sp in self._shared_storages:
                    storages_to_query.append(self._shared_storages[sp])

        candidates: Dict[str, Dict[str, Any]] = {}
        for st in storages_to_query:
            meta_ev = int(st.get_meta("embed_version") or "1")
            rows = st.list_chunks(state=None, origin=None)
            valid_chunks = {
                r["id"]: r for r in rows
                if r["embed_version"] >= meta_ev
                and r["state"] != "archived"
                and r.get("origin") != "spark"
            }

            content_res = st.search_vec_content(q_content, self.top_k_candidates * 2)
            trigger_res = st.search_vec_trigger(q_trigger, self.top_k_candidates * 2)

            for cid, dist in content_res:
                if cid not in valid_chunks:
                    continue
                sim = 1.0 - dist
                if cid in candidates:
                    candidates[cid]["sim_content"] = max(candidates[cid]["sim_content"], sim)
                else:
                    candidates[cid] = {
                        "chunk": valid_chunks[cid],
                        "sim_content": sim,
                        "sim_trigger": 0.0,
                    }
            for cid, dist in trigger_res:
                if cid not in valid_chunks:
                    continue
                sim = 1.0 - dist
                if cid in candidates:
                    candidates[cid]["sim_trigger"] = max(candidates[cid]["sim_trigger"], sim)
                else:
                    candidates[cid] = {
                        "chunk": valid_chunks[cid],
                        "sim_content": 0.0,
                        "sim_trigger": sim,
                    }
        return candidates

    def _recall_sparks(
        self, q_content: List[float], q_trigger: List[float], libs: List[str] | None = None
    ) -> List[Dict[str, Any]]:
        """独立召回 sparks(不占用 knowledge budget). 纯向量相似度, 不含 confidence 加权."""
        storages_to_query: List[Storage] = [self.storage]
        if libs:
            for sp in self.shared_paths:
                if (sp in libs or "shared" in libs) and sp in self._shared_storages:
                    storages_to_query.append(self._shared_storages[sp])

        spark_scores: Dict[str, Tuple[float, Dict[str, Any]]] = {}
        for st in storages_to_query:
            meta_ev = int(st.get_meta("embed_version") or "1")
            rows = st.list_chunks(state=None, origin="spark")
            valid = {
                r["id"]: r
                for r in rows
                if r["embed_version"] >= meta_ev
                and r["state"] != "archived"
                and r.get("maturity") not in ("promoted", "dropped")
            }

            content_res = st.search_vec_content(q_content, self.top_k_candidates)
            trigger_res = st.search_vec_trigger(q_trigger, self.top_k_candidates)

            for cid, dist in content_res + trigger_res:
                if cid not in valid:
                    continue
                sim = 1.0 - dist
                chunk = valid[cid]
                previous = spark_scores.get(cid)
                if previous is None or sim > previous[0]:
                    chunk["_fused_score"] = sim
                    spark_scores[cid] = (sim, chunk)

        sparks = list(spark_scores.values())
        sparks.sort(key=lambda x: x[0], reverse=True)
        return [chunk for _, chunk in sparks[: self.top_k_candidates]]

    def _score_candidates(
        self, candidates: Dict[str, Dict[str, Any]], query: str
    ) -> List[Tuple[float, Dict[str, Any]]]:
        """融合分数 + anti_trigger 惩罚."""
        scored = []
        for cid, info in candidates.items():
            chunk = info["chunk"]
            conf = float(chunk.get("confidence") or 0.5)
            fused = (
                self.w_content * info["sim_content"]
                + self.w_trigger * info["sim_trigger"]
                + self.w_confidence * conf
            )
            # anti_trigger 内存匹配
            anti = chunk.get("anti_trigger_desc") or ""
            if anti and self._anti_trigger_hit(query, anti):
                fused *= self.anti_trigger_penalty
            info["chunk"]["_fused_score"] = fused
            scored.append((fused, info["chunk"]))
        scored.sort(key=lambda x: x[0], reverse=True)
        return scored[: self.top_k_candidates]

    def _anti_trigger_hit(self, query: str, anti: str) -> bool:
        """简单内存匹配."""
        q_lower = query.lower()
        for part in anti.lower().split(","):
            part = part.strip()
            if part and part in q_lower:
                return True
        return False

    def _pack(
        self,
        scored: List[Tuple[float, Dict[str, Any]]],
        budget: int,
        expand_deps: bool | str,
        allow_trim: bool,
        query: str,
    ) -> Tuple[List[Dict[str, Any]], List[Tuple[List[Dict[str, Any]], float, int]], List[str]]:
        """first-fit 装包. 返回 (selected, skipped_blocks, depth_skipped_ids).

        depth_skipped_ids: 因 expand_deps='closure' 触达 CTE 深度上限而被丢弃的 seed.
        设计原则(§六 CTE 双保险):'宁可不召回,不给半截 hard 依赖'——丢弃 seed 而非截断闭包.
        """
        selected: List[Dict[str, Any]] = []
        skipped: List[Tuple[List[Dict[str, Any]], float, int]] = []
        depth_skipped: List[str] = []
        used_ids = set()
        used_tokens = 0

        for fused_score, chunk in scored:
            cid = chunk["id"]
            if cid in used_ids:
                continue
            block, depth_exceeded = self._build_block(cid, expand_deps)
            if depth_exceeded:
                depth_skipped.append(cid)
                continue
            if not block:
                continue
            cost = sum((b.get("token_count") or estimate_tokens(b["content"]) or 100) for b in block)

            if used_tokens + cost <= budget:
                for b in block:
                    if b["id"] not in used_ids:
                        b["_fused_score"] = fused_score
                        selected.append(b)
                        used_ids.add(b["id"])
                used_tokens += cost
                continue

            if allow_trim and self.refiner.available:
                # trim: 不破坏 hard 闭包,只裁非关键段落(由 Refiner 实现)
                try:
                    refined = self.refiner.refine(block, query, "trim")
                except Exception:
                    refined = None
                if refined and self._deps_intact(block, refined):
                    refined_cost = sum(
                        (b.get("token_count") or estimate_tokens(b["content"]) or 100)
                        for b in refined
                    )
                    if used_tokens + refined_cost <= budget:
                        for b in refined:
                            if b["id"] not in used_ids:
                                b["_fused_score"] = fused_score
                                b["_refined"] = True
                                selected.append(b)
                                used_ids.add(b["id"])
                        used_tokens += refined_cost
                        continue

            skipped.append((block, fused_score, cost))

        return selected, skipped, depth_skipped

    @staticmethod
    def _deps_intact(original: List[Dict[str, Any]], refined: List[Dict[str, Any]]) -> bool:
        """trim 只能裁块内文本,不能删除 hard 闭包成员."""
        return {b["id"] for b in original} == {b["id"] for b in refined}

    def _get_chunk_any(self, chunk_id: str) -> Dict[str, Any] | None:
        """跨库查找 chunk."""
        c = self.storage.get_chunk(chunk_id)
        if c:
            return c
        for st in self._shared_storages.values():
            c = st.get_chunk(chunk_id)
            if c:
                return c
        return None

    def _get_deps_any(self, src: str, kind: str | None = None) -> List[Dict[str, Any]]:
        """跨库查找依赖."""
        deps = self.storage.get_deps(src, kind)
        if deps:
            return deps
        for st in self._shared_storages.values():
            deps = st.get_deps(src, kind)
            if deps:
                return deps
        return []

    def _build_block(self, seed_id: str, expand_deps: bool | str) -> Tuple[List[Dict[str, Any]], bool]:
        """构建不可分割块. 返回 (block, depth_exceeded).

        depth_exceeded=True 表示 expand_deps='closure' 触达 CTE 深度上限,
        调用方应丢弃 seed 而非将半截 hard 闭包送进 context(§六 CTE 双保险).
        soft 依赖不强制装入,只作为候选加分(§四 装包只强制展开 hard 闭包).
        """
        block: List[Dict[str, Any]] = []
        seed = self._get_chunk_any(seed_id)
        if not seed:
            return block, False
        block.append(seed)

        if not expand_deps:
            return block, False

        if expand_deps == "direct":
            deps = self._get_deps_any(seed_id, kind="hard")
            for d in deps:
                c = self._get_chunk_any(d["dst"])
                if c:
                    block.append(c)
            return block, False

        if expand_deps == "closure":
            visited = {seed_id}
            queue = [(seed_id, 0)]
            depth_limit = 3
            while queue:
                sid, depth = queue.pop(0)
                deps = self._get_deps_any(sid, kind="hard")
                for d in deps:
                    did = d["dst"]
                    if did in visited:
                        continue
                    if depth + 1 > depth_limit:
                        return [], True
                    visited.add(did)
                    c = self._get_chunk_any(did)
                    if c:
                        block.append(c)
                    queue.append((did, depth + 1))
            return block, False

        return block, False

    def _soft_dep_chunks(self, seed_id: str) -> List[Dict[str, Any]]:
        """soft 依赖块列表. 仅作提示, 不强制装入(§四)."""
        deps = self._get_deps_any(seed_id, kind="soft")
        chunks = []
        for d in deps:
            c = self._get_chunk_any(d["dst"])
            if c:
                chunks.append(c)
        return chunks

    def _apply_soft_dep_bonus(self, candidates: Dict[str, Dict[str, Any]]) -> None:
        """为有 soft dep 的 seed 加 5% fused_score 提示(§四 'soft 仅作提示:目标库在 attach 列表且预算允许时作为普通候选加分')."""
        for cid, info in candidates.items():
            chunk = info["chunk"]
            if chunk.get("origin") == "spark":
                continue
            soft = self._get_deps_any(cid, kind="soft")
            if soft:
                info["sim_content"] = min(1.0, info.get("sim_content", 0.0) * 1.05)

    def _density_refill(
        self,
        selected: List[Dict[str, Any]],
        skipped: List[Tuple[List[Dict[str, Any]], float, int]],
        budget: int,
    ) -> List[Dict[str, Any]]:
        """价值密度回填."""
        used_tokens = sum(
            (b.get("token_count") or estimate_tokens(b["content"]) or 100)
            for b in selected
        )
        if used_tokens >= budget:
            return selected

        # 计算密度
        density_items = []
        for block, fscore, cost in skipped:
            # 过滤掉已选中的
            block = [b for b in block if b["id"] not in {x["id"] for x in selected}]
            if not block:
                continue
            cost2 = sum((b.get("token_count") or estimate_tokens(b["content"]) or 100) for b in block)
            density = fscore / max(cost2, 1)
            density_items.append((density, block, cost2))

        density_items.sort(key=lambda x: x[0], reverse=True)
        selected_ids = {b["id"] for b in selected}
        for density, block, cost in density_items:
            if used_tokens + cost <= budget:
                for b in block:
                    if b["id"] not in selected_ids:
                        selected.append(b)
                        selected_ids.add(b["id"])
                used_tokens += cost
        return selected

    # ------------------------------------------------------------------
    # Public API: record
    # ------------------------------------------------------------------
    def record(
        self,
        trace_id: str,
        query: str | None = None,
        output: str | None = None,
        output_summary: str | None = None,
        outcome: str | None = None,
        used: List[str] | None = None,
        feedback: str | Dict[str, Any] | None = None,
        nomination: str | None = None,
        priority: int = 0,
        source: str = "sdk",
    ) -> None:
        """同步极轻:写日志 + EMA 更新 confidence."""
        if outcome not in (None, "ok", "fail", "unknown"):
            raise InvalidStateError(f"invalid outcome: {outcome}")
        now = utc_now_iso()
        self.storage.begin_immediate()
        try:
            log = self.storage.get_log_by_trace(trace_id)
            # 标记是否为无预写行的新建 log(Hook/Daemon 直接 record 场景)
            is_fresh_insert = False
            if log is None:
                # 无预写行:Hook/Daemon 直接 record
                self.storage.insert_log({
                    "id": gen_uuid(),
                    "trace_id": trace_id,
                    "lib_id": self.storage.get_meta("lib_id") or "",
                    "ts": now,
                    "query": query or "",
                    "output": output,
                    "output_summary": output_summary,
                    "outcome": outcome,
                    "event_source": source,
                    "nomination": nomination,
                    "priority": priority,
                    "distill_state": "open",
                })
                log = self.storage.get_log_by_trace(trace_id)
                is_fresh_insert = True

            # outcome 冲突检查
            existing_outcome = log.get("outcome")
            if outcome and existing_outcome and existing_outcome != outcome:
                self.storage.rollback()
                raise OutcomeConflictError(
                    f"trace {trace_id} already has outcome={existing_outcome}, cannot set {outcome}"
                )

            # usage_trace: used
            if used:
                for cid in used:
                    self.storage.append_trace({
                        "trace_id": trace_id,
                        "chunk_id": cid,
                        "event": "used",
                        "strength": 0.3,
                        "source": source,
                        "ts": now,
                    })
                    self.storage.update_chunk_last_used(cid)

            # usage_trace: task_ok / task_fail
            if outcome in ("ok", "fail"):
                event = "task_ok" if outcome == "ok" else "task_fail"
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": None,
                    "event": event,
                    "strength": 0.15 if event == "task_fail" else 1.0,
                    "source": source,
                    "ts": now,
                })

            # outcome 隐式弱更新(§二·五B:used 的块按上表弱更新;selected 未 used 极弱更新)
            # is_fresh_insert: Hook/Daemon 直接 record 场景下,outcome 随 log 一起新建,
            # 此时 existing_outcome == outcome(刚写入),必须用 is_fresh_insert 标志触发更新.
            # 预写行场景: existing_outcome != outcome 保证幂等重复调用不多次更新 confidence.
            if outcome and (is_fresh_insert or existing_outcome != outcome):
                self._apply_outcome_implicit(trace_id, outcome, used, now)

            # feedback → confidence EMA
            self._apply_feedback(trace_id, feedback, used, now)

            # 更新 episodic_log(跳过与现有值相同的字段,减少无意义 UPDATE)
            updates: Dict[str, Any] = {}
            if output is not None and output != log.get("output"):
                updates["output"] = output
            if output_summary is not None and output_summary != log.get("output_summary"):
                updates["output_summary"] = output_summary
            if outcome is not None and outcome != existing_outcome:
                updates["outcome"] = outcome
            if nomination is not None and nomination != log.get("nomination"):
                updates["nomination"] = nomination
            if priority and priority != log.get("priority"):
                updates["priority"] = priority
            if source != log.get("event_source"):
                updates["event_source"] = source

            # open → new / discarded 判断
            # 只对 distill_state='open' 的 log 执行,避免把已是 'new' / 'screening' 等状态的
            # log 降级为 'discarded'(§五 record() 写入逻辑第 6 步:"outcome 补完后执行").
            current_distill_state = log.get("distill_state", "open")
            if current_distill_state == "open":
                # 同时检查传入参数和 log 中已有的值,支持幂等重复调用
                has_material = bool(
                    output_summary or log.get("output_summary")
                    or nomination or log.get("nomination")
                    or (used and outcome and outcome != "unknown")
                    or (used and log.get("outcome") and log.get("outcome") != "unknown")
                    or output or log.get("output")
                )
                if has_material:
                    updates["distill_state"] = "new"
                else:
                    updates["distill_state"] = "discarded"
                    updates["distill_note"] = "insufficient_material"

            if updates:
                self.storage.update_log(trace_id, updates)

            self.storage.commit()
        except Exception:
            self.storage.rollback()
            raise

    def _apply_outcome_implicit(
        self, trace_id: str, outcome: str | None, used: List[str] | None, now: str
    ) -> None:
        """outcome 隐式弱更新 confidence(§二·五B)."""
        if outcome not in ("ok", "fail"):
            return
        used_set = set(used or [])
        # used 的块:弱更新
        if outcome == "ok":
            target, strength, reason = 1.0, 0.3, "agent_used"
        else:
            target, strength, reason = 0.0, 0.15, "task_fail"
        for cid in used_set:
            self._update_confidence(cid, target, strength, reason, now)
        # selected 未 used 的块:极弱更新
        rows = self.storage.conn.execute(
            "SELECT chunk_id FROM usage_trace WHERE trace_id=? AND event='selected' AND chunk_id IS NOT NULL",
            (trace_id,),
        ).fetchall()
        for row in rows:
            cid = row["chunk_id"]
            if cid not in used_set:
                self._update_confidence(cid, 0.3, 0.1, "selected_unused", now)

    def _apply_feedback(
        self,
        trace_id: str,
        feedback: str | Dict[str, Any] | None,
        used: List[str] | None,
        now: str,
    ) -> None:
        """反馈更新 confidence (EMA)."""
        if feedback is None:
            return

        if isinstance(feedback, str):
            # trace 级 feedback
            if feedback not in ("up", "down"):
                return
            if not used:
                return  # 无 used 块,不更新 (设计: "若本次无 used → 只记日志")
            target = 1.0 if feedback == "up" else 0.0
            strength = 1.0
            reason = "user_up" if feedback == "up" else "user_down"
            for cid in used:
                self._update_confidence(cid, target, strength, reason, now)
                # 设计 §二·五B "last_used_at 只在 used/显式正反馈更新":
                # thumbs_up 是显式正反馈, 应刷新 last_used_at (以激活时效加权与归档判定)
                if feedback == "up":
                    self.storage.update_chunk_last_used(cid)
        elif isinstance(feedback, dict):
            # chunk 级 feedback
            for fb, cids in feedback.items():
                if fb not in ("up", "down"):
                    continue
                target = 1.0 if fb == "up" else 0.0
                strength = 1.0
                reason = "user_up" if fb == "up" else "user_down"
                for cid in cids:
                    self._update_confidence(cid, target, strength, reason, now)
                    if fb == "up":
                        self.storage.update_chunk_last_used(cid)

    def _update_confidence(
        self,
        chunk_id: str,
        target: float,
        strength: float,
        reason: str,
        now: str,
    ) -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            return
        if chunk.get("origin") == "spark":
            return  # spark 不更新 confidence

        alpha = 0.2
        # 时效加权(仅显式信号)
        recency_w = 1.0
        last_used = chunk.get("last_used_at")
        if last_used and reason in ("user_up", "user_down", "judge_score"):
            from datetime import datetime
            try:
                t1 = datetime.fromisoformat(last_used.replace("Z", "+00:00"))
                t2 = datetime.fromisoformat(now.replace("Z", "+00:00"))
                gap_days = (t2 - t1).total_seconds() / 86400.0
                recency_w = 1.0 + 0.5 * (2.0 ** (-gap_days / 14.0))
                recency_w = min(recency_w, 1.5)
            except Exception:
                pass

        effective_alpha = alpha * strength * recency_w
        conf = float(chunk.get("confidence") or 0.5)
        new_conf = conf + effective_alpha * (target - conf)
        new_conf = max(0.0, min(1.0, new_conf))
        self.storage.update_chunk_confidence(chunk_id, new_conf, reason)

    # ------------------------------------------------------------------
    # Public API: add
    # ------------------------------------------------------------------
    def add(
        self,
        content: str,
        kind: str = "note",
        trigger_desc: str | None = None,
        anti_trigger_desc: str | None = None,
        source: str = "chat",
    ) -> str:
        """写入外部确认的知识."""
        if kind not in ("note", "skill"):
            raise InvalidStateError(f"invalid kind: {kind}")
        if self.sanitize:
            content, action = self.sanitize(content)
            if action == "discard":
                return ""  # §二·六 不写 chunk

        h = content_hash(content)
        if self.storage.is_invalidated(h):
            raise InvalidStateError("content hash is invalidated")

        # §六·五 幂等性: install 靠 content_hash 应用层去重.
        # 同一 hash 已有非 archived chunk → 返回已有 id, 不重复写入.
        existing = self.storage.conn.execute(
            "SELECT id FROM chunks WHERE content_hash=? AND state IN ('active','pending') "
            "ORDER BY created_at ASC LIMIT 1",
            (h,),
        ).fetchone()
        if existing:
            return existing["id"]

        now = utc_now_iso()
        chunk_id = gen_uuid()

        # §二·六 写入路径与 redact 落点:
        # - add(source=manual/chat) + redact: 脱敏后可 active, conf ≤ 0.4
        # - add(source=agent) + redact: 脱敏后强制 pending, conf ≤ 0.4
        # - add + allow: 按 kind/source 默认 conf
        sanitized = (action == "redact") if self.sanitize else False
        if source == "agent":
            origin = "captured"
            state = "pending"
            conf = 0.4 if sanitized else 0.60
            prot = 0
            state_reason = "init:captured_agent"
        elif kind == "skill":
            origin = "installed"
            state = "active"
            conf = 0.4 if sanitized else 0.85
            prot = 1
            state_reason = "init:installed"
        else:
            origin = "captured"
            state = "active"
            conf = 0.4 if sanitized else 0.60
            prot = 0
            state_reason = "init:captured"

        tokens = estimate_tokens(content)

        chunk = {
            "id": chunk_id,
            "content": content,
            "trigger_desc": trigger_desc,
            "anti_trigger_desc": anti_trigger_desc,
            "content_hash": h,
            "token_count": tokens,
            "origin": origin,
            "source": source,
            "protected": prot,
            "state": state,
            "state_reason": state_reason,
            "confidence": conf,
            "confidence_reason": f"init:{origin}",
            "created_at": now,
            "updated_at": now,
        }

        # embedding
        embed_ok = True
        try:
            cvec = self.embedding.embed_content(content)
            tvec = self.embedding.embed_trigger(trigger_desc or content)
        except Exception:
            embed_ok = False
            chunk["embed_version"] = 0
            chunk["state_reason"] = f"embedding_pending:target={state}"
            # 保持 state 写入,但 embed_version=0 标记待补

        self.storage.insert_chunk(chunk)
        if embed_ok:
            self.storage.insert_vec_content(chunk_id, cvec)
            # tvec 已在 try 块中按 embed_trigger(trigger_desc or content) 计算,
            # 无论是否有 trigger_desc 都使用正确的 trigger 向量(§三 地基#8 双向量).
            self.storage.insert_vec_trigger(chunk_id, tvec)
        self.storage.conn.commit()
        return chunk_id

    # ------------------------------------------------------------------
    # Public API: spark
    # ------------------------------------------------------------------
    def spark(self, content: str, trigger_desc: str | None = None,
              anti_trigger_desc: str | None = None) -> str:
        """记录灵感.

        §二·七 边界: trigger/anti_trigger 都可选(灵感往往没想清楚边界).
        保留 anti_trigger_desc 接口以便上层明确表达"不适用场景".
        """
        if self.sanitize:
            content, action = self.sanitize(content)
            if action == "discard":
                return ""

        now = utc_now_iso()
        chunk_id = gen_uuid()
        h = content_hash(content)

        if self.storage.is_invalidated(h):
            raise InvalidStateError("content hash is invalidated")

        # 入库时自动 recall 一次,找 related_ids
        related = []
        try:
            result = self.recall(content, budget=2000, trace=False)
            related = [c["id"] for c in result.knowledge[:5]]
        except Exception:
            pass

        chunk = {
            "id": chunk_id,
            "content": content,
            "trigger_desc": trigger_desc,
            "anti_trigger_desc": anti_trigger_desc,
            "content_hash": h,
            "token_count": estimate_tokens(content),
            "origin": "spark",
            "maturity": "seed",
            "related_ids": ",".join(related) if related else None,
            "protected": 0,
            "state": "active",  # spark 的 state 只是占位,实际用 maturity
            "state_reason": None,
            "confidence": 0.5,  # 语义 NULL,不参与排序
            "confidence_reason": None,
            "created_at": now,
            "updated_at": now,
        }

        embed_ok = True
        try:
            cvec = self.embedding.embed_content(content)
            tvec = self.embedding.embed_trigger(trigger_desc or content)
        except Exception:
            embed_ok = False
            chunk["embed_version"] = 0
            chunk["state_reason"] = "embedding_pending:target=active"

        self.storage.insert_chunk(chunk)
        if embed_ok:
            self.storage.insert_vec_content(chunk_id, cvec)
            self.storage.insert_vec_trigger(chunk_id, tvec)
        self.storage.conn.commit()
        return chunk_id

    def promote_spark(self, spark_id: str, to: str = "note") -> str:
        """灵感孵化晋升.

        to=note: 转 captured note, active, conf=0.60, protected=0, state_reason=init:captured
        to=skill: 转 installed skill, active, conf=0.85, protected=1, state_reason=init:installed
        """
        spark = self.storage.get_chunk(spark_id)
        if not spark or spark.get("origin") != "spark":
            raise ChunkNotFoundError(f"spark {spark_id} not found")
        if spark.get("maturity") in ("promoted", "dropped"):
            raise InvalidStateError(f"spark {spark_id} already {spark['maturity']}")
        if to not in ("note", "skill"):
            raise InvalidStateError(f"invalid spark promotion target: {to}")

        if self.sanitize:
            content, action = self.sanitize(spark["content"])
            if action == "discard":
                # §二·六 拒绝晋升, maturity 保持原状(incubating/seed)
                raise InvalidStateError("sanitize discard on promote")
        else:
            content = spark["content"]
            action = "allow"

        promoted_hash = content_hash(content)
        if (
            self.storage.is_invalidated(spark["content_hash"])
            or self.storage.is_invalidated(promoted_hash)
        ):
            raise InvalidStateError("spark content hash is invalidated")

        now = utc_now_iso()
        new_id = gen_uuid()

        # §五 kb.add 语义表: skill/note 默认参数
        if to == "skill":
            state = "active"
            conf = 0.85
            prot = 1
            origin = "installed"
            state_reason = "init:installed"
        else:  # to == "note" (默认)
            state = "active"
            conf = 0.60
            prot = 0
            origin = "captured"
            state_reason = "init:captured"

        # redact 后 conf 上限 0.4
        if action == "redact" and self.sanitize:
            conf = 0.4

        chunk = {
            "id": new_id,
            "content": content,
            "trigger_desc": spark.get("trigger_desc"),
            "anti_trigger_desc": spark.get("anti_trigger_desc"),
            "content_hash": promoted_hash,
            "token_count": estimate_tokens(content),
            "origin": origin,
            "source": "manual",
            "protected": prot,
            "state": state,
            "state_reason": state_reason,
            "confidence": conf,
            "confidence_reason": "manual_set",
            "parent_id": spark_id,
            "created_at": now,
            "updated_at": now,
        }

        embed_ok = True
        try:
            cvec = self.embedding.embed_content(content)
            tvec = self.embedding.embed_trigger(spark.get("trigger_desc") or content)
        except Exception:
            embed_ok = False
            chunk["embed_version"] = 0
            chunk["state_reason"] = f"embedding_pending:target={state}"

        self.storage.insert_chunk(chunk)
        if embed_ok:
            self.storage.insert_vec_content(new_id, cvec)
            self.storage.insert_vec_trigger(new_id, tvec)

        # 原 spark 标 maturity='promoted' (§二·七). state 保持 'active'(spark 永远不归档).
        self.storage.conn.execute(
            "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
            (now, spark_id),
        )
        self.storage.conn.commit()
        return new_id

    def drop_spark(self, spark_id: str, reason: str = "") -> None:
        """放弃灵感."""
        spark = self.storage.get_chunk(spark_id)
        if not spark or spark.get("origin") != "spark":
            raise ChunkNotFoundError(f"spark {spark_id} not found")
        if spark.get("maturity") == "promoted":
            raise InvalidStateError(f"spark {spark_id} already promoted")
        if spark.get("maturity") == "dropped":
            return
        now = utc_now_iso()
        self.storage.conn.execute(
            "UPDATE chunks SET maturity='dropped', state_reason=?, updated_at=? WHERE id=?",
            (f"dropped:{reason}" if reason else "dropped", now, spark_id),
        )
        self.storage.conn.commit()

    # ------------------------------------------------------------------
    # Public API: approve / archive / invalidate / restore
    # ------------------------------------------------------------------
    def approve(self, chunk_id: str) -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        self.storage.update_chunk_state(chunk_id, "active", "approved")
        self.storage.conn.commit()

    def archive(self, chunk_id: str, reason: str = "stale") -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        if chunk.get("protected"):
            raise InvalidStateError("cannot archive protected chunk")
        self.storage.update_chunk_state(chunk_id, "archived", reason)
        self.storage.conn.commit()

    def invalidate(self, chunk_id: str, reason: str = "") -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        now = utc_now_iso()
        h = chunk["content_hash"]
        # 1. 归档 + confidence 归零
        self.storage.conn.execute(
            "UPDATE chunks SET state='archived', confidence=0.0, state_reason=?, updated_at=? WHERE id=?",
            (f"invalidated:{reason}" if reason else "invalidated", now, chunk_id),
        )
        # 2. 同 hash 连带
        self.storage.conn.execute(
            "UPDATE chunks SET state='archived', confidence=0.0, state_reason=? WHERE content_hash=? AND id!=?",
            (f"invalidated:same_hash", h, chunk_id),
        )
        # 3. 重入黑名单
        self.storage.insert_invalidated_hash(h, reason, now)
        self.storage.conn.commit()

    def restore(self, chunk_id: str) -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        self.storage.update_chunk_state(chunk_id, "active", "restore")
        self.storage.conn.commit()

    # ------------------------------------------------------------------
    # Public API: evolve
    # ------------------------------------------------------------------
    def evolve(self, trigger: str = "manual") -> Dict[str, Any]:
        """成长:distill + curate + aggregate + purge."""
        result: Dict[str, Any] = {"distilled": 0, "curate": None}

        # threshold 检查
        if trigger == "threshold":
            row = self.storage.conn.execute(
                "SELECT COUNT(*) AS cnt FROM episodic_log WHERE distill_state='new'"
            ).fetchone()
            if row and row["cnt"] < self.evolve_threshold:
                return result

        # token 熔断检查
        max_tokens_str = self.storage.get_meta("max_distill_tokens_per_period")
        if max_tokens_str and trigger == "threshold":
            max_tokens = int(max_tokens_str)
            tok = self.storage.conn.execute(
                "SELECT COALESCE(SUM(distill_prompt_tokens),0) + COALESCE(SUM(distill_completion_tokens),0) AS total FROM episodic_log"
            ).fetchone()
            if tok and tok["total"] and tok["total"] > max_tokens:
                result["curate"] = CurateReport(warnings=[f"token budget exceeded: {tok['total']} > {max_tokens}"])
                return result

        # 1. distill
        distilled = self._distill_batch()
        result["distilled"] = distilled

        # 2. curate (含 aggregate + purge)
        scope = CurateScope()
        report = self._curator.run(self, scope)
        result["curate"] = report

        return result

    def _distill_batch(self, batch_size: int | None = None) -> int:
        """简单蒸馏:将 new 日志提炼为 pending chunk.

        状态机收口: _distill_one 内部已通过 _distill_finalize 处理 sanitize_discard
        / embedding_failed 等终态, 此处只需判定"成功/不足"两类结果.
        """
        run_id = gen_uuid()
        locked_at = utc_now_iso()
        bs = batch_size if batch_size is not None else self.distill_batch_size
        logs = self.storage.claim_logs_for_distill(run_id, bs, locked_at)
        count = 0
        for log in logs:
            # 防御: claim 完可能已是终态(并发场景)
            if log["distill_state"] != "screening":
                continue
            try:
                chunk_id = self._distill_one(log)
                # 简化版 token 估算(实际 LLM 蒸馏时应替换为真实值)
                prompt_tokens = estimate_tokens(log.get("output_summary") or log.get("query") or "")
                completion_tokens = estimate_tokens(log.get("output_summary") or "")
                if chunk_id:
                    count += 1
                    self._distill_finalize(log, "distilled", None)
                else:
                    # _distill_one 内部可能已 finalize (sanitize_discard/embed_failed),
                    # 终态不再覆盖. insufficient_material 由 _distill_batch 标.
                    current = self.storage.conn.execute(
                        "SELECT distill_state FROM episodic_log WHERE id=?", (log["id"],),
                    ).fetchone()
                    if current and current["distill_state"] == "screening":
                        self._distill_finalize(log, "discarded", "insufficient_material")
                # 记入 token 估算(无论终态如何, 都做估算)
                self.storage.conn.execute(
                    "UPDATE episodic_log SET distill_prompt_tokens=?, distill_completion_tokens=? WHERE id=?",
                    (prompt_tokens, completion_tokens, log["id"]),
                )
            except Exception as exc:
                self._distill_finalize(log, "failed", f"distill_failed:{exc}")
        self.storage.conn.commit()
        return count

    def _distill_one(self, log: Dict[str, Any]) -> str | None:
        """单条日志蒸馏. 调用 Distiller 提炼,失败时 sanitize/embed 路径按规约处理.

        返回值约定:
            str  - 成功,新 chunk_id
            None - 内容不足 (insufficient_material),不写终态
        sanitize_discard / embed_failed 等终态由调用方 _distill_batch 统一 UPDATE,
        避免 _distill_one 与 _distill_batch 之间的状态机竞态.
        """
        result = self.distiller.distill(log, self.embedding)
        if result is None:
            return None

        content = result.get("content")
        if not content:
            return None

        if self.sanitize:
            content, action = self.sanitize(content)
            if action == "discard":
                # 通知调用方:这是 sanitize_discard 终态
                self._distill_finalize(log, "discarded", "sanitize_discard")
                return None

        h = content_hash(content)
        if self.storage.is_invalidated(h):
            return None

        # §六·五 幂等性: distilled_from UNIQUE 索引保证重跑不重复
        existing = self.storage.conn.execute(
            "SELECT id FROM chunks WHERE distilled_from=?", (log["id"],),
        ).fetchone()
        if existing:
            return existing["id"]

        now = utc_now_iso()
        chunk_id = gen_uuid()
        trigger = result.get("trigger_desc") or log.get("query", "")[:200]
        anti = result.get("anti_trigger_desc") or ""

        # §二·六 distill() redact 路径: 脱敏写 pending, conf ≤ 0.4
        sanitized = (action == "redact") if self.sanitize else False
        chunk_conf = 0.4 if sanitized else 0.45

        chunk = {
            "id": chunk_id,
            "content": content,
            "trigger_desc": trigger,
            "anti_trigger_desc": anti,
            "content_hash": h,
            "token_count": estimate_tokens(content),
            "origin": "distilled",
            "source": None,
            "protected": 0,
            "state": "pending",
            "state_reason": "init:distilled",
            "confidence": chunk_conf,
            "confidence_reason": "init:distilled",
            "distilled_from": log["id"],
            "created_at": now,
            "updated_at": now,
        }

        embed_ok = True
        try:
            cvec = self.embedding.embed_content(content)
            tvec = self.embedding.embed_trigger(trigger)
        except Exception:
            embed_ok = False

        if not embed_ok:
            # §六·五 蒸馏结果强依赖向量,无向量不写半成品
            self._distill_finalize(log, "failed", "embedding_failed")
            return None

        self.storage.insert_chunk(chunk)
        self.storage.insert_vec_content(chunk_id, cvec)
        self.storage.insert_vec_trigger(chunk_id, tvec)
        return chunk_id

    def _distill_finalize(self, log: Dict[str, Any], state: str, note: str | None) -> None:
        """蒸馏终态标定 — 统一 UPDATE 入口,避免与 _distill_batch 竞态.

        只覆盖状态字段 (distill_state / distill_note / run_id / locked_at),
        不动 distill_prompt_tokens / distill_completion_tokens (由 _distill_batch 后续写).
        episodic_log 的 ts 字段含义是"trace 发生时间", 状态变更不应回写 ts.
        """
        self.storage.conn.execute(
            "UPDATE episodic_log SET distill_state=?, distill_note=?, "
            "distill_run_id=NULL, distill_locked_at=NULL WHERE id=?",
            (state, note, log["id"]),
        )

    # ------------------------------------------------------------------
    # builtin curate
    # ------------------------------------------------------------------
    def _builtin_curate(self, scope: CurateScope) -> CurateReport:
        report = CurateReport()
        now = utc_now_iso()

        cutoff_ts = now
        if not scope.dry_run:
            # 1. aggregate (cutoff_ts 固定)
            last_ts = self.storage.get_meta("last_agg_ts") or "1970-01-01T00:00:00.000Z"
            self.storage.aggregate_success_traces(last_ts, cutoff_ts)
            self.storage.aggregate_counters(last_ts, cutoff_ts)
            self.storage.aggregate_success_counts()
            self.storage.set_meta("last_agg_ts", cutoff_ts)

            # 2. purge_logs 前置: stale screening + open TTL
            stale = self.storage.purge_stale_screening(self.screening_timeout_minutes, now)
            if stale:
                report.warnings.append(f"recovered {stale} stale screening rows")
            open_purged = self.storage.purge_open_timeout(self.open_ttl_days, "no_record_timeout")
            if open_purged:
                report.warnings.append(f"purged {open_purged} open timeout rows")

        # 3. archive rules:必须先于 decay,避免低分失效块被拉回中性下限后逃过归档.
        self._curate_archive(report, now, scope)

        # 4. dedupe
        self._curate_dedupe(report, now, scope)

        # 5. decay
        self._curate_decay(report, now, scope)

        # 6. promote pending → active
        self._curate_promote(report, scope)

        # 7. cycle / orphan (简化:仅检测,只读)
        self._curate_cycles(report, scope)

        if not scope.dry_run:
            # 7. purge usage_trace
            purged = self.storage.purge_usage_trace(cutoff_ts)
            report.stats["purged_traces"] = purged

            # 8. purge old episodic_log
            old_logs = self.storage.purge_old_logs(30)
            report.stats["purged_logs"] = old_logs

            self.storage.conn.commit()
        return report

    @staticmethod
    def _matches_scope(row: Any, scope: CurateScope) -> bool:
        """CurateScope 只限制治理目标,不改变全库聚合."""
        if scope.origin and row["origin"] != scope.origin:
            return False
        if scope.skill_name and row["skill_name"] != scope.skill_name:
            return False
        return True

    def _curate_decay(self, report: CurateReport, now_iso: str, scope: CurateScope) -> None:
        from datetime import datetime
        rows = self.storage.conn.execute(
            """SELECT id, origin, skill_name, confidence, last_used_at
               FROM chunks WHERE state='active' AND origin!='spark'"""
        ).fetchall()
        for row in rows:
            if not self._matches_scope(row, scope):
                continue
            last = row["last_used_at"]
            if not last:
                continue
            try:
                t1 = datetime.fromisoformat(last.replace("Z", "+00:00"))
                t2 = datetime.fromisoformat(now_iso.replace("Z", "+00:00"))
                idle_days = (t2 - t1).total_seconds() / 86400.0
            except Exception:
                continue
            if idle_days <= 0:
                continue
            conf = float(row["confidence"] or 0.5)
            floor = 0.3
            new_conf = floor + (conf - floor) * (0.5 ** (idle_days / 90.0))
            new_conf = round(new_conf, 4)
            if abs(new_conf - conf) > 0.001:
                if not scope.dry_run:
                    self.storage.update_chunk_confidence(
                        row["id"], new_conf, f"decay:{int(idle_days)}d"
                    )
                report.decayed.append(row["id"])

    def _curate_dedupe(self, report: CurateReport, now_iso: str, scope: CurateScope) -> None:
        rows = self.storage.conn.execute(
            """SELECT id, origin, skill_name, content_hash, confidence, protected, state
               FROM chunks
               WHERE state IN ('active','pending') AND origin!='spark'
               ORDER BY protected DESC, confidence DESC"""
        ).fetchall()
        seen: Dict[str, str] = {}  # hash → canonical id
        for row in rows:
            h = row["content_hash"]
            if h in seen:
                canonical = seen[h]
                if row["protected"]:
                    continue
                if not self._matches_scope(row, scope):
                    continue
                if not scope.dry_run:
                    self.storage.update_chunk_state(
                        row["id"], "archived", f"duplicate:{canonical}"
                    )
                    # parent_id 指向 canonical,保留血缘关系
                    self.storage.conn.execute(
                        "UPDATE chunks SET parent_id=? WHERE id=?",
                        (canonical, row["id"]),
                    )
                report.deduped.append(row["id"])
            else:
                seen[h] = row["id"]

    def _curate_archive(self, report: CurateReport, now_iso: str, scope: CurateScope) -> None:
        from datetime import datetime
        t2 = datetime.fromisoformat(now_iso.replace("Z", "+00:00"))

        rows = self.storage.conn.execute(
            "SELECT * FROM chunks WHERE state IN ('active','pending')"
        ).fetchall()
        for row in rows:
            if row["protected"]:
                continue
            if row["origin"] == "spark":
                continue
            if not self._matches_scope(row, scope):
                continue

            cid = row["id"]
            conf = float(row["confidence"] or 0.5)
            last_used = row["last_used_at"]
            selected_cnt = int(row["selected_count"] or 0)
            used_cnt = int(row["used_count"] or 0)

            # low_confidence
            if last_used:
                try:
                    t1 = datetime.fromisoformat(last_used.replace("Z", "+00:00"))
                    idle_days = (t2 - t1).total_seconds() / 86400.0
                except Exception:
                    idle_days = 0
                if conf < self.low_conf_threshold and idle_days > self.low_conf_idle_days:
                    if not scope.dry_run:
                        self.storage.update_chunk_state(cid, "archived", "low_confidence")
                    report.archived.append(cid)
                    continue

            # repeated_selected_unused
            if selected_cnt >= self.repeat_select_min and used_cnt == 0 and conf < self.repeat_select_conf_max:
                if not scope.dry_run:
                    self.storage.update_chunk_state(cid, "archived", "repeated_selected_unused")
                report.archived.append(cid)
                continue

            # never_used
            created = row["created_at"]
            if not last_used and selected_cnt == 0 and used_cnt == 0 and created:
                try:
                    t1 = datetime.fromisoformat(created.replace("Z", "+00:00"))
                    age_days = (t2 - t1).total_seconds() / 86400.0
                except Exception:
                    age_days = 0
                if age_days > self.never_used_age_days:
                    if not scope.dry_run:
                        self.storage.update_chunk_state(cid, "archived", "never_used")
                    report.archived.append(cid)

    def _curate_promote(self, report: CurateReport, scope: CurateScope) -> None:
        rows = self.storage.conn.execute(
            """SELECT id, origin, skill_name, used_success_count, success_trace_ids_count, confidence
               FROM chunks WHERE state='pending'"""
        ).fetchall()
        for row in rows:
            if not self._matches_scope(row, scope):
                continue
            if (int(row["used_success_count"] or 0) >= self.promote_used_success_min
                and int(row["success_trace_ids_count"] or 0) >= 2
                and float(row["confidence"] or 0) >= self.promote_confidence_min):
                if not scope.dry_run:
                    self.storage.update_chunk_state(row["id"], "active", "repeated_success")

    def _curate_cycles(self, report: CurateReport, scope: CurateScope) -> None:
        # 简化环检测:DFS
        deps_rows = self.storage.conn.execute("SELECT src, dst FROM deps WHERE kind='hard'").fetchall()
        graph: Dict[str, List[str]] = {}
        for r in deps_rows:
            graph.setdefault(r["src"], []).append(r["dst"])
        visited = set()
        rec_stack = set()

        def dfs(node: str, path: List[str]) -> bool:
            visited.add(node)
            rec_stack.add(node)
            for nxt in graph.get(node, []):
                if nxt not in visited:
                    if dfs(nxt, path + [nxt]):
                        return True
                elif nxt in rec_stack:
                    cycle_start = path.index(nxt)
                    report.cycles.append(path[cycle_start:] + [nxt])
                    return True
            rec_stack.remove(node)
            return False

        for node in list(graph.keys()):
            if node not in visited:
                dfs(node, [node])
        if graph:
            connected = set(graph)
            for destinations in graph.values():
                connected.update(destinations)
            rows = self.storage.conn.execute(
                """SELECT id, origin, skill_name FROM chunks
                   WHERE state IN ('active','pending') AND origin!='spark'"""
            ).fetchall()
            report.orphans.extend(
                row["id"]
                for row in rows
                if self._matches_scope(row, scope) and row["id"] not in connected
            )

    # ------------------------------------------------------------------
    # Public API: inspect
    # ------------------------------------------------------------------
    def inspect(self, chunk_id: str | None = None, trace_id: str | None = None) -> Dict[str, Any]:
        if chunk_id:
            return self._inspect_chunk(chunk_id)
        if trace_id:
            return self._inspect_trace(trace_id)
        return self._inspect_library()

    def _inspect_chunk(self, chunk_id: str) -> Dict[str, Any]:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        # 关联/衍生(§三·六 invalidate 第 3 防护 衍生提示)
        # - parent_id: 由 promote_spark / 父子关系产生的下游
        # - distilled_from: 由 distill 从该 chunk (作为 episodic_log.id) 派生的 chunk
        related_parent = self.storage.conn.execute(
            "SELECT id, state, confidence, 'parent_id' AS via FROM chunks WHERE parent_id=?",
            (chunk_id,),
        ).fetchall()
        related_distilled = self.storage.conn.execute(
            "SELECT id, state, confidence, 'distilled_from' AS via FROM chunks WHERE distilled_from=?",
            (chunk_id,),
        ).fetchall()
        related = [dict(r) for r in related_parent] + [dict(r) for r in related_distilled]
        return {
            "chunk": dict(chunk),
            "related": related,
        }

    def _inspect_trace(self, trace_id: str) -> Dict[str, Any]:
        log = self.storage.get_log_by_trace(trace_id)
        traces = self.storage.conn.execute(
            "SELECT * FROM usage_trace WHERE trace_id=? ORDER BY ts", (trace_id,)
        ).fetchall()
        return {
            "log": dict(log) if log else None,
            "traces": [dict(r) for r in traces],
        }

    def _inspect_library(self) -> Dict[str, Any]:
        stats = self.storage.conn.execute(
            """SELECT
               SUM(CASE WHEN state='active' THEN 1 ELSE 0 END) AS active,
               SUM(CASE WHEN state='pending' THEN 1 ELSE 0 END) AS pending,
               SUM(CASE WHEN state='archived' THEN 1 ELSE 0 END) AS archived,
               SUM(CASE WHEN origin='spark' THEN 1 ELSE 0 END) AS sparks,
               SUM(CASE WHEN embed_version < ? THEN 1 ELSE 0 END) AS pending_embed,
               SUM(CASE WHEN origin!='spark' AND state='active' THEN 1 ELSE 0 END) AS knowledge_active,
               SUM(CASE WHEN origin!='spark' AND state='pending' THEN 1 ELSE 0 END) AS knowledge_pending
               FROM chunks"""
            ,
            (int(self.storage.get_meta("embed_version") or "1"),),
        ).fetchone()

        active = stats["active"] or 0
        pending = stats["pending"] or 0
        archived = stats["archived"] or 0
        total = active + pending + archived

        # 僵尸块 = confidence [0.4, 0.6] 且已存在 > 7 天的 active 块(§五 inspect 五个健康信号).
        # 时间过滤排除刚加入的 captured note(默认 conf=0.60),只标记"卡在中间"很久的块.
        zombie_row = self.storage.conn.execute(
            "SELECT COUNT(*) AS c FROM chunks "
            "WHERE state='active' AND origin!='spark' "
            "AND confidence >= 0.4 AND confidence <= 0.6 "
            "AND created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')"
        ).fetchone()
        zombie = zombie_row["c"] or 0

        # spark 不参与知识债务比.
        knowledge_pending = stats["knowledge_pending"] or 0
        valid = (stats["knowledge_active"] or 0) + knowledge_pending
        debt = (knowledge_pending + zombie) / max(valid, 1)

        # stale screening
        stale = self.storage.conn.execute(
            """SELECT COUNT(*) AS cnt FROM episodic_log
               WHERE distill_state='screening'
                 AND distill_locked_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 minutes')"""
        ).fetchone()["cnt"] or 0

        # distill tokens
        tok = self.storage.conn.execute(
            "SELECT COALESCE(SUM(distill_prompt_tokens),0) AS p, COALESCE(SUM(distill_completion_tokens),0) AS c FROM episodic_log"
        ).fetchone()

        # spark 提示
        spark_hints = []
        spark_rows = self.storage.conn.execute(
            """SELECT c.id, COUNT(*) AS cnt
               FROM chunks c
               JOIN usage_trace u ON u.chunk_id = c.id AND u.event='retrieved'
               WHERE c.origin='spark'
                 AND c.state!='archived'
                 AND c.maturity NOT IN ('promoted','dropped')
               GROUP BY c.id HAVING cnt > 3"""
        ).fetchall()
        for r in spark_rows:
            spark_hints.append({"id": r["id"], "recall_count": r["cnt"]})

        # 上限阈值(用于 inspect 展示)
        max_distill_tokens = int(self.storage.get_meta("max_distill_tokens_per_period") or 0)

        # §二·五A "Recall 可配置参数": 'innate inspect 库体检时打印当前值'
        recall_params = {
            "w_content": self.w_content,
            "w_trigger": self.w_trigger,
            "w_confidence": self.w_confidence,
            "top_k_candidates": self.top_k_candidates,
            "anti_trigger_penalty": self.anti_trigger_penalty,
            "density_refill": self.density_refill,
        }
        curate_params = {
            "low_conf_threshold": self.low_conf_threshold,
            "low_conf_idle_days": self.low_conf_idle_days,
            "repeat_select_min": self.repeat_select_min,
            "repeat_select_conf_max": self.repeat_select_conf_max,
            "never_used_age_days": self.never_used_age_days,
            "open_ttl_days": self.open_ttl_days,
            "screening_timeout_minutes": self.screening_timeout_minutes,
            "promote_used_success_min": self.promote_used_success_min,
            "promote_confidence_min": self.promote_confidence_min,
        }

        return {
            "db": self.db_path,
            "chunks": {"active": active, "pending": pending, "archived": archived, "total": total,
                       "zombie": zombie},
            "knowledge_debt_ratio": round(debt, 2),
            "pending_embed_rebuild": stats["pending_embed"] or 0,
            "stale_screening": stale,
            "distill_tokens": {"prompt": tok["p"] or 0, "completion": tok["c"] or 0,
                               "max": max_distill_tokens},
            "spark_hints": spark_hints,
            "recall_params": recall_params,
            "curate_params": curate_params,
        }

    # ------------------------------------------------------------------
    # Public API: @augmented
    # ------------------------------------------------------------------
    def augmented(self, budget: int = 6000):
        """装饰器:自动 recall + 注入 context."""
        def decorator(func: Callable):
            def wrapper(*args, **kwargs):
                # 尝试从第一个位置参数取 query
                query = args[0] if args else ""
                if not query:
                    return func(*args, **kwargs)
                ctx = self.recall(query, budget=budget, trace=True)
                # 注入 context 到 kwargs
                kwargs["_innate_context"] = ctx
                result = func(*args, **kwargs)
                # 尝试解析 outcome
                if isinstance(result, dict) and "outcome" in result:
                    self.record(
                        ctx.trace_id,
                        outcome=result["outcome"],
                        output_summary=result.get("output_summary"),
                        source="augmented",
                    )
                return result
            return wrapper
        return decorator

    # ------------------------------------------------------------------
    # rebuild embeddings
    # ------------------------------------------------------------------
    def rebuild_embeddings(self) -> int:
        """重建 embed_version=0 或落后的 chunk 向量."""
        meta_ev = int(self.storage.get_meta("embed_version") or "1")
        rows = self.storage.conn.execute(
            "SELECT * FROM chunks WHERE embed_version < ? OR embed_version=0", (meta_ev,)
        ).fetchall()
        count = 0
        for row in rows:
            try:
                cvec = self.embedding.embed_content(row["content"])
                trigger_text = row["trigger_desc"] if row["trigger_desc"] is not None else row["content"]
                tvec = self.embedding.embed_trigger(trigger_text)
                # 删除旧向量再插入
                self.storage.delete_vec_content(row["id"])
                self.storage.delete_vec_trigger(row["id"])
                self.storage.insert_vec_content(row["id"], cvec)
                self.storage.insert_vec_trigger(row["id"], tvec)
                # 更新 embed_version
                self.storage.conn.execute(
                    "UPDATE chunks SET embed_version=?, updated_at=? WHERE id=?",
                    (meta_ev, utc_now_iso(), row["id"]),
                )
                # 恢复 state_reason
                sr = row["state_reason"] or "" if row["state_reason"] is not None else ""
                if sr.startswith("embedding_pending:target="):
                    target = sr.split("=")[1]
                    self.storage.conn.execute(
                        "UPDATE chunks SET state=?, state_reason='embedding_rebuilt' WHERE id=?",
                        (target, row["id"]),
                    )
                count += 1
            except Exception:
                continue
        self.storage.conn.commit()
        return count
