<center>

# CodexUsage — AI 编程助手使用量追踪器

[![rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![tests](https://github.com/Mai0313/codex_usage/actions/workflows/test.yml/badge.svg)](https://github.com/Mai0313/codex_usage/actions/workflows/test.yml)
[![code-quality](https://github.com/Mai0313/codex_usage/actions/workflows/code-quality-check.yml/badge.svg)](https://github.com/Mai0313/codex_usage/actions/workflows/code-quality-check.yml)
[![license](https://img.shields.io/badge/License-MIT-green.svg?labelColor=gray)](https://github.com/Mai0313/codex_usage/tree/master?tab=License-1-ov-file)
[![PRs](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/Mai0313/codex_usage/pulls)

</center>

**实时追踪您的 AI 编程成本。** CodexUsage 是一个强大的 CLI 工具，帮助您监控和分析 Claude Code 和 Codex 的使用情况，提供详细的成本分解、token 统计和代码操作洞察。

[English](README.md) | [繁體中文](README.zh-TW.md) | [简体中文](README.zh-CN.md)

---

## 🎯 为什么选择 CodexUsage？

### 💰 了解您的成本
不再疑惑您的 AI 编程会话花费多少。通过 [LiteLLM](https://github.com/BerriAI/litellm) 自动更新定价，获取**实时成本追踪**。

### 📊 精美的可视化
选择您喜欢的视图：
- **交互式仪表板**：自动刷新的终端 UI，实时更新
- **静态报表**：专业的表格，适合文档
- **脚本友好**：纯文本和 JSON，便于自动化
- **完整精度**：导出精确成本，用于财务核算

### 🚀 零配置
自动检测并处理 Claude Code 和 Codex 的日志。无需设置——只需运行和分析。

### 🎨 丰富的洞察
- 按模型和日期的 token 使用量
- 按缓存类型的成本分解
- 文件操作追踪
- 命令执行历史
- Git 仓库信息

---

## ✨ 核心特性

| 特性 | 描述 |
|---------|-------------|
| 🤖 **自动检测** | 智能识别 Claude Code 或 Codex 日志 |
| 💵 **智能定价** | 模糊模型匹配 + 每日缓存以提高速度 |
| 🎨 **4 种显示模式** | 交互式、表格、文本和 JSON 输出 |
| 📈 **全面统计** | Token、成本、文件操作和工具调用 |
| ⚡ **高性能** | 使用 Rust 构建，速度快且可靠 |
| 🔄 **实时更新** | 仪表板每秒刷新 |
| 💾 **高效缓存** | 智能的每日缓存减少 API 调用 |

---

## 🚀 快速开始

### 安装

**前提条件**：[Rust 工具链](https://rustup.rs/)（1.70+）

```bash
# 克隆和构建
git clone https://github.com/Mai0313/codex_usage.git
cd CodexUsage
cargo build --release

# 二进制文件位置：./target/release/codex_usage
```

### 首次运行

```bash
# 使用交互式仪表板查看使用量
./target/release/codex_usage usage

# 或分析特定对话
./target/release/codex_usage analysis --path ~/.claude/projects/session.jsonl
```

---

## 📖 命令指南

### 🔍 快速参考

```bash
codex_usage <命令> [选项]

命令：
  usage       显示 token 使用量和成本（默认：交互式）
  analysis    分析对话文件并导出数据
  version     显示版本信息
  help        显示帮助信息
```

---

## 💰 Usage 命令

**追踪您所有 AI 编程会话的支出。**

### 基本用法

```bash
# 交互式仪表板（推荐）
codex_usage usage

# 静态表格，适合报表
codex_usage usage --table

# 纯文本，适合脚本
codex_usage usage --text

# JSON，适合数据处理
codex_usage usage --json
```

### 您将获得什么

该工具自动扫描这些目录：
- `~/.claude/projects/*.jsonl`（Claude Code）
- `~/.codex/sessions/*.jsonl`（Codex）

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
│ 💰 总成本：$5.30  |  🔢 总 Token：819,900  |  📅 条目：4  |  🧠 内存：12.5 MB                                    │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

按 'q'、'Esc' 或 'Ctrl+C' 退出
```

**特性**：
- ✨ 每秒自动刷新
- 🎯 高亮今日条目
- 🔄 显示最近更新的行
- 💾 显示内存使用量
- 📊 汇总统计

**控制**：按 `q`、`Esc` 或 `Ctrl+C` 退出

### 📋 静态表格模式

**非常适合文档和报表**

```bash
codex_usage usage --table
```

```
📊 Token 使用统计

╔════════════╦══════════════════════╦════════════╦════════════╦════════════╦══════════════╦══════════════╦════════════╗
║ 日期       ║ 模型                 ║ 输入       ║ 输出       ║ 缓存读取   ║ 缓存创建     ║ 总 Token     ║ 成本 (USD) ║
╠════════════╬══════════════════════╬════════════╬════════════╬════════════╬══════════════╬══════════════╬════════════╣
║ 2025-10-01 ║ claude-sonnet-4-20…  ║ 45,230     ║ 12,450     ║ 230,500    ║ 50,000       ║ 338,180      ║ $2.15      ║
║ 2025-10-02 ║ claude-sonnet-4-20…  ║ 32,100     ║ 8,920      ║ 180,000    ║ 30,000       ║ 251,020      ║ $1.58      ║
║ 2025-10-03 ║ claude-sonnet-4-20…  ║ 28,500     ║ 7,200      ║ 150,000    ║ 25,000       ║ 210,700      ║ $1.32      ║
║            ║ 总计                 ║ 105,830    ║ 28,570     ║ 560,500    ║ 105,000      ║ 799,900      ║ $5.05      ║
╚════════════╩══════════════════════╩════════════╩════════════╩════════════╩══════════════╩══════════════╩════════════╝
```

### 📝 文本模式

**非常适合脚本和解析**

```bash
codex_usage usage --text
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
codex_usage usage --json
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
      "cost_usd": 2.1542304567890123
    }
  ]
}
```

### 🔍 输出对比

| 特性 | 交互式 | 表格 | 文本 | JSON |
|---------|-------------|-------|------|------|
| **最适合** | 监控 | 报表 | 脚本 | 集成 |
| **成本格式** | $2.15 | $2.15 | $2.154230 | 2.1542304567890123 |
| **更新** | 实时 | 静态 | 静态 | 静态 |
| **颜色** | ✅ | ✅ | ❌ | ❌ |
| **可解析** | ❌ | ❌ | ✅ | ✅ |

### 💡 使用场景

- **预算追踪**：监控您的每日 AI 支出
- **成本优化**：识别昂贵的会话
- **团队报告**：为管理层生成使用报告
- **账单**：导出精确成本用于开票
- **监控**：活跃开发的实时仪表板

---

## 📊 Analysis 命令

**深入了解特定对话文件。**

### 基本用法

```bash
# 分析并显示
codex_usage analysis --path ~/.claude/projects/session.jsonl

# 保存到文件
codex_usage analysis --path ~/.claude/projects/session.jsonl --output report.json
```

### 您将获得什么

详细的 JSON 报告包括：
- **Token 使用量**：按模型的输入、输出和缓存统计
- **文件操作**：每次读取、写入和编辑的完整详情
- **命令历史**：所有执行的 shell 命令
- **工具使用**：每种工具类型的使用次数
- **元数据**：用户、机器 ID、Git 仓库、时间戳

### 示例输出

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

### 💡 使用场景

- **使用审计**：追踪 AI 在每个会话中做了什么
- **成本归因**：计算每个项目或功能的成本
- **合规性**：导出详细的活动日志
- **分析**：了解编码模式和工具使用

---

## 🔧 Version 命令

**检查您的安装。**

```bash
# 格式化输出
codex_usage version

# JSON 格式
codex_usage version --json

# 纯文本
codex_usage version --text
```

### 输出

```
🚀 Codex Usage Analyzer

╔════════════════╦═════════╗
║ 版本           ║ 0.1.0   ║
╠════════════════╬═════════╣
║ Rust 版本      ║ 1.89.0  ║
╠════════════════╬═════════╣
║ Cargo 版本     ║ 1.89.0  ║
╚════════════════╩═════════╝
```

---

## 💡 智能定价系统

### 工作原理

1. **自动更新**：每天从 [LiteLLM](https://github.com/BerriAI/litellm) 获取定价
2. **智能缓存**：在 `~/.codex-usage/` 中存储定价 24 小时
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
docker build -f docker/Dockerfile --target prod -t codex_usage:latest .

# 使用您的会话运行
docker run --rm \
  -v ~/.claude:/root/.claude \
  -v ~/.codex:/root/.codex \
  codex_usage:latest usage
```

---

## 🔍 故障排除

### 定价数据未加载

```bash
# 检查缓存
ls -la ~/.codex-usage/

# 强制刷新
rm -rf ~/.codex-usage/
codex_usage usage

# 调试模式
RUST_LOG=debug codex_usage usage
```

### 没有显示使用数据

```bash
# 验证会话目录
ls -la ~/.claude/projects/
ls -la ~/.codex/sessions/

# 统计 JSONL 文件
find ~/.claude/projects -name "*.jsonl" | wc -l
find ~/.codex/sessions -name "*.jsonl" | wc -l
```

### Analysis 命令失败

```bash
# 验证 JSONL 格式
jq empty < your-file.jsonl

# 检查文件权限
ls -la your-file.jsonl

# 使用调试输出运行
RUST_LOG=debug codex_usage analysis --path your-file.jsonl
```

### 交互式模式问题

```bash
# 如果中断则重置终端
reset

# 检查终端类型
echo $TERM  # 应该是 xterm-256color 或兼容

# 使用静态表格作为后备
codex_usage usage --table
```

---

## ⚡ 性能

使用 Rust 构建，追求**速度**和**可靠性**：

| 操作 | 时间 |
|-----------|------|
| 解析 10MB JSONL | ~320ms |
| 分析 1000 个事件 | ~45ms |
| 加载缓存的定价 | ~2ms |
| 交互式刷新 | ~30ms |

**二进制大小**：~3-5 MB（剥离后）

---

## 📚 了解更多

- **开发者文档**：参见 [.github/copilot-instructions.md](.github/copilot-instructions.md)
- **报告问题**：[GitHub Issues](https://github.com/Mai0313/codex_usage/issues)
- **源代码**：[GitHub 仓库](https://github.com/Mai0313/codex_usage)

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
- Claude Code 和 Codex 团队创建了出色的 AI 编程助手
- Rust 社区提供了优秀的工具

---

<center>

**省钱。追踪使用量。更智能地编程。**

如果您觉得有用，请[⭐ Star 这个项目](https://github.com/Mai0313/codex_usage)！

使用 🦀 Rust 制作

</center>
