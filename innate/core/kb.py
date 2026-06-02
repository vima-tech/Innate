"""Core SDK — KnowledgeBase: 8 类 Public API 能力域的完整实现."""

from __future__ import annotations

import functools
import inspect
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Tuple

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
    skipped_reasons: Dict[str, str] = field(default_factory=dict)
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
    EVENT_SOURCES = ("sdk", "cli", "hook", "daemon", "augmented")
    EVOLVE_TRIGGERS = ("manual", "scheduled", "threshold")
    EXPAND_DEPS_MODES = (False, "direct", "closure")
    NOMINATION_DEFAULT_PRIORITY = 1

    def __init__(
        self,
        db_path: str,
        shared: List[str] | None = None,
        embedding: EmbeddingProvider | None = None,
        curator: Curator | None = None,
        refiner: Refiner | None = None,
        distiller: Distiller | None = None,
        sanitize: Callable[[str | None], Tuple[str | None, str]] | None = default_sanitize,
        storage_factory: Callable[..., Storage] = Storage,
    ):
        self.db_path = db_path
        self.shared_paths = shared or []
        self.embedding = embedding or DummyEmbeddingProvider()
        self.sanitize = sanitize
        self._curator = curator or Curator()
        self.refiner = refiner or NullRefiner()
        self.distiller = distiller or HeuristicDistiller()
        self.storage_factory = storage_factory

        self.storage = self.storage_factory(
            db_path,
            content_dim=self.embedding.content_dim,
            trigger_dim=self.embedding.trigger_dim,
        )
        self._shared_storages: Dict[str, Storage] = {}
        try:
            for sp in self.shared_paths:
                self._shared_storages[sp] = self.storage_factory(
                    sp,
                    content_dim=self.embedding.content_dim,
                    trigger_dim=self.embedding.trigger_dim,
                    read_only=True,
                )
            self._init_meta()
            self._validate_embedding_dims(self.storage)
            for st in self._shared_storages.values():
                self._validate_embedding_dims(st)
            self._load_params()
        except Exception:
            self.close()
            raise

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
        refine_mode: str = "off",
        include_sparks: bool = False,
        top: int | None = None,
        source: str = "sdk",
    ) -> RecallResult:
        """同步召回. 纯数学,零模型调用(allow_trim 除外)."""
        if allow_trim and refine_mode == "off":
            refine_mode = "trim"
        if refine_mode not in ("off", "trim", "adapt"):
            raise InvalidStateError(f"invalid refine_mode: {refine_mode}")
        if expand_deps not in self.EXPAND_DEPS_MODES:
            raise InvalidStateError(f"invalid expand_deps: {expand_deps}")
        self._validate_event_source(source)
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
        selected, skipped, skipped_reasons = self._pack(
            scored, budget, expand_deps, allow_trim, query, refine_mode
        )
        depth_skipped = [
            cid for cid, reason in skipped_reasons.items()
            if reason == "dep_depth_limit"
        ]

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
            for rank, item in enumerate(retrieved, start=1):
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": item["id"],
                    "event": "retrieved",
                    "similarity": item.get("_fused_score"),
                    "rank": rank,
                    "refine_mode": (
                        f"skipped:{skipped_reasons[item['id']]}"
                        if item["id"] in skipped_reasons else None
                    ),
                    "source": source,
                    "ts": now,
                })
            # refined trace (trim 发生时)
            refined = {
                item["id"]: item.pop("_refined")
                for item in selected
                if item.get("_refined")
            }
            for cid, mode in refined.items():
                self.storage.append_trace({
                    "trace_id": trace_id,
                    "chunk_id": cid,
                    "event": "refined",
                    "strength": 0.5,
                    "refine_mode": mode,
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
                "skipped_reasons": skipped_reasons,
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
            skipped_reasons=skipped_reasons,
            empty=len(visible_knowledge) == 0 and len(sparks) == 0,
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
        storages_to_query = self._storages_to_query(libs)

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
        storages_to_query = self._storages_to_query(libs)

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

    def _storages_to_query(self, libs: List[str] | None) -> List[Storage]:
        """按调用方选择组装检索库;默认只查 personal."""
        if not libs:
            return [self.storage]
        storages: List[Storage] = []
        personal_id = self.storage.get_meta("lib_id")
        if any(lib in ("personal", self.db_path, personal_id) for lib in libs):
            storages.append(self.storage)
        for path, storage in self._shared_storages.items():
            shared_id = storage.get_meta("lib_id")
            if any(lib in ("shared", path, shared_id) for lib in libs):
                storages.append(storage)
        return storages

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
        refine_mode: str | None = None,
    ) -> Tuple[List[Dict[str, Any]], List[Tuple[List[Dict[str, Any]], float, int]], Dict[str, str]]:
        """first-fit 装包. 返回 (selected, skipped_blocks, skipped_reasons).

        skipped_reasons: 因依赖闭包不完整而被丢弃的 seed 及原因.
        设计原则(§六 hard 闭包双保险):'宁可不召回,不给半截 hard 依赖'——丢弃 seed 而非截断闭包.
        """
        selected: List[Dict[str, Any]] = []
        skipped: List[Tuple[List[Dict[str, Any]], float, int]] = []
        skipped_reasons: Dict[str, str] = {}
        used_ids = set()
        used_tokens = 0
        mode = refine_mode or ("trim" if allow_trim else "off")

        for fused_score, chunk in scored:
            cid = chunk["id"]
            if cid in used_ids:
                continue
            block, skip_reason = self._build_block_reason(cid, expand_deps)
            if skip_reason:
                skipped_reasons[cid] = skip_reason
                continue
            if not block:
                continue
            if mode == "adapt" and self.refiner.available:
                try:
                    refined = self.refiner.refine(block, query, "adapt")
                except Exception:
                    refined = None
                if refined and self._refine_intact(block, refined):
                    block = refined
                    for item in block:
                        item["_refined"] = "adapt"
            new_block = [b for b in block if b["id"] not in used_ids]
            cost = self._block_cost(new_block)

            if used_tokens + cost <= budget:
                for b in block:
                    if b["id"] not in used_ids:
                        b["_fused_score"] = fused_score
                        selected.append(b)
                        used_ids.add(b["id"])
                used_tokens += cost
                continue

            if mode == "trim" and self.refiner.available:
                # trim: 不破坏 hard 闭包,只裁非关键段落(由 Refiner 实现)
                try:
                    refined = self.refiner.refine(block, query, "trim")
                except Exception:
                    refined = None
                if refined and self._refine_intact(block, refined):
                    refined_cost = sum(
                        estimate_tokens(b["content"]) or 100 for b in refined
                    )
                    if used_tokens + refined_cost <= budget:
                        for b in refined:
                            if b["id"] not in used_ids:
                                b["_fused_score"] = fused_score
                                b["_refined"] = "trim"
                                selected.append(b)
                                used_ids.add(b["id"])
                        used_tokens += refined_cost
                        continue

            skipped.append((block, fused_score, cost))

        return selected, skipped, skipped_reasons

    @staticmethod
    def _refine_intact(original: List[Dict[str, Any]], refined: List[Dict[str, Any]]) -> bool:
        """refine 不得删除闭包成员或改写 protected 块."""
        original_by_id = {b["id"]: b for b in original}
        refined_by_id = {b.get("id"): b for b in refined}
        if set(original_by_id) != set(refined_by_id):
            return False
        if any("content" not in b for b in refined):
            return False
        return all(
            not block.get("protected")
            or refined_by_id[chunk_id]["content"] == block["content"]
            for chunk_id, block in original_by_id.items()
        )

    _deps_intact = _refine_intact

    @staticmethod
    def _block_cost(block: List[Dict[str, Any]]) -> int:
        return sum(
            (
                estimate_tokens(item["content"])
                if item.get("_refined")
                else item.get("token_count") or estimate_tokens(item["content"])
            )
            or 100
            for item in block
        )

    def _get_chunk_with_storage_any(
        self, chunk_id: str
    ) -> Tuple[Dict[str, Any] | None, Storage | None]:
        """跨库查找 chunk 及所属存储."""
        c = self.storage.get_chunk(chunk_id)
        if c:
            return c, self.storage
        for st in self._shared_storages.values():
            c = st.get_chunk(chunk_id)
            if c:
                return c, st
        return None, None

    def _get_available_hard_dep(
        self, chunk_id: str, storage: Storage
    ) -> Dict[str, Any] | None:
        """hard dep 必须在所属库内存在、可召回且向量未过期."""
        chunk = storage.get_chunk(chunk_id)
        if (
            not chunk
            or chunk["state"] == "archived"
            or chunk.get("origin") == "spark"
            or chunk["embed_version"] < int(storage.get_meta("embed_version") or "1")
        ):
            return None
        return chunk

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

    def _build_block_reason(
        self, seed_id: str, expand_deps: bool | str
    ) -> Tuple[List[Dict[str, Any]], str | None]:
        """构建不可分割块并返回失败原因."""
        block: List[Dict[str, Any]] = []
        seed, storage = self._get_chunk_with_storage_any(seed_id)
        if not seed or not storage:
            return block, "seed_missing"
        block.append(seed)

        if not expand_deps:
            return block, None

        if expand_deps == "direct":
            deps = storage.get_deps(seed_id, kind="hard")
            for dep in deps:
                chunk = self._get_available_hard_dep(dep["dst"], storage)
                if not chunk:
                    return [], "hard_dep_unavailable"
                block.append(chunk)
            return block, None

        if expand_deps == "closure":
            visited = {seed_id}
            queue = [(seed_id, 0)]
            depth_limit = 3
            while queue:
                src, depth = queue.pop(0)
                for dep in storage.get_deps(src, kind="hard"):
                    dst = dep["dst"]
                    if dst in visited:
                        continue
                    if depth + 1 > depth_limit:
                        return [], "dep_depth_limit"
                    visited.add(dst)
                    chunk = self._get_available_hard_dep(dst, storage)
                    if not chunk:
                        return [], "hard_dep_unavailable"
                    block.append(chunk)
                    queue.append((dst, depth + 1))
            return block, None

        return [], "invalid_expand_deps"

    def _build_block(self, seed_id: str, expand_deps: bool | str) -> Tuple[List[Dict[str, Any]], bool]:
        """构建不可分割块. 返回 (block, depth_exceeded).

        第二项为兼容旧调用保留:任何闭包不完整均返回 True.
        soft 依赖不强制装入,只作为候选加分(§四 装包只强制展开 hard 闭包).
        """
        block, reason = self._build_block_reason(seed_id, expand_deps)
        return block, reason is not None

    def _storage_for_lib(self, lib_ref: str | None) -> Storage | None:
        """按路径或 lib_id 找已挂载库."""
        if lib_ref in (None, "", "personal", self.db_path):
            return self.storage
        if lib_ref == self.storage.get_meta("lib_id"):
            return self.storage
        for path, storage in self._shared_storages.items():
            if lib_ref in (path, storage.get_meta("lib_id")):
                return storage
        return None

    def _resolve_soft_dep(self, dep: Dict[str, Any]) -> Dict[str, Any] | None:
        """解析已挂载库中的 soft 引用;失败不阻塞 seed."""
        dst_ref = dep.get("dst_ref") or dep.get("dst")
        dst_lib = dep.get("dst_lib")
        storage = self._storage_for_lib(dst_lib) if dst_lib else None
        if dst_lib:
            chunk = storage.get_chunk(dst_ref) if storage else None
        else:
            chunk, storage = self._get_chunk_with_storage_any(dst_ref)
        if not chunk or chunk["state"] == "archived" or chunk.get("origin") == "spark":
            return None
        if storage and chunk["embed_version"] < int(storage.get_meta("embed_version") or "1"):
            return None
        return chunk

    def _apply_soft_dep_bonus(self, candidates: Dict[str, Dict[str, Any]]) -> None:
        """将可解析 soft 引用作为普通候选加入并轻量加分."""
        for cid, info in list(candidates.items()):
            chunk = info["chunk"]
            if chunk.get("origin") == "spark":
                continue
            for dep in self._get_deps_any(cid, kind="soft"):
                target = self._resolve_soft_dep(dep)
                if not target or target["id"] == cid:
                    continue
                target_info = candidates.setdefault(
                    target["id"],
                    {"chunk": target, "sim_content": 0.0, "sim_trigger": 0.0},
                )
                target_info["sim_content"] = min(
                    1.0, target_info.get("sim_content", 0.0) + 0.05
                )

    def _density_refill(
        self,
        selected: List[Dict[str, Any]],
        skipped: List[Tuple[List[Dict[str, Any]], float, int]],
        budget: int,
    ) -> List[Dict[str, Any]]:
        """价值密度回填."""
        used_tokens = self._block_cost(selected)
        if used_tokens >= budget:
            return selected

        # 计算密度
        density_items = []
        for block, fscore, cost in skipped:
            # 过滤掉已选中的
            block = [b for b in block if b["id"] not in {x["id"] for x in selected}]
            if not block:
                continue
            cost2 = self._block_cost(block)
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
    @classmethod
    def _validate_event_source(cls, source: str) -> None:
        if source not in cls.EVENT_SOURCES:
            raise InvalidStateError(f"invalid event source: {source}")

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
        self._validate_event_source(source)
        effective_priority = (
            self.NOMINATION_DEFAULT_PRIORITY if nomination and priority == 0 else priority
        )
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
                    "priority": effective_priority,
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
            if effective_priority and effective_priority != log.get("priority"):
                updates["priority"] = effective_priority
            if source != log.get("event_source"):
                updates["event_source"] = source

            # open → new / discarded 判断
            # 只对 distill_state='open' 的 log 执行,避免把已是 'new' / 'screening' 等状态的
            # log 降级为 'discarded'(§五 record() 写入逻辑第 6 步:"outcome 补完后执行").
            current_distill_state = log.get("distill_state", "open")
            outcome_completed = outcome is not None or log.get("outcome") is not None
            if current_distill_state == "open" and outcome_completed:
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
                return  # 无 used 块,不更新 (设计:宁可漏奖,不可错奖)
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
                if fb == "judge_score" and isinstance(cids, dict):
                    for cid, score in cids.items():
                        value = max(0.0, min(1.0, float(score)))
                        self._update_confidence(
                            cid, value, 0.8, f"judge_score:{value:.2f}", now
                        )
                    continue
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
        if last_used and reason.split(":", 1)[0] in ("user_up", "user_down", "judge_score"):
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
    def _sanitize_content(self, content: str) -> Tuple[str, str]:
        """统一执行 sanitize 钩子并校验扩展点合同."""
        if not self.sanitize:
            return content, "allow"
        cleaned, action = self.sanitize(content)
        if action not in ("allow", "redact", "discard"):
            raise InvalidStateError(f"invalid sanitize action: {action}")
        if action != "discard" and not isinstance(cleaned, str):
            raise InvalidStateError("sanitize must return string content unless discarded")
        return cleaned or "", action

    def add(
        self,
        content: str,
        kind: str = "note",
        trigger_desc: str | None = None,
        anti_trigger_desc: str | None = None,
        source: str = "chat",
        skill_name: str | None = None,
    ) -> str:
        """写入外部确认的知识."""
        if kind not in ("note", "skill"):
            raise InvalidStateError(f"invalid kind: {kind}")
        if source not in ("chat", "manual", "doc", "agent"):
            raise InvalidStateError(f"invalid source: {source}")
        if kind == "skill":
            try:
                skill_path = Path(content)
                if skill_path.is_file():
                    content = skill_path.read_text(encoding="utf-8")
                    skill_name = skill_name or skill_path.stem
            except OSError:
                pass
        content, action = self._sanitize_content(content)
        if action == "discard":
            return ""  # §二·六 不写 chunk

        h = content_hash(content)
        if self.storage.is_invalidated(h):
            raise InvalidStateError("content hash is invalidated")

        # §六·五 幂等性: install 靠 content_hash 应用层去重.
        # 同一 hash 已有非 archived knowledge chunk → 返回已有 id, 不重复写入.
        # spark 是待孵化灵感,不能阻止同内容知识被正式捕获.
        existing = self.storage.conn.execute(
            "SELECT id FROM chunks WHERE content_hash=? AND origin!='spark' "
            "AND state IN ('active','pending') "
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
        sanitized = action == "redact"
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
            "skill_name": skill_name,
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

        if embed_ok:
            try:
                self.storage.insert_chunk_with_vectors(chunk, cvec, tvec)
            except Exception:
                chunk["embed_version"] = 0
                chunk["state_reason"] = f"embedding_pending:target={state}"
                self.storage.insert_chunk(chunk)
        else:
            self.storage.insert_chunk(chunk)
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
        content, action = self._sanitize_content(content)
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
            "state": "active",  # 统一 schema 要求 state;spark 生命周期由 maturity 表达
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

        if embed_ok:
            try:
                self.storage.insert_chunk_with_vectors(chunk, cvec, tvec)
            except Exception:
                chunk["embed_version"] = 0
                chunk["state_reason"] = "embedding_pending:target=active"
                self.storage.insert_chunk(chunk)
        else:
            self.storage.insert_chunk(chunk)
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

        content, action = self._sanitize_content(spark["content"])
        if action == "discard":
            # §二·六 拒绝晋升, maturity 保持原状(incubating/seed)
            raise InvalidStateError("sanitize discard on promote")

        promoted_hash = content_hash(content)
        if (
            self.storage.is_invalidated(spark["content_hash"])
            or self.storage.is_invalidated(promoted_hash)
        ):
            raise InvalidStateError("spark content hash is invalidated")

        now = utc_now_iso()
        existing = self.storage.conn.execute(
            """SELECT id FROM chunks
               WHERE content_hash=? AND origin!='spark'
                 AND state IN ('active','pending')
               ORDER BY created_at ASC LIMIT 1""",
            (promoted_hash,),
        ).fetchone()
        if existing:
            self.storage.conn.execute(
                "UPDATE chunks SET maturity='promoted', updated_at=? WHERE id=?",
                (now, spark_id),
            )
            self.storage.conn.commit()
            return existing["id"]

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
        if action == "redact":
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

        if embed_ok:
            try:
                self.storage.insert_chunk_with_vectors(chunk, cvec, tvec)
            except Exception:
                chunk["embed_version"] = 0
                chunk["state_reason"] = f"embedding_pending:target={state}"
                self.storage.insert_chunk(chunk)
        else:
            self.storage.insert_chunk(chunk)

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

    def mature_spark(self, spark_id: str, to: str) -> None:
        """人工推进 spark 孵化阶段;只允许前向转换."""
        spark = self.storage.get_chunk(spark_id)
        if not spark or spark.get("origin") != "spark":
            raise ChunkNotFoundError(f"spark {spark_id} not found")
        current = spark.get("maturity") or "seed"
        transitions = {
            "seed": {"sprouting"},
            "sprouting": {"incubating"},
            "incubating": set(),
        }
        if current == to:
            return
        if current not in transitions:
            raise InvalidStateError(f"spark {spark_id} already {current}")
        if to not in transitions[current]:
            raise InvalidStateError(f"invalid spark maturity transition: {current} -> {to}")
        self.storage.conn.execute(
            "UPDATE chunks SET maturity=?, updated_at=? WHERE id=?",
            (to, utc_now_iso(), spark_id),
        )
        self.storage.conn.commit()

    # ------------------------------------------------------------------
    # Public API: approve / archive / invalidate / restore
    # ------------------------------------------------------------------
    def approve(self, chunk_id: str) -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        if chunk.get("origin") == "spark":
            raise InvalidStateError("spark lifecycle uses promote_spark() or invalidate()")
        if chunk.get("state") == "active":
            return
        if chunk.get("state") != "pending":
            raise InvalidStateError("approve requires pending chunk")
        self.storage.update_chunk_state(chunk_id, "active", "approved")
        self.storage.conn.execute(
            "UPDATE chunks SET confidence_reason='manual_set', updated_at=? WHERE id=?",
            (utc_now_iso(), chunk_id),
        )
        self.storage.conn.commit()

    def archive(self, chunk_id: str, reason: str = "stale") -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        if chunk.get("origin") == "spark":
            raise InvalidStateError("spark lifecycle uses drop_spark() or invalidate()")
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
            """UPDATE chunks
               SET state='archived', confidence=0.0, state_reason=?,
                   state_updated_at=?, updated_at=?
               WHERE id=?""",
            (f"invalidated:{reason}" if reason else "invalidated", now, now, chunk_id),
        )
        # 2. 同 hash 连带
        self.storage.conn.execute(
            """UPDATE chunks
               SET state='archived', confidence=0.0, state_reason=?,
                   state_updated_at=?, updated_at=?
               WHERE content_hash=? AND id!=?""",
            ("invalidated:same_hash", now, now, h, chunk_id),
        )
        # 3. 重入黑名单
        self.storage.insert_invalidated_hash(h, reason, now)
        self.storage.conn.commit()

    def restore(self, chunk_id: str) -> None:
        chunk = self.storage.get_chunk(chunk_id)
        if not chunk:
            raise ChunkNotFoundError(chunk_id)
        if chunk.get("state") == "active":
            return
        if chunk.get("state") != "archived":
            raise InvalidStateError("restore requires archived chunk")
        self.storage.update_chunk_state(chunk_id, "active", "restore")
        self.storage.conn.execute(
            "UPDATE chunks SET confidence_reason='restore', updated_at=? WHERE id=?",
            (utc_now_iso(), chunk_id),
        )
        self.storage.conn.commit()

    # ------------------------------------------------------------------
    # Public API: evolve
    # ------------------------------------------------------------------
    def evolve(self, trigger: str = "manual") -> Dict[str, Any]:
        """成长:distill + curate + aggregate + purge."""
        if trigger not in self.EVOLVE_TRIGGERS:
            raise InvalidStateError(f"invalid evolve trigger: {trigger}")
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
                # 出生版使用轻量 token 估算;注入 LLM Distiller 时仍可复用.
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
        if not self.distiller.screen(log):
            self._distill_finalize(log, "discarded", "screened_out")
            return None

        result = self.distiller.distill(log, self.embedding)
        if result is None:
            return None

        required = {"content", "trigger_desc", "anti_trigger_desc"}
        if not required.issubset(result):
            missing = ", ".join(sorted(required - set(result)))
            raise InvalidStateError(f"distiller result missing fields: {missing}")

        content = result["content"]
        if not content:
            return None

        content, action = self._sanitize_content(content)
        if action == "discard":
            # 通知调用方:这是 sanitize_discard 终态
            self._distill_finalize(log, "discarded", "sanitize_discard")
            return None

        h = content_hash(content)
        if self.storage.is_invalidated(h):
            self._distill_finalize(log, "discarded", "invalidated_hash")
            return None

        # §六·五 幂等性: distilled_from UNIQUE 索引保证重跑不重复
        existing = self.storage.conn.execute(
            "SELECT id FROM chunks WHERE distilled_from=?", (log["id"],),
        ).fetchone()
        if existing:
            return existing["id"]

        now = utc_now_iso()
        chunk_id = gen_uuid()
        trigger = result["trigger_desc"]
        anti = result["anti_trigger_desc"]

        # §二·六 distill() redact 路径: 脱敏写 pending, conf ≤ 0.4
        sanitized = action == "redact"
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

        try:
            self.storage.insert_chunk_with_vectors(chunk, cvec, tvec)
        except Exception:
            self._distill_finalize(log, "failed", "embedding_failed")
            return None
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
            # 1. aggregate + watermark + raw trace purge 必须原子提交.
            # 半开窗口配合 BEGIN IMMEDIATE:等于 cutoff 的 trace 留给下一轮.
            last_ts = self.storage.get_meta("last_agg_ts") or "1970-01-01T00:00:00.000Z"
            self.storage.begin_immediate()
            try:
                self.storage.aggregate_success_traces(last_ts, cutoff_ts)
                self.storage.aggregate_success_counts()
                self.storage.aggregate_counters(last_ts, cutoff_ts)
                self.storage.set_meta("last_agg_ts", cutoff_ts, commit=False)
                purged = self.storage.purge_usage_trace(cutoff_ts)
                self.storage.commit()
            except Exception:
                self.storage.rollback()
                raise
            report.stats["purged_traces"] = purged

            # 2. purge_logs 前置: stale screening + open TTL
            stale = self.storage.purge_stale_screening(self.screening_timeout_minutes)
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

        # 7. cycle / orphan 仅检测,不自动改写依赖图.
        self._curate_cycles(report, scope)

        if not scope.dry_run:
            # 8. purge old episodic_log
            old_logs = self.storage.purge_old_logs(30)
            report.stats["purged_logs"] = old_logs

            self.storage.conn.commit()
        report.stats.setdefault("archived_count", len(report.archived))
        report.stats.setdefault("deduped_count", len(report.deduped))
        report.stats.setdefault("decayed_count", len(report.decayed))
        report.stats.setdefault("promoted_count", 0)
        report.stats.setdefault("cycle_count", len(report.cycles))
        report.stats.setdefault("orphan_count", len(report.orphans))
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
               FROM chunks WHERE state IN ('active', 'pending') AND origin!='spark'"""
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
                        row["id"], "archived", f"duplicate:{canonical}", commit=False
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
                        self.storage.update_chunk_state(cid, "archived", "low_confidence", commit=False)
                    report.archived.append(cid)
                    continue

            # repeated_selected_unused
            if selected_cnt >= self.repeat_select_min and used_cnt == 0 and conf < self.repeat_select_conf_max:
                if not scope.dry_run:
                    self.storage.update_chunk_state(cid, "archived", "repeated_selected_unused", commit=False)
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
                        self.storage.update_chunk_state(cid, "archived", "never_used", commit=False)
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
                    self.storage.update_chunk_state(row["id"], "active", "repeated_success", commit=False)
                report.stats["promoted_count"] = report.stats.get("promoted_count", 0) + 1

    def _curate_cycles(self, report: CurateReport, scope: CurateScope) -> None:
        # hard dependency 图环检测:保留遍历栈,报告每个回边形成的环.
        deps_rows = self.storage.conn.execute("SELECT src, dst FROM deps WHERE kind='hard'").fetchall()
        graph: Dict[str, List[str]] = {}
        for r in deps_rows:
            graph.setdefault(r["src"], []).append(r["dst"])
        visited = set()
        on_stack = set()

        def dfs(node: str, path: List[str]) -> None:
            visited.add(node)
            on_stack.add(node)
            for nxt in graph.get(node, []):
                if nxt not in visited:
                    dfs(nxt, path + [nxt])
                elif nxt in on_stack:
                    cycle_start = path.index(nxt)
                    cycle = path[cycle_start:] + [nxt]
                    if cycle not in report.cycles:
                        report.cycles.append(cycle)
            on_stack.remove(node)

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
        # - distilled_from: 由召回中选中过该 chunk 的 episodic_log 派生的 chunk
        related_parent = self.storage.conn.execute(
            "SELECT id, state, confidence, 'parent_id' AS via FROM chunks WHERE parent_id=?",
            (chunk_id,),
        ).fetchall()
        related_distilled = self.storage.conn.execute(
            """SELECT DISTINCT c.id, c.state, c.confidence, 'distilled_from' AS via
               FROM chunks c
               JOIN episodic_log l ON l.id = c.distilled_from
               JOIN json_each(l.recall_snapshot, '$.selected') selected
               WHERE selected.value=?""",
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

        # 新写入 captured note 默认 0.60;超过缓冲期仍卡在中间才算僵尸块.
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
                 AND distill_locked_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)""",
            (f"-{self.screening_timeout_minutes} minutes",),
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
            "stale_screening_count": stale,
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
            parameters = inspect.signature(func).parameters
            context_name = (
                "context"
                if "context" in parameters
                else "_innate_context"
                if "_innate_context" in parameters
                else None
            )

            @functools.wraps(func)
            def wrapper(*args, **kwargs):
                query = args[0] if args else kwargs.get("query", "")
                if not query:
                    return func(*args, **kwargs)
                ctx = self.recall(query, budget=budget, trace=True, source="augmented")
                if context_name and context_name not in kwargs:
                    kwargs[context_name] = ctx
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
                self.storage.replace_vectors(row["id"], cvec, tvec)
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
