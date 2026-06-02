"""基础工具函数 — 全库统一入口,禁止业务层散落 datetime/time 调用."""

from __future__ import annotations

import hashlib
import re
import uuid
from datetime import datetime, timezone


def utc_now_iso() -> str:
    """全库统一时间生成函数。输出毫秒精度 ISO 8601 UTC。
    禁止在业务层直接调用 datetime.utcnow()、time.time() 或 SQLite datetime('now')。
    """
    dt = datetime.now(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%S.") + f"{dt.microsecond // 1000:03d}Z"


def gen_uuid() -> str:
    """生成标准 UUID4 字符串。"""
    return str(uuid.uuid4())


def content_hash(content: str) -> str:
    """计算内容哈希,用于去重。"""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:32]


# ---------------------------------------------------------------------------
# 默认 sanitize 正则规则（零重依赖）
# ---------------------------------------------------------------------------
_SECRET_PATTERNS = [
    re.compile(r"sk-[A-Za-z0-9]{20,}"),          # OpenAI/Anthropic API key
    re.compile(r"AKIA[0-9A-Z]{16}"),              # AWS access key
    re.compile(r"ghp_[A-Za-z0-9]{36}"),           # GitHub personal access token
    re.compile(r"(?i)bearer\s+[A-Za-z0-9\-._~+/]+=*"),  # Bearer token
    re.compile(r"(?i)password\s*[:=]\s*\S+"),     # password: xxx
]

_INJECTION_PATTERNS = [
    re.compile(r"(?i)ignore\s+(all\s+)?previous\s+instructions?"),
    re.compile(r"(?i)system\s*prompt\s*[:：]"),
    re.compile(r"(?i)you\s+are\s+now\s+(?:a\s+)?(?:different|new)"),
]


def default_sanitize(content: str | None) -> tuple[str | None, str]:
    """默认 sanitize: 极轻正则,零依赖。
    返回 (cleaned_content, action), action ∈ {"allow", "redact", "discard"}
    """
    if content is None:
        return None, "allow"

    for pat in _INJECTION_PATTERNS:
        if pat.search(content):
            return content, "discard"

    cleaned = content
    redacted = False
    for pat in _SECRET_PATTERNS:
        cleaned, count = pat.subn("[REDACTED]", cleaned)
        redacted = redacted or count > 0

    return cleaned, "redact" if redacted else "allow"


def estimate_tokens(text: str | None) -> int | None:
    """粗略估算 token 数(约 4 字符/token)。None → None。"""
    if text is None:
        return None
    return max(1, len(text) // 4)
