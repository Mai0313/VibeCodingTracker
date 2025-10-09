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
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg?style=flat-square)](https://github.com/Mai0313/VibeCodingTracker/pulls)

</center>

**实时追踪您的 AI 编程成本。** Vibe Coding Tracker 是一个强大的 CLI 工具，帮助您监控和分析 Claude Code、Codex 和 Gemini 的使用情况，提供详细的成本分解、token 统计和代码操作洞察。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

> 注意：文中的 CLI 示例默认使用短别名 `vct`。如果你是从源码构建的，生成的二进制名称是 `vibe_coding_tracker`，可以手动创建别名，或在执行命令时把 `vct` 替换为完整名称。

---

## 🎯 为什么选择 Vibe Coding Tracker？

### 💰 了解您的成本

不再疑惑您的 AI 编程会话花费多少。通过 [LiteLLM](https://github.com/BerriAI/litellm) 自动更新定价，获取**实时成本追踪**。

### 📊 精美的可视化

选择您喜欢的视图：

- **交互式仪表板**：自动刷新的终端 UI，实时更新
- **静态报表**：专业的表格，适合文档
- **脚本友好**：纯文本和 JSON，便于自动化
- **完整精度**：导出精确成本，用于财务核算

### 🚀 零配置

自动检测并处理 Claude Code、Codex 和 Gemini 的日志。无需设置——只需运行和分析。

### 🎨 丰富的洞察

- 按模型和日期的 token 使用量
- 按缓存类型的成本分解
- 文件操作追踪
- 命令执行历史
- Git 仓库信息

---

## ✨ 核心特性

| 特性                | 描述                                       |
| ------------------- | ------------------------------------------ |
| 🤖 **自动检测**     | 智能识别 Claude Code、Codex 或 Gemini 日志 |
| 💵 **智能定价**     | 模糊模型匹配 + 每日缓存以提高速度          |
| 🎨 **4 种显示模式** | 交互式、表格、文本和 JSON 输出             |
| 📈 **全面统计**     | Token、成本、文件操作和工具调用            |
| ⚡ **高性能**       | 使用 Rust 构建，速度快且可靠               |
| 🔄 **实时更新**     | 仪表板每秒刷新                             |
| 💾 **高效缓存**     | 智能的每日缓存减少 API 调用                |

---

## 🚀 快速开始

### 安装

选择最适合您的安装方式：

#### 方式 1: 从 npm 安装 (推荐 ✨)

**最简单的安装方式** - 包含针对您平台预编译的二进制文件，无需构建步骤！

选择以下任一包名称（三者完全相同）：

```bash
# 主要包
npm install -g vibe-coding-tracker

# 带 scope 的短别名
npm install -g @mai0313/vct

# 带 scope 的完整名称
npm install -g @mai0313/vibe-coding-tracker
```

**前提条件**: [Node.js](https://nodejs.org/) v22 或更高版本

**支持平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### 方式 2: 从 PyPI 安装

**适合 Python 用户** - 包含针对您平台预编译的二进制文件，无需构建步骤！

```bash
# 使用 pip 安装
pip install vibe_coding_tracker

# 使用 uv 安装（推荐，安装速度更快）
uv pip install vibe_coding_tracker
```

**前提条件**: Python 3.8 或更高版本

**支持平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)
- Windows (x64, ARM64)

#### 方式 3: 从 crates.io 安装

使用 Cargo 从 Rust 官方包注册表安装：

```bash
cargo install vibe_coding_tracker
```

**前提条件**: [Rust 工具链](https://rustup.rs/) 1.70 或更高版本

#### 方式 4: 从源码编译

适合希望自定义构建或贡献开发的用户：

```bash
# 1. 克隆仓库
git clone https://github.com/Mai0313/VibeCodingTracker.git
cd VibeCodingTracker

# 2. 构建 release 版本
cargo build --release

# 3. 二进制文件位置
./target/release/vibe_coding_tracker

# 4. （可选）创建短别名
# Linux/macOS:
sudo ln -sf "$(pwd)/target/release/vibe_coding_tracker" /usr/local/bin/vct

# 或安装到用户目录:
mkdir -p ~/.local/bin
ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct
# 确保 ~/.local/bin 在您的 PATH 中
```

**前提条件**: [Rust 工具链](https://rustup.rs/) 1.70 或更高版本

#### 方式 5: 通过 Curl 快速安装 (Linux/macOS)

**一行命令安装** - 自动检测您的平台并安装最新版本：

```bash
curl -fsSLk https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.sh | bash
```

**前提条件**: `curl` 和 `tar` (通常已预装)

**功能说明**:

- 自动检测您的操作系统和架构
- 从 GitHub 下载最新版本
- 解压并安装到 `/usr/local/bin` 或 `~/.local/bin`
- 自动创建 `vct` 短别名
- 跳过 SSL 验证，适用于受限网络环境

**支持平台**:

- Linux (x64, ARM64)
- macOS (x64, ARM64)

#### 方式 6: 通过 PowerShell 快速安装 (Windows)

**一行命令安装** - 自动检测您的架构并安装最新版本：

```powershell
powershell -ExecutionPolicy ByPass -c "[System.Net.ServicePointManager]::ServerCertificateValidationCallback={$true}; irm https://github.com/Mai0313/VibeCodingTracker/raw/main/scripts/install.ps1 | iex"
```

**前提条件**: PowerShell 5.0 或更高版本 (Windows 10+ 已内置)

**功能说明**:

- 自动检测您的 Windows 架构 (x64 或 ARM64)
- 从 GitHub 下载最新版本
- 安装到 `%LOCALAPPDATA%\Programs\VibeCodingTracker`
- 自动创建 `vct.exe` 短别名
- 自动加入用户 PATH
- 跳过 SSL 验证，适用于受限网络环境

**注意**: 您可能需要重启终端，PATH 更改才会生效。

**支持平台**:

- Windows 10/11 (x64, ARM64)

### 首次运行

```bash
# 使用交互式仪表板查看使用量（已配置短别名时）
vct usage

# 或使用完整名称
./target/release/vibe_coding_tracker usage

# 分析特定对话
./target/release/vibe_coding_tracker analysis --path ~/.claude/projects/session.jsonl
```

> 💡 **提示**：使用 `vct` 作为 `vibe_coding_tracker` 的短别名，节省输入时间——可以通过 `ln -sf "$(pwd)/target/release/vibe_coding_tracker" ~/.local/bin/vct` 手动创建。

---

## 📖 命令指南

### 🔍 快速参考

```bash
vct <命令> [选项]
# 如果未配置别名，请改用 `vibe_coding_tracker`

命令：
usage       显示 token 使用量和成本（默认：交互式）
analysis    分析对话文件并导出数据
version     显示版本信息
update      从 GitHub releases 更新到最新版本
help        显示帮助信息
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
- `~/.gemini/tmp/<project_hash>/chats/*.json`（Gemini）

### 🎨 交互式模式（默认）

**每秒更新的实时仪表板**

```
┌──────────────────────────────────────────────────────────────────┐
│                  📊 Token 使用统计                               │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬──────────────────────┬────────────┬────────────┬────────────┬──────────────┬────────────┬────────────┐
│ 日期       │ 模型                 │ 输入       │ 输出       │ 缓存读取   │ 缓存创建     │ 总计       │ 成本 (USD) │
├────────────┼──────────────────────┼────────────┼────────────┼────────────┼──────────────┼────────────┼────────────┤
│ 2025-10-01 │ claude-sonnet-4-20…  │ 45,230     │ 12,450     │ 230,500    │ 50,000       │ 338,180    │ $2.15      │
│ 2025-10-02 │ claude-sonnet-4-20…  │ 32,100     │ 8,920      │ 180,000    │ 30,000       │ 251,020    │ $1.58      │
│ 2025-10-03 │ claude-sonnet-4-20…  │ 28,500     │ 7,200      │ 150,000    │ 25,000       │ 210,700    │ $1.32      │
│ 2025-10-03 │ gpt-4-turbo          │ 15,000     │ 5,000      │ 0          │ 0            │ 20,000     │ $0.25      │
│            │ 总计                 │ 120,830    │ 33,570     │ 560,500    │ 105,000      │ 819,900    │ $5.30      │
└────────────┴──────────────────────┴────────────┴────────────┴────────────┴──────────────┴────────────┴────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 💰 总成本：$5.30  |  🔢 总 Token：819,900  |  📅 条目：4  |  ⚡ CPU：2.3%  |  🧠 内存：12.5 MB                      │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                            📈 每日平均                                                            │
│                                                                                                                   │
│  Claude Code: 266,667 tokens/天  |  $1.68/天                                                                     │
│  Codex: 20,000 tokens/天  |  $0.25/天                                                                            │
│  总体: 204,975 tokens/天  |  $1.33/天                                                                            │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**特性**：

- ✨ 每秒自动刷新
- 🎯 高亮今日条目
- 🔄 显示最近更新的行
- 💾 显示内存使用量
- 📊 汇总统计
- 📈 按提供者（Claude Code、Codex、Gemini）的每日平均值

**控制**：按 `q`、`Esc` 或 `Ctrl+C` 退出

### 📋 静态表格模式

**非常适合文档和报表**

```bash
vct usage --table
```

```
📊 Token 使用统计

╔════════════╦══════════════════════╦════════════╦════════════╦════════════╦══════════════╦══════════════╦════════════╗
║ 日期       ║ 模型                 ║ 输入       ║ 输出       ║ 缓存读取   ║ 缓存创建     ║ 总 Token     ║ 成本 (USD) ║
╠════════════╬══════════════════════╬════════════╬════════════╬════════════╬══════════════╬══════════════╬════════════╣
║ 2025-10-01 ║ claude-sonnet-4-20…  ║ 45,230     ║ 12,450     ║ 230,500    ║ 50,000       ║ 338,180      ║ $2.15      ║
║ 2025-10-02 ║ claude-sonnet-4-20…  ║ 32,100     ║ 8,920      ║ 180,000    ║ 30,000       ║ 251,020      ║ $1.58      ║
║ 2025-10-03 ║ claude-sonnet-4-20…  ║ 28,500     ║ 7,200      ║ 150,000    ║ 25,000       ║ 210,700      ║ $1.32      ║
║ 2025-10-03 ║ gpt-4-turbo          ║ 15,000     ║ 5,000      ║ 0          ║ 0            ║ 20,000       ║ $0.25      ║
║            ║ 总计                 ║ 120,830    ║ 33,570     ║ 560,500    ║ 105,000      ║ 819,900      ║ $5.30      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝

📈 每日平均（按提供者）

╔═════════════╦════════════════╦══════════════╦══════╗
║ 提供者      ║ 平均 Token/天  ║ 平均成本/天  ║ 天数 ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Claude Code ║ 266,667        ║ $1.68        ║ 3    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ Codex       ║ 20,000         ║ $0.25        ║ 1    ║
╠═════════════╬════════════════╬══════════════╬══════╣
║ 总体        ║ 204,975        ║ $1.33        ║ 4    ║
╚═════════════╩════════════════╩══════════════╩══════╝
```

### 📝 文本模式

**非常适合脚本和解析**

```bash
vct usage --text
```

```
2025-10-01 > claude-sonnet-4-20250514: $2.154230
2025-10-02 > claude-sonnet-4-20250514: $1.583450
2025-10-03 > claude-sonnet-4-20250514: $1.321200
2025-10-03 > gpt-4-turbo: $0.250000
```

### 🗂️ JSON 模式

**完整精度，用于财务核算和集成**

```bash
vct usage --json
```

```json
{
  "2025-10-01": [
    {
      "model": "claude-sonnet-4-20250514",
      "usage": {
        "input_tokens": 45230,
        "output_tokens": 12450,
        "cache_read_input_tokens": 230500,
        "cache_creation_input_tokens": 50000,
        "cache_creation": {
          "ephemeral_5m_input_tokens": 50000
        },
        "service_tier": "standard"
      },
      "cost_usd": 2.1542304567890125
    }
  ]
}
```

### 🔍 输出对比

| 特性         | 交互式 | 表格  | 文本      | JSON               |
| ------------ | ------ | ----- | --------- | ------------------ |
| **最适合**   | 监控   | 报表  | 脚本      | 集成               |
| **成本格式** | $2.15  | $2.15 | $2.154230 | 2.1542304567890123 |
| **更新**     | 实时   | 静态  | 静态      | 静态               |
| **颜色**     | ✅     | ✅    | ❌        | ❌                 |
| **可解析**   | ❌     | ❌    | ✅        | ✅                 |

### 💡 使用场景

- **预算追踪**：监控您的每日 AI 支出
- **成本优化**：识别昂贵的会话
- **团队报告**：为管理层生成使用报告
- **账单**：导出精确成本用于开票
- **监控**：活跃开发的实时仪表板

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

# 批量并按提供者分组：输出完整的 records，按提供者分组（JSON 格式）
vct analysis --all

# 将分组结果保存到文件
vct analysis --all --output grouped_report.json
```

### 您将获得什么

**单文件分析**：

- **Token 使用量**：按模型的输入、输出和缓存统计
- **文件操作**：每次读取、写入和编辑的完整详情
- **命令历史**：所有执行的 shell 命令
- **工具使用**：每种工具类型的使用次数
- **元数据**：用户、机器 ID、Git 仓库、时间戳

**批量分析**：

- **汇总指标**：按日期和模型分组
- **行数统计**：编辑、读取和写入操作
- **工具统计**：Bash、Edit、Read、TodoWrite、Write 计数
- **交互式显示**：实时 TUI 表格（默认）
- **JSON 导出**：结构化数据用于进一步处理

### 示例输出 - 单文件

```json
{
  "extensionName": "Claude-Code",
  "insightsVersion": "0.1.0",
  "user": "wei",
  "machineId": "5b0dfa41ada84d5180a514698f67bd80",
  "records": [
    {
      "conversationUsage": {
        "claude-sonnet-4-20250514": {
          "input_tokens": 252,
          "output_tokens": 3921,
          "cache_read_input_tokens": 1298818,
          "cache_creation_input_tokens": 124169
        }
      },
      "toolCallCounts": {
        "Read": 15,
        "Write": 4,
        "Edit": 2,
        "Bash": 5,
        "TodoWrite": 3
      },
      "totalUniqueFiles": 8,
      "totalWriteLines": 80,
      "totalReadLines": 120,
      "folderPath": "/home/wei/repo/project",
      "gitRemoteUrl": "https://github.com/user/project.git"
    }
  ]
}
```

### 示例输出 - 批量分析

**交互式表格**（运行 `vct analysis` 时的默认输出）：

```
┌──────────────────────────────────────────────────────────────────┐
│                  🔍 分析统计                                     │
└──────────────────────────────────────────────────────────────────┘
┌────────────┬────────────────────┬────────────┬────────────┬────────────┬──────┬──────┬──────┬───────────┬───────┐
│ 日期       │ 模型               │ 编辑行数   │ 读取行数   │ 写入行数   │ Bash │ Edit │ Read │ TodoWrite │ Write │
├────────────┼────────────────────┼────────────┼────────────┼────────────┼──────┼──────┼──────┼───────────┼───────┤
│ 2025-10-02 │ claude-sonnet-4-5…│ 901        │ 11,525     │ 53         │ 13   │ 26   │ 27   │ 10        │ 1     │
│ 2025-10-03 │ claude-sonnet-4-5…│ 574        │ 10,057     │ 1,415      │ 53   │ 87   │ 78   │ 30        │ 8     │
│ 2025-10-03 │ gpt-5-codex        │ 0          │ 1,323      │ 0          │ 75   │ 0    │ 20   │ 0         │ 0     │
│            │ 总计               │ 1,475      │ 22,905     │ 1,468      │ 141  │ 113  │ 125  │ 40        │ 9     │
└────────────┴────────────────────┴────────────┴────────────┴────────────┴──────┴──────┴──────┴───────────┴───────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ 📝 总行数：25,848  |  🔧 总工具：428  |  📅 条目：3  |  ⚡ CPU：1.8%  |  🧠 内存：8.2 MB                          │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                    📈 每日平均（按提供者）                                                      │
│                                                                                                                 │
│  🤖 Claude Code: 737 编辑/天 | 10,791 读取/天 | 734 写入/天 | 3 天                                             │
│  💻 Codex: 0 编辑/天 | 1,323 读取/天 | 0 写入/天 | 1 天                                                         │
│  ⭐ 所有提供者: 491 编辑/天 | 7,635 读取/天 | 489 写入/天 | 3 天                                                │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**静态表格模式**（使用 `--table`）：

```bash
vct analysis --table
```

```
🔍 分析统计

╔════════════╦════════════════════╦════════════╦════════════╦═════════════╦══════╦═══════╦═══════╦═══════════╦═══════╗
║ 日期       ║ 模型               ║ 编辑行数   ║ 读取行数   ║ 写入行数    ║ Bash ║  Edit ║  Read ║ TodoWrite ║ Write ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-02 ║ claude-sonnet-4-5…║ 901        ║ 11,525     ║ 53          ║ 13   ║ 26    ║ 27    ║ 10        ║ 1     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-03 ║ claude-sonnet-4-5…║ 574        ║ 10,057     ║ 1,415       ║ 53   ║ 87    ║ 78    ║ 30        ║ 8     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║ 2025-10-03 ║ gpt-5-codex        ║ 0          ║ 1,323      ║ 0           ║ 75   ║ 0     ║ 20    ║ 0         ║ 0     ║
╠════════════╬════════════════════╬════════════╬════════════╬═════════════╬══════╬═══════╬═══════╬═══════════╬═══════╣
║            ║ 总计               ║ 1,475      ║ 22,905     ║ 1,468       ║ 141  ║ 113   ║ 125   ║ 40        ║ 9     ║
╚════════════╩════════════════════╩════════════╩════════════╩═════════════╩══════╩═══════╩═══════╩═══════════╩═══════╝

📈 每日平均（按提供者）

╔══════════════╦═══════════╦═══════════╦════════════╦══════════╦══════════╦══════════╦══════════╦═══════════╦══════╗
║ 提供者       ║ 编辑/天   ║ 读取/天   ║ 写入/天    ║ Bash/天  ║ Edit/天  ║ Read/天  ║ Todo/天  ║ Write/天  ║ 天数 ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ 🤖 Claude Code ║ 737.5     ║ 10,791    ║ 734        ║ 33.0     ║ 56.5     ║ 52.5     ║ 20.0     ║ 4.5       ║ 2    ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ 💻 Codex       ║ 0         ║ 1,323     ║ 0          ║ 75.0     ║ 0.0      ║ 20.0     ║ 0.0      ║ 0.0       ║ 1    ║
╠══════════════╬═══════════╬═══════════╬════════════╬══════════╬══════════╬══════════╬══════════╬═══════════╬══════╣
║ ⭐ 所有提供者  ║ 491.7     ║ 7,635     ║ 489.3      ║ 47.0     ║ 37.7     ║ 41.7     ║ 13.3     ║ 3.0       ║ 3    ║
╚══════════════╩═══════════╩═══════════╩════════════╩══════════╩══════════╩══════════╩══════════╩═══════════╩══════╝
```

**JSON 导出**（使用 `--output`）：

```json
[
  {
    "date": "2025-10-02",
    "model": "claude-sonnet-4-5-20250929",
    "editLines": 901,
    "readLines": 11525,
    "writeLines": 53,
    "bashCount": 13,
    "editCount": 26,
    "readCount": 27,
    "todoWriteCount": 10,
    "writeCount": 1
  },
  {
    "date": "2025-10-03",
    "model": "claude-sonnet-4-5-20250929",
    "editLines": 574,
    "readLines": 10057,
    "writeLines": 1415,
    "bashCount": 53,
    "editCount": 87,
    "readCount": 78,
    "todoWriteCount": 30,
    "writeCount": 8
  }
]
```

### 💡 使用场景

**单文件分析**：

- **使用审计**：追踪 AI 在每个会话中做了什么
- **成本归因**：计算每个项目或功能的成本
- **合规性**：导出详细的活动日志
- **分析**：了解编码模式和工具使用

**批量分析**：

- **生产力追踪**：监控随时间推移的编码活动
- **工具使用模式**：识别所有会话中最常用的工具
- **模型比较**：比较不同 AI 模型之间的效率
- **历史分析**：按日期追踪代码操作趋势

---

## 🔧 Version 命令

**检查您的安装。**

```bash
# 格式化输出
vct version

# JSON 格式
vct version --json

# 纯文本
vct version --text
```

### 输出

```
🚀 Vibe Coding Tracker

╔════════════════╦═════════╗
║ 版本           ║ 0.1.0   ║
╠════════════════╬═════════╣
║ Rust 版本      ║ 1.89.0  ║
╠════════════════╬═════════╣
║ Cargo 版本     ║ 1.89.0  ║
╚════════════════╩═════════╝
```

---

## 🔄 Update 命令

**自动保持安装版本为最新。**

update 命令会检查 GitHub releases 并为您的平台下载最新版本。

### 基本用法

```bash
# 交互式更新（会询问确认）
vct update

# 仅检查更新而不安装
vct update --check

# 强制更新，不显示确认提示
vct update --force
```

### 工作原理

1. **检查最新版本**：从 GitHub API 获取最新 release
2. **比较版本**：比较当前版本与最新可用版本
3. **下载二进制文件**：下载适合您平台的二进制文件（Linux/macOS/Windows）
4. **智能替换**：
   - **Linux/macOS**：自动替换二进制文件（将旧版本备份为 `.old`）
   - **Windows**：下载为 `.new` 并创建批处理脚本以安全替换

### 平台支持

update 命令会自动检测您的平台并下载正确的压缩文件：

- **Linux**：`vibe_coding_tracker-v{版本}-linux-x64-gnu.tar.gz`、`vibe_coding_tracker-v{版本}-linux-arm64-gnu.tar.gz`
- **macOS**：`vibe_coding_tracker-v{版本}-macos-x64.tar.gz`、`vibe_coding_tracker-v{版本}-macos-arm64.tar.gz`
- **Windows**：`vibe_coding_tracker-v{版本}-windows-x64.zip`、`vibe_coding_tracker-v{版本}-windows-arm64.zip`

### Windows 更新流程

在 Windows 上，无法在程序运行时替换二进制文件。update 命令会：

1. 将新版本下载为 `vct.new`
2. 创建更新脚本（`update_vct.bat`）
3. 显示完成更新的说明

关闭应用程序后运行批处理脚本以完成更新。

---

## 💡 智能定价系统

### 工作原理

1. **自动更新**：每天从 [LiteLLM](https://github.com/BerriAI/litellm) 获取定价
2. **智能缓存**：在 `~/.vibe_coding_tracker/` 中存储定价 24 小时
3. **模糊匹配**：即使对于自定义模型名称也能找到最佳匹配
4. **始终准确**：确保您获得最新的定价

### 模型匹配

**优先级顺序**：

1. ✅ **精确匹配**：`claude-sonnet-4` → `claude-sonnet-4`
2. 🔄 **规范化**：`claude-sonnet-4-20250514` → `claude-sonnet-4`
3. 🔍 **子字符串**：`custom-gpt-4` → `gpt-4`
4. 🎯 **模糊（AI 驱动）**：使用 Jaro-Winkler 相似度（70% 阈值）
5. 💵 **后备**：如果找不到匹配则显示 $0.00

### 成本计算

```
总成本 = (输入 Token × 输入成本) +
         (输出 Token × 输出成本) +
         (缓存读取 × 缓存读取成本) +
         (缓存创建 × 缓存创建成本)
```

---

## 🐳 Docker 支持

```bash
# 构建镜像
docker build -f docker/Dockerfile --target prod -t vibe_coding_tracker:latest .

# 使用您的会话运行
docker run --rm \
    -v ~/.claude:/root/.claude \
    -v ~/.codex:/root/.codex \
    -v ~/.gemini:/root/.gemini \
    vibe_coding_tracker:latest usage
```

---

## 🔍 故障排除

### 定价数据未加载

```bash
# 检查缓存
ls -la ~/.vibe_coding_tracker/

# 强制刷新
rm -rf ~/.vibe_coding_tracker/
vct usage

# 调试模式
RUST_LOG=debug vct usage
```

### 没有显示使用数据

```bash
# 验证会话目录
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/
ls -la ~/.gemini/tmp/

# 统计会话文件
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
find ~/.gemini/tmp -name "*.json" | wc -l
```

### Analysis 命令失败

```bash
# 验证 JSONL 格式
jq empty < your-file.jsonl

# 检查文件权限
ls -la your-file.jsonl

# 使用调试输出运行
RUST_LOG=debug vct analysis --path your-file.jsonl
```

### 交互式模式问题

```bash
# 如果中断则重置终端
reset

# 检查终端类型
echo $TERM  # 应该是 xterm-256color 或兼容

# 使用静态表格作为后备
vct usage --table
```

---

## ⚡ 性能

使用 Rust 构建，追求**速度**和**可靠性**：

| 操作             | 时间   |
| ---------------- | ------ |
| 解析 10MB JSONL  | ~320ms |
| 分析 1000 个事件 | ~45ms  |
| 加载缓存的定价   | ~2ms   |
| 交互式刷新       | ~30ms  |

**二进制大小**：~3-5 MB（剥离后）

---

## 📚 了解更多

- **开发者文档**：参见 [.github/copilot-instructions.md](.github/copilot-instructions.md)
- **报告问题**：[GitHub Issues](https://github.com/Mai0313/VibeCodingTracker/issues)
- **源代码**：[GitHub 仓库](https://github.com/Mai0313/VibeCodingTracker)

---

## 🤝 贡献

欢迎贡献！方法如下：

1. Fork 仓库
2. 创建您的功能分支
3. 进行更改
4. 提交拉取请求

有关开发设置和指南，请参见 [.github/copilot-instructions.md](.github/copilot-instructions.md)。

---

## 📄 许可证

MIT 许可证 - 详见 [LICENSE](LICENSE)。

---

## 🙏 鸣谢

- [LiteLLM](https://github.com/BerriAI/litellm) 提供模型定价数据
- Claude Code、Codex 和 Gemini 团队创建了出色的 AI 编程助手
- Rust 社区提供了优秀的工具

---

<center>

**省钱。追踪使用量。更智能地编程。**

如果您觉得有用，请[⭐ Star 这个项目](https://github.com/Mai0313/VibeCodingTracker)！

使用 🦀 Rust 制作

</center>
