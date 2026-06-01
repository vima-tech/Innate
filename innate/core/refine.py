"""可替换扩展点: Refiner / Distiller (默认实现)."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any, Dict, List, Optional


class Refiner(ABC):
    """在线精炼器(默认关闭,允许 allow_trim 时启用)."""

    @property
    def available(self) -> bool:
        """是否有可用的精炼实现."""
        return False

    @abstractmethod
    def refine(
        self,
        blocks: List[Dict[str, Any]],
        query: str,
        mode: str,
    ) -> List[Dict[str, Any]]:
        """返回精炼后的 blocks. mode ∈ {'trim', 'adapt'}.

        设计约束(§二·五A 'trim 不可破坏 hard 闭包'):
        精炼只能裁剪块内部非关键段落,不能删除整个 hard dependency,
        不能把硬依赖降级为可选. 若精炼后不满足 hard 闭包,返回原 blocks.
        """
        ...


class NullRefiner(Refiner):
    """默认 no-op refiner. 装包时不会触发 trim 路径."""

    @property
    def available(self) -> bool:
        return False

    def refine(
        self,
        blocks: List[Dict[str, Any]],
        query: str,
        mode: str,
    ) -> List[Dict[str, Any]]:
        return blocks


class Distiller(ABC):
    """蒸馏器(从 episodic_log 提炼 chunk dict)."""

    def screen(self, log: Dict[str, Any]) -> bool:
        """廉价初筛:False 表示原料不值得进入提炼步骤."""
        return True

    @abstractmethod
    def distill(
        self,
        log: Dict[str, Any],
        embedder: Any,
    ) -> Optional[Dict[str, Any]]:
        """返回 chunk 字段 dict (content/trigger_desc/anti_trigger_desc) 或 None.

        返回 None 表示该 log 不应蒸馏(调用方会标 'insufficient_material').
        """
        ...


class HeuristicDistiller(Distiller):
    """默认蒸馏器:启发式,无 LLM. 用 output_summary / query 直接提炼."""

    def screen(self, log: Dict[str, Any]) -> bool:
        return bool(log.get("output_summary") or log.get("query"))

    def distill(
        self,
        log: Dict[str, Any],
        embedder: Any,
    ) -> Optional[Dict[str, Any]]:
        content = log.get("output_summary") or log.get("query")
        if not content:
            return None
        trigger = (log.get("query") or "")[:200]
        return {
            "content": content,
            "trigger_desc": trigger,
            "anti_trigger_desc": "",
        }
