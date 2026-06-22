# 架构约束

## 单二进制 + lib 分层

项目产出单一二进制 innate（main.rs）和库 innate_core（lib.rs）；CLI、MCP、Web、Daemon 均为 lib 模块的薄封装层。

---

## 可插拔 Provider 架构

KnowledgeBase 通过 Arc<dyn Trait> 注入五个扩展点：EmbeddingProvider、Refiner、Distiller、Curator、Sanitizer；open_with() 接受 Option，缺省回退到 Dummy/Heuristic/Null 实现。

---

## 三层知识模型

系统管理程序性知识，分 Memory（经验蒸馏+置信EMA+时间衰减）、Skill（可安装 kind=skill chunk）、Intuition（appraise 评审）三层协作；chunk origin 限定为 installed/distilled/captured/spark。

---

## 韧性蒸馏管线

ResilientDistiller 包装 LLM Distiller + HeuristicDistiller，LLM 优先尝试 2 次后回退确定性蒸馏，确保知识创建不依赖 LLM 可用性。

---

## SDK 架构

Python/TypeScript SDK 均通过 subprocess 调用 innate 二进制（CLI 模式），TypeScript 额外支持 MCP stdio 客户端模式；SDK 不直接操作数据库。
