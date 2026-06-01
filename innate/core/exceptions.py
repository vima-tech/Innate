"""Innate 异常体系。"""


class InnateError(Exception):
    """基类。"""
    pass


class EmbeddingUnavailable(InnateError):
    """embed 服务不可用(recall 时)。"""
    pass


class OutcomeConflictError(InnateError):
    """record() 传入不同 outcome,与已有 outcome 冲突。"""
    pass


class ChunkNotFoundError(InnateError):
    """指定 chunk_id 不存在。"""
    pass


class InvalidStateError(InnateError):
    """状态转换非法。"""
    pass
