# 技术栈

## Rust 核心

核心使用 Rust edition 2021 编写，包名 innate，当前版本 0.1.23；release profile 启用 LTO、codegen-units=1、strip=true 以优化二进制体积。

---

## SQLite 存储

使用 rusqlite 0.32（bundled feature）作为唯一持久化引擎，schema 版本 4.22；向量相似度由 Rust 代码直接计算，不依赖 sqlite-vec 扩展。

---

## SDK 语言与版本

Python SDK（innate-py）要求 Python ≥3.10，零运行时依赖，dev 依赖 pytest≥7；TypeScript SDK（@vima-tech/sdk）要求 Node ≥18、TypeScript ≥5，零运行时依赖。

---

## 关键依赖版本

clap 4（derive+env）、serde 1、serde_json 1、ureq 3（json feature）、tiny_http 0.12、thiserror 2、anyhow 1、uuid 1（v4）、chrono 0.4（serde）、sha2 0.11、hmac 0.13。

---

## 协议与接口

MCP 服务基于 JSON-RPC 2.0 over stdio；Web UI 使用 tiny_http 内嵌静态资源 + REST API；CLI 输出 JSON 供 SDK 解析。
