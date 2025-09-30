<center>

# CodexUsage（支持 Codex 与 Claude Code）

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](LICENSE)

</center>

用 Rust 实现的遥测日志解析器：读取 Codex 与 Claude Code 生成的 JSONL 事件，输出汇总的 CodeAnalysis JSON，并可选择生成调试文件。

其他语言: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## 功能

- 解析来自 Claude Code 与 Codex 的 JSONL 事件
- 自动判别来源并规范化路径，避免重复统计
- 汇总 Read/Write/Edit/Command、工具调用次数与对话 token 使用量
- 输出单条 CodeAnalysis（JSON）与可选的调试文件

聚焦数据提取、统计与文件处理；结果传输（如 SendAnalysisData）不在本项目范围。

## 快速开始

前置：Rust 工具链（rustup），Docker 可选

```bash
make fmt            # 格式化 + clippy
make test           # 测试（详细输出）
make build          # 构建
make build-release  # 发布构建（release）
make run            # 运行 release 二进制
make package        # 生成 .crate 包
```

## Docker

```bash
docker build -f docker/Dockerfile --target prod -t ghcr.io/<owner>/<repo>:latest .
docker run --rm ghcr.io/<owner>/<repo>:latest
```

二进制镜像标签：
```bash
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .
docker run --rm codex_usage:latest
```

## 命名

- crate/二进制：`codex_usage`
- 仓库链接：`https://github.com/<owner>/codex-usage`
- CI 已固定使用 `codex_usage` 作为二进制名称，避免与 repo 名称绑定

## 许可证

MIT — 见 `LICENSE`。
