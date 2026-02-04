<center>

# Vibe Coding Tracker — AI 编程助手使用量追踪器

[![Crates.io](https://img.shields.io/crates/v/vibe_coding_tracker?logo=rust&style=flat-square&color=E05D44)](https://crates.io/crates/vibe_coding_tracker)
[![Crates.io Downloads](https://img.shields.io/crates/d/vibe_coding_tracker?logo=rust&style=flat-square)](https://crates.io/crates/vibe_coding_tracker)
[![npm version](https://img.shields.io/npm/v/vibe-coding-tracker?logo=npm&style=flat-square&color=CB3837)](https://www.npmjs.com/package/vibe-coding-tracker)
[![npm downloads](https://img.shields.io/npm/dt/vibe-coding-tracker?logo=npm&style=flat-square)](https://www.npmjs.com/package/vibe-coding-tracker)
[![PyPI version](https://img.shields.io/pypi/v/vibe_coding_tracker?logo=python&style=flat-square&color=3776AB)](https://pypi.org/project/vibe_coding_tracker/)
[![PyPI downloads](https://img.shields.io/pypi/dm/vibe_coding_tracker?logo=python&style=flat-square)](https://pypi.org/project/vibe-coding-tracker)
[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white&style=flat-square)](https://www.rust-lang.org/)
[![tests](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/test.yml?label=tests&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/test.yml)
[![code-quality](https://img.shields.io/github/actions/workflow/status/Mai0313/VibeCodingTracker/code-quality-check.yml?label=code-quality&logo=github&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray&style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/tree/master?tab=License-1-ov-file)
[![Star on GitHub](https://img.shields.io/github/stars/Mai0313/VibeCodingTracker?style=social&label=Star)](https://github.com/Mai0313/VibeCodingTracker)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</center>

**实时追踪您的 AI 编程成本。** Vibe Coding Tracker 是一个强大的 CLI 工具，帮助您监控和分析 Claude Code、Codex、Copilot 和 Gemini 的使用情况，提供详细的成本分解、token 统计和代码操作洞察。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：以下 CLI 示例默认使用短别名 `vct`。若你是从源码构建，产生的二进制文件名称为 `vibe_coding_tracker`，可以自行建立别名，或在执行指令时将 `vct` 换成完整名称。

---

## 🎯 为什么选择 Vibe Coding Tracker？

### 💰 了解您的成本

不再疑惑您的 AI 编程会话花费多少。通过 [LiteLLM](https://github.com/BerriAI/litellm) 自动更新定价，获取**实时成本追踪**。

### 📊 精美的可视化

选择您偏好的视图：

- **交互式仪表板**：自动刷新的终端 UI，实时更新
- **静态报表**：专业的表格，适合文档
- **脚本友好**：纯文本和 JSON，便于自动化
- **全精度**：导出精确成本，用于财务核算

### 🚀 零配置

自动检测并处理 Claude Code、Codex、Copilot 和 Gemini 的日志。无需设置——只需运行和分析。

### 🎨 丰富的洞察

- 按模型和日期的 token 使用量
- 按缓存类型的成本分解
- 文件操作追踪
- 命令执行历史
- Git 仓库信息

---

## ✨ 核心特性

| 特性                | 描述                                                |
| ------------------- | --------------------------------------------------- |
| 🤖 **自动检测**     | 智能识别 Claude Code、Codex、Copilot 或 Gemini 日志 |
| 💵 **智能定价**     | 模糊模型匹配 + 每日缓存以提高速度                   |
| 🎨 **4 种显示模式** | 交互式、表格、文本和 JSON 输出                      |
| 📈 **全面统计**     | Token、成本、文件操作和工具调用                     |
| ⚡ **高性能**       | 使用 Rust 构建，速度快且可靠                        |
| 🔄 **实时更新**     | 仪表板每秒刷新                                      |
| 💾 **高效缓存**     | 智能的每日缓存减少 API 调用                         |

---

## 🚀 快速开始

### 安装

选择最适合您的安装方式：

#### 方式 1: 从源码编译 (推荐开发者 ✨)

适合想要自定义构建或贡献开发的用户：

```bash
# 1. 克隆仓库
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker

# 2. 构建 release 版本
cargo build --release

# 3. 二进制文件位置
./target/release/vibe_coding_tracker

# 4. （可选）建立短别名
# Linux/macOS:
sudo ln -sf "$(pwd)/target/release/vibe_coding_tracker" /usr/local/bin/vct

# 或安装到用户目录:
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct
# 确保 ~/.local/bin 在您的 PATH 中
```

**前置条件**: [Rust 工具链](https://rustup.rs/) 1.85 或更高版本

> **注意**: 此项目使用 **Rust 2024 edition**，需要 Rust 1.85+。如需更新，请执行 `rustup update`。

#### 方式 2: 从 crates.io 安装

使用 Cargo 从 Rust 官方包注册表安装：

```bash
cargo install vibe_coding_tracker
```

#### 方式 3: 从 npm 安装

**前置条件**: [Node.js](https://nodejs.org/) v22 或更高版本

选择以下任一包名称（三者完全相同）：

```bash
# 主要包
npm install -g vibe-coding-tracker

# 带 scope 的短别名
npm install -g @mai0313/vct

# 带 scope 的完整名称
npm install -g @mai0313/vibe-coding-tracker
```

#### 方式 4: 从 PyPI 安装

**前置条件**: Python 3.8 或更高版本

```bash
pip install vibe_coding_tracker
# 或使用 uv
uv pip install vibe_coding_tracker
```

### 首次运行

```bash
# 使用交互式仪表板查看使用量（已设置短别名时）
vct usage

# 或使用完整名称
./target/release/vibe_coding_tracker usage

# 分析特定对话
./target/release/vibe_coding_tracker analysis --path ~/.claude/projects/session.jsonl
```

> 💡 **提示**：使用 `vct` 作为 `vibe_coding_tracker` 的短别名，节省输入时间——可通过 `ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct` 手动建立。

---

## 📖 命令指南

### 🔍 快速参考

```bash
vct <命令> [选项]
# 若未设置别名，请改用 `vibe_coding_tracker` 完整二进制名称

命令：
analysis    分析对话文件并导出数据（支持单文件或所有会话）
usage       显示 token 使用量统计
version     显示版本信息
update      从 GitHub releases 更新到最新版本
help        显示此信息或给定子命令的说明
```

---

## 💰 Usage 命令

**追踪您所有 AI 编程会话的支出。**

### 基本用法

```bash
# 交互式仪表板（推荐）
vct usage

# 静态表格，适合报表
vct usage --table

# 纯文本，适合脚本
vct usage --text

# JSON，适合数据处理
vct usage --json
```

### 您将获得什么

该工具自动扫描这些目录：

- `~/.claude/projects/*.jsonl`（Claude Code）
- `~/.codex/sessions/*.jsonl`（Codex）
- `~/.copilot/history-session-state/*.json`（Copilot）
- `~/.gemini/tmp/<project_hash>/chats/*.json`（Gemini）

---

## 📊 Analysis 命令

**深入了解对话文件 - 单文件或批量分析。**

### 基本用法

```bash
# 单文件：分析并显示
vct analysis --path ~/.claude/projects/session.jsonl

# 单文件：保存到文件
vct analysis --path ~/.claude/projects/session.jsonl --output report.json

# 批量：使用交互式表格分析所有会话（默认）
vct analysis

# 批量：静态表格输出并显示每日平均
vct analysis --table

# 批量：将汇总结果保存为 JSON
vct analysis --output batch_report.json

# 批量并依提供者分组：输出完整的 records，依提供者分组（JSON 格式）
vct analysis --all

# 将分组结果保存到文件
vct analysis --all --output grouped_report.json
```

---

## 🔄 Update 命令

**自动保持安装版本为最新。**

update 命令适用于**所有安装方式**（npm/pip/cargo/manual），直接从 GitHub releases 下载并替换二进制文件。

### 基本用法

```bash
# 检查更新
vct update --check

# 交互式更新（会询问确认）
vct update

# 强制更新 - 总是下载最新版本（即使已是最新版本）
vct update --force
```

---

## 💡 智能定价系统

### 运作原理

1. **自动更新**：每天从 [LiteLLM](https://github.com/BerriAI/litellm) 获取定价
2. **智能缓存**：在 `~/.vibe_coding_tracker/` 中存储定价 24 小时
3. **模糊匹配**：即使对于自定义模型名称也能找到最佳匹配
4. **始终准确**：确保您获取最新的定价

### 模型匹配

**优先顺序**：

1. ✅ **精确匹配**：`claude-sonnet-4` → `claude-sonnet-4`
2. 🔄 **规范化**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. 🔍 **子字符串**：`custom-gpt-4` → `gpt-4`
4. 🎯 **模糊（AI 驱动）**：使用 Jaro-Winkler 相似度（70% 阈值）
5. 💵 **后备**：如果找不到匹配则显示 $0.00

---

## 🐳 Docker 支持

```bash
# 构建镜像
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# 使用您的会话运行
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.copilot:/root/.copilot \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```
