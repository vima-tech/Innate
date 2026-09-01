# API 契约

## MCP 工具集

MCP 暴露 15 个工具：innate_recall、innate_record、innate_appraise、innate_add、innate_spark、innate_inspect、innate_evolve、innate_approve、innate_archive、innate_invalidate、innate_restore、innate_mature_spark、innate_promote_spark、innate_drop_spark、innate_backup。

---

## REST API 端点

Web API 路径前缀 /api/：GET inspect、GET chunks（支持 state/origin/limit/offset 查询）、GET governance、GET llm-traces、GET chunk/:id、POST chunk/:id/{approve|restore|archive|invalidate}、POST chunks/batch/{approve|restore|archive|invalidate}（批量治理，body `{ids:[...], reason}`，单次上限 200，逐条报告失败而不中断）、POST daemon/restart（重启/拉起守护进程，返回最新 health）。

---

## API 安全机制

非 loopback 绑定时所有 /api/ 读端点需 x-innate-token 头认证；写端点额外校验 Origin 头防 CSRF；loopback 绑定时读端点免认证以便本地 UI 浏览。

---

## CLI JSON 契约

CLI 所有子命令（recall/record/add/inspect/evolve 等）以 --format json 输出结构化 JSON 到 stdout，stderr 仅用于错误信息；SDK 通过解析 stdout JSON 获取结果。

---

## SDK 接口一致性

Python/TypeScript SDK 暴露与 CLI 子命令一一对应的方法（recall/record/add/inspect/evolve 等），参数命名与 CLI flag 保持一致，返回类型使用 dataclass/interface 强类型定义。
