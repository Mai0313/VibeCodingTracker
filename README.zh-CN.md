<center>

# CodexUsage（支持 Codex 与 Claude Code）

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/CodexUsage/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/CodexUsage/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/CodexUsage/pulls)

</center>

用 Rust 实现的遥测日志解析器：读取 Codex 与 Claude Code 生成的 JSONL 事件，输出汇总的 CodeAnalysis JSON，并可选择生成调试文件。

其他语言: [English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

## 功能

本项目是将原本的 Go 语言实现 (`parser.go`) 完整翻译成 Rust 的项目，主要功能是解析和分析 Claude Code 和 Codex 的 JSONL 日志文件。

- 解析来自 Claude Code 与 Codex 的 JSONL 事件
- 自动判别来源并规范化路径，避免重复统计
- 汇总 Read/Write/Edit/Command、工具调用次数与对话 token 使用量
- 输出单条 CodeAnalysis（JSON）与可选的调试文件

聚焦数据提取、统计与文件处理；结果传输（如 SendAnalysisData）不在本项目范围。

## 特色功能

1. **自动检测**: 自动识别 Claude Code 或 Codex 日志格式
2. **完整统计**: 包含文件操作、工具调用、token 使用量等详细统计
3. **美观输出**: 使用量统计提供格式化的表格显示，附千位分隔符
4. **健全错误处理**: 使用 Rust 的类型系统提供可靠的错误管理
5. **性能优化**: Release 构建包含 LTO 和符号剥离优化

## 快速开始

前置：Rust 工具链（rustup），Docker 可选

```bash
# 构建项目
make fmt            # 格式化 + clippy
make test           # 测试（详细输出）
make build          # 构建
make build-release  # 发布构建（release）
make package        # 生成 .crate 包
```

## CLI 使用方式

### 分析命令

分析 JSONL 对话文件并获取详细统计：

```bash
# 分析并输出到标准输出
codex_usage analysis --path examples/test_conversation.jsonl

# 分析并保存到文件
codex_usage analysis --path examples/test_conversation.jsonl --output result.json

# 分析 Codex 日志
codex_usage analysis --path examples/test_conversation_oai.jsonl
```

### 使用量命令

显示 Claude Code 和 Codex 会话的 token 使用统计：

```bash
# 以表格格式显示使用量
codex_usage usage

# 以 JSON 格式显示使用量
codex_usage usage --json
```

### 版本命令

显示版本信息：

```bash
codex_usage version
```

## 项目结构

```
codex_usage/
├── src/
│   ├── lib.rs              # 函数库主文件
│   ├── main.rs             # CLI 入口点
│   ├── cli.rs              # CLI 参数解析
│   ├── models/             # 数据模型
│   │   ├── mod.rs
│   │   ├── analysis.rs     # 分析数据结构
│   │   ├── usage.rs        # 使用量数据结构
│   │   ├── claude.rs       # Claude Code 日志模型
│   │   └── codex.rs        # Codex 日志模型
│   ├── analysis/           # 分析功能
│   │   ├── mod.rs
│   │   ├── analyzer.rs     # 主分析器
│   │   ├── claude_analyzer.rs  # Claude Code 分析器
│   │   ├── codex_analyzer.rs   # Codex 分析器
│   │   └── detector.rs     # 扩展类型检测
│   ├── usage/              # 使用量统计
│   │   ├── mod.rs
│   │   ├── calculator.rs   # 使用量计算
│   │   └── display.rs      # 使用量显示格式化
│   └── utils/              # 工具函数
│       ├── mod.rs
│       ├── paths.rs        # 路径处理
│       ├── time.rs         # 时间解析
│       ├── file.rs         # 文件 I/O
│       └── git.rs          # Git 操作
├── examples/               # 示例 JSONL 文件
├── tests/                  # 集成测试
└── parser.go              # 原始 Go 实现（参考用）
```

## 主要依赖

- **CLI**: clap (v4.5) - 命令行参数解析
- **序列化**: serde, serde_json - JSON 处理
- **错误处理**: anyhow, thiserror - 健全的错误管理
- **时间**: chrono - 时间戳解析
- **文件系统**: walkdir, home - 目录遍历和路径解析
- **正则表达式**: regex - 日志解析中的模式匹配
- **日志**: log, env_logger - 调试输出

## Go 到 Rust 的对应

| Go 功能 | Rust 实现 | 说明 |
|---------|-----------|------|
| `analyzeConversations` | `analysis::claude_analyzer::analyze_claude_conversations` | Claude Code 分析 |
| `analyzeCodexConversations` | `analysis::codex_analyzer::analyze_codex_conversations` | Codex 分析 |
| `CalculateUsageFromJSONL` | `usage::calculator::calculate_usage_from_jsonl` | 单文件使用量计算 |
| `GetUsageFromDirectories` | `usage::calculator::get_usage_from_directories` | 目录使用量统计 |
| `ReadJSONL` | `utils::file::read_jsonl` | JSONL 文件读取 |
| `parseISOTimestamp` | `utils::time::parse_iso_timestamp` | 时间戳解析 |
| `getGitRemoteOriginURL` | `utils::git::get_git_remote_url` | Git 远程 URL 提取 |

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
