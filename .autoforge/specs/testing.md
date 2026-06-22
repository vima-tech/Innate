# 测试要求

## 集成测试模块

core/src/tests/ 下含 12 个测试模块（basics、distillation、eval、feedback、governance、intuition、intuition_optim、reliability、restoration 等），通过 tmp_kb() 创建临时 SQLite 文件隔离测试。其中 intuition_optim 锁定直觉模块偏差治理（弃权门 A、verdict_log B、provenance C、基率先验 D、校准映射 E、双通道 F、离散度 G）激活后的行为。

---

## Web 路由纯函数测试

web/tests.rs 直接调用 route() 纯函数，不启动 HTTP 服务器；覆盖 chunk 列表、状态过滤、inspect 开放访问、token 认证、CSRF 防御等场景。

---

## 测试辅助函数

tests/mod.rs 提供 attributed_trace()、record_down_as() 等辅助函数，封装 episodic_log 写入和 usage_trace 插入，降低测试样板代码。

---

## SDK 测试

Python SDK 使用 pytest（tests/test_client.py）；TypeScript SDK 通过 tsc 编译验证类型正确性，package.json 定义 build 脚本。

---

## Daemon 测试

daemon/tests.rs 独立测试守护进程的文件监听逻辑；dev-dependencies 仅依赖 tempfile 3 用于创建临时文件/目录。
