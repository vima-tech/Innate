# 编码规范

## 错误处理规范

统一使用 thiserror 定义 InnateError 枚举，模块级 pub type Result<T> = std::result::Result<T, InnateError>；外部错误通过 #[from] 自动转换，业务错误用 Other(String) 或专用变体。

---

## 纯函数可测性

IO 与逻辑分离：Web 路由 route() 为纯函数（无 socket），handle() 仅做 tiny_http 胶水；便于无网络单元测试。

---

## 常量调参约定

召回权重、阈值等调参默认值定义为模块级 const（如 W_CONTENT=0.55），KnowledgeBase 初始化时从 meta 表加载覆盖值，运行时可通过 settings 调整。

---

## 向量维度守卫

所有向量写入必须通过 store_vec_content/store_vec_trigger，在写入前校验维度与 EmbeddingProvider 配置一致，不匹配则 fail-closed 返回 InvalidState 错误。

---

## Schema 迁移

数据库迁移使用增量 SQL 文件（如 4.13_to_4.14.sql），由 migrate.rs 按 schema_version 顺序执行；新迁移必须向后兼容且可重复执行（IF NOT EXISTS）。
