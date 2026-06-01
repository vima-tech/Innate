"""EmbeddingProvider 扩展点."""

from __future__ import annotations

import hashlib
import random
from abc import ABC, abstractmethod
from typing import List


class EmbeddingProvider(ABC):
    """嵌入模型抽象."""

    @property
    @abstractmethod
    def content_dim(self) -> int:
        """content_vec 维度."""
        ...

    @property
    @abstractmethod
    def trigger_dim(self) -> int:
        """trigger_vec 维度."""
        ...

    @abstractmethod
    def embed_content(self, text: str) -> List[float]:
        """编码 content."""
        ...

    @abstractmethod
    def embed_trigger(self, text: str) -> List[float]:
        """编码 trigger_desc."""
        ...


class DummyEmbeddingProvider(EmbeddingProvider):
    """占位实现:生成随机向量(仅用于测试/无网络环境)."""

    def __init__(self, content_dim: int = 1024, trigger_dim: int = 256, seed: int = 42):
        self._content_dim = content_dim
        self._trigger_dim = trigger_dim
        self._seed = seed

    @property
    def content_dim(self) -> int:
        return self._content_dim

    @property
    def trigger_dim(self) -> int:
        return self._trigger_dim

    def _vec(self, text: str, dim: int) -> List[float]:
        digest = hashlib.sha256(f"{self._seed}:{text}".encode("utf-8")).digest()
        rng = random.Random(int.from_bytes(digest[:8], "big"))
        vec = [rng.uniform(-1.0, 1.0) for _ in range(dim)]
        norm = sum(x * x for x in vec) ** 0.5
        if norm == 0:
            norm = 1.0
        return [x / norm for x in vec]

    def embed_content(self, text: str) -> List[float]:
        return self._vec(text, self._content_dim)

    def embed_trigger(self, text: str) -> List[float]:
        return self._vec(text, self._trigger_dim)
