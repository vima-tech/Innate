-- 4.21 → 4.22 — 2026-09 调优：参数校正 + 新增调参项。
--
-- 本次迁移只动 meta（无表结构变更），但必须走迁移链：这些键在旧库里已有
-- 播种值，`KnowledgeBase::open` 的 seed 只做 INSERT OR IGNORE，不会覆盖，
-- 所以光改代码常量对存量库无效。

-- ── 修正：反噪规则的结构性死锁 ──────────────────────────────────────
-- repeated_selected_unused 归档条件是 confidence < repeat_select_conf_max，
-- 而蒸馏 chunk 的初始置信度是 0.55。从未被使用的 chunk 拿不到置信度抬升，
-- 会永久停在种子值 → 阈值 0.5 时该规则对蒸馏 chunk 永不可达。
-- 实测：满足「被选 ≥10 次、使用 0 次」的 59 条里 48 条（81%）逃过归档。
-- 只在仍是旧默认值时更新，尊重用户手工调过的值。
UPDATE meta SET value='0.60'
 WHERE key='curate.repeat_select_conf_max' AND value IN ('0.5','0.50');

-- ── 修正：蒸馏批量导致 LLM 响应被截断 ─────────────────────────────
-- batch=20 时 completion 常触顶 max_tokens，finish_reason=length，
-- 截断的 JSON 解析失败 → ResilientDistiller 静默回退到启发式蒸馏。
UPDATE meta SET value='8'
 WHERE key='evolve.distill_batch_size' AND value='20';

-- ── 新增调参项 ────────────────────────────────────────────────────
-- curate 节流：daemon 为了及时消费蒸馏请求而高频轮询，但维护性 curate
-- 不需要同样的频率。未节流时 30 天跑了 39,383 次全量 curate。
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.min_interval_minutes', '60');

-- 弱晋级通道：强晋级要求 used_success_count ≥ 2，依赖 agent 主动带 outcome
-- 收尾，实际只有极少数召回做到 → pending 堆积（459 条，最老 73 天，周晋级 2 条）。
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.weak_promote_selected_min', '20');
INSERT OR IGNORE INTO meta(key, value) VALUES ('curate.weak_promote_age_days', '14');

-- 长度归一：长 chunk 在余弦下通吃。实测 800–2k 字节的 chunk 人均被选 65.7 次
-- （300–800 字节桶为 10.9 次），使用率反而更低（2.5% vs 4.0%）。
INSERT OR IGNORE INTO meta(key, value) VALUES ('recall.length_penalty', '0.15');
INSERT OR IGNORE INTO meta(key, value) VALUES ('recall.length_penalty_free_bytes', '800');

UPDATE meta SET value='4.22' WHERE key='schema_version';
