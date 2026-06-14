# Innate × Claude Code 插件化实施文档 v1

> 状态：实施提案（待评审）
> 适用版本：Innate v0.1.10
> 关联代码：`core/src/install/`、`skills/innate-memory/SKILL.md`、`core/assets/SKILL.md`
> 关联设计：`docs/Innate-设计文档-v0.1.9.md`（编码基线）

---

## 0. TL;DR

Innate 现在通过 `innate install` 这个 TUI 向导，**手工**把自己接入 Claude Code：
写 MCP server 配置、装 Skill（symlink）、装 6 个 slash command、装 4 个 hook、写 `permissions.allow`。
这套逻辑分散在 `core/src/install/{agents,skills,settings,wizard}.rs`（约 1900 行），且每个被支持的 agent（Claude / Codex / opencode）都要单独实现一遍写配置的代码。

Claude Code 的**插件（Plugin）机制**正是为「把命令 + hook + agent + MCP server 打成一个包，一键安装」而生。
本文档给出把 Innate 对 **Claude Code** 这一端的接入，从「命令式手工写配置」迁移到「声明式插件包 + marketplace 分发」的完整实施方案。

**范围边界**：
- ✅ 本次只把 **Claude Code** 接入方式插件化。
- ❌ 不动 Codex / opencode —— 它们没有插件机制，继续走 `innate install` 的 TOML/JSONC 写入逻辑。
- ✅ `innate install` 继续保留，作为「非插件用户 / 离线 / Codex / opencode」的安装路径。插件是 Claude Code 用户的**新增**首选路径，不是替换。

---

## 1. 背景：插件 vs 当前 install vs MCP

### 1.1 三者关系

| 概念 | 本质 | 在 Innate 里对应什么 |
|---|---|---|
| **MCP** | 协议（JSON-RPC over stdio）。Claude 作为 client 连接外部工具服务 | `innate mcp` —— 14 个 tool 直连 KnowledgeBase Core |
| **Skill / 命令 / hook** | Claude Code 的扩展点（能力说明 / `/命令` / 生命周期钩子） | `SKILL.md`、6 个 slash command、Stop/UserPromptSubmit/SessionStart/SubagentStop hook |
| **插件（Plugin）** | **分发容器**：把上面这些（含 MCP server 声明）打包，一键装卸 | 目前不存在 —— 这些能力靠 `innate install` 逐个手写进用户的 `settings.json` |

一句话：**MCP 是"能力来源"，插件是"分发容器"**。插件**包含** MCP server 声明，是更外层的东西。

### 1.2 当前 `innate install` 实际做了什么（迁移基线）

读 `core/src/install/wizard.rs::run_install`，对 Claude Code 一端它会写入：

1. **MCP server**（`agents.rs::configure_claude`）→ 写入 `mcpServers.innate = {type:"stdio", command:"innate", args:["mcp"]}`
2. **权限自动放行**（同上）→ 往 `permissions.allow` 追加 `"mcp__innate__*"`
3. **Skill**（`skills.rs::install_skill`）→ 把 `SKILL.md` 写到 `~/.agents/skills/innate-memory/`，再 symlink 到 `~/.claude/skills/innate-memory`
4. **6 个 slash command**（`skills.rs::install_commands`）→ 从 `SKILL.md` 里的 ` ```command ` 块解析出 `innate-recall / innate-record / innate-save / innate-spark / innate-evolve / innate-inspect`，逐个写到 `~/.claude/commands/<name>.md`
5. **4 个 hook**（`agents.rs::configure_claude_*_hook`）：
   - `Stop` → `innate hook stop`
   - `UserPromptSubmit` → `innate hook prompt`（关联召回，relevance-gated）
   - `SessionStart` → `innate hook session-start`
   - `SubagentStop` → `innate hook stop`

> ⚠️ 注意 wizard.rs:149-157 的既有约束：hook **必须**写进 `settings.json`，不能写进 `~/.claude.json`（后者只放 MCP / OAuth / state）。插件机制天然规避了这个坑——插件 hook 由插件清单声明，不碰用户的 `settings.json`。

这 5 类东西，正是插件清单能声明的全部内容。迁移目标就是把它们从"运行时写进用户配置"变成"插件包里的静态声明文件"。

---

## 2. 目标与收益

### 2.1 目标

让 Claude Code 用户能用以下方式接入 Innate：

```bash
/plugin marketplace add renmengkai/innate      # 添加 marketplace（仓库根的 marketplace 清单）
/plugin install innate@innate                  # 一键安装：MCP + 命令 + hook + skill
```

安装后立即获得：`innate` MCP server（14 tool）、6 个 `/innate-*` 命令、4 个 hook、innate-memory skill —— 与今天 `innate install` 的产物**功能等价**。

### 2.2 收益

| 维度 | 现状（`innate install`） | 插件化后 |
|---|---|---|
| 安装 | 跑 TUI 向导、改用户 `settings.json` | 两条斜杠命令，零侵入用户配置 |
| 卸载 | `innate uninstall` 反向删配置（易残留） | `/plugin uninstall`，Claude Code 托管，干净 |
| 升级 | 用户重跑 install / `innate upgrade` 后再 install | `/plugin update`，跟随 marketplace 版本 |
| hook 冲突 | 直接追加到用户 `settings.json`，难回收 | 插件命名空间隔离，不污染用户配置 |
| 维护成本 | 写配置逻辑（~1900 行）需自测幂等/容错 | 声明式清单，由 Claude Code 校验加载 |

---

## 3. 插件包结构

新建一个**插件目录**，建议直接放在本仓库内，与现有资源单一真源对齐：

```text
Innate/
├─ plugin/                              ← 新增：Claude Code 插件根
│  └─ innate/
│     ├─ .claude-plugin/
│     │  └─ plugin.json                 ← 插件清单（名称、版本、组件入口）
│     ├─ commands/                      ← 6 个 slash command（从 SKILL.md 生成）
│     │  ├─ innate-recall.md
│     │  ├─ innate-record.md
│     │  ├─ innate-save.md
│     │  ├─ innate-spark.md
│     │  ├─ innate-evolve.md
│     │  └─ innate-inspect.md
│     ├─ skills/
│     │  └─ innate-memory/
│     │     └─ SKILL.md                 ← 与 core/assets/SKILL.md 字节一致
│     ├─ hooks/
│     │  └─ hooks.json                  ← Stop / UserPromptSubmit / SessionStart / SubagentStop
│     └─ .mcp.json                      ← innate MCP server 声明
└─ .claude-plugin/
   └─ marketplace.json                  ← 仓库根 marketplace 清单（指向 plugin/innate）
```

### 3.1 `plugin/innate/.claude-plugin/plugin.json`

```json
{
  "name": "innate",
  "version": "0.1.10",
  "description": "自成长 agent 程序性知识层 —— 召回 / 记录 / 蒸馏 / 演化闭环",
  "author": { "name": "Renmengkai", "email": "renmengkai@gmail.com" },
  "homepage": "https://github.com/renmengkai/innate",
  "mcpServers": "./.mcp.json",
  "commands": "./commands",
  "hooks": "./hooks/hooks.json",
  "skills": "./skills"
}
```

> 字段以 Claude Code 当前插件 schema 为准（见 §8 验证清单）。若某字段路径与默认约定一致（如 `commands/`、`skills/`、`hooks/hooks.json`），可省略显式声明，依赖目录约定发现。

### 3.2 `plugin/innate/.mcp.json`

直接等价于今天 `configure_claude` 写入 `mcpServers.innate` 的内容：

```json
{
  "mcpServers": {
    "innate": {
      "type": "stdio",
      "command": "innate",
      "args": ["mcp"]
    }
  }
}
```

> **前提**：`innate` 二进制必须在 PATH 上。插件**不负责**安装二进制（插件只是声明，不跑安装器）。因此首次使用仍需 `innate upgrade` / 包管理器 / `innate install` 把二进制装上 PATH。详见 §6 二进制依赖。
> 命令可用 `${CLAUDE_PLUGIN_ROOT}` 等插件变量；但 Innate 二进制不在插件包内，故仍用裸 `innate`（依赖 PATH）。

### 3.3 `plugin/innate/hooks/hooks.json`

把今天 `configure_claude_*_hook` 追加进用户 `settings.json` 的 4 个 hook，原样声明：

```json
{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "innate hook session-start" }] }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "innate hook prompt" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "innate hook stop" }] }
    ],
    "SubagentStop": [
      { "hooks": [{ "type": "command", "command": "innate hook stop" }] }
    ]
  }
}
```

> 与 `agents.rs::configure_claude_hook` 生成的结构完全一致（`{"hooks":[{"type":"command","command":...}]}`）。区别仅在于：声明位置从用户 `settings.json` 变成插件清单。

### 3.4 `plugin/innate/commands/*.md`

6 个命令的 body 就是 `SKILL.md` 里 ` ```command ` 块 `---` 之后的正文（与 `skills.rs::parse_skill_commands` 的解析结果一致）。
**不要手抄**——见 §5 的生成脚本，保证与 SKILL.md 单一真源同步。

### 3.5 `plugin/innate/skills/innate-memory/SKILL.md`

与 `core/assets/SKILL.md`、`skills/innate-memory/SKILL.md` **字节一致**。这是 Innate 既有的"Skill 双副本必须同步"约束的第三个落点（见 §7 风险）。

### 3.6 仓库根 `.claude-plugin/marketplace.json`

```json
{
  "name": "innate",
  "owner": { "name": "Renmengkai", "email": "renmengkai@gmail.com" },
  "plugins": [
    {
      "name": "innate",
      "source": "./plugin/innate",
      "description": "自成长 agent 程序性知识层（召回/记录/蒸馏/演化闭环）"
    }
  ]
}
```

用户 `/plugin marketplace add renmengkai/innate` 时，Claude Code 读取仓库根这个清单，发现名为 `innate` 的插件，源在 `./plugin/innate`。

---

## 4. 字段映射总表（install → 插件）

| 当前 install 行为 | 代码位置 | 插件中的等价物 |
|---|---|---|
| 写 `mcpServers.innate` | `agents.rs::configure_claude` | `plugin/innate/.mcp.json` |
| 追加 `permissions.allow: mcp__innate__*` | `agents.rs::configure_claude` | 插件 MCP 工具默认在插件命名空间放行（见 §6.2）；如仍需显式放行，由用户一次性确认 |
| 装 Skill（symlink） | `skills.rs::install_skill` | `plugin/innate/skills/innate-memory/SKILL.md`（插件托管，无需 symlink） |
| 装 6 个 slash command | `skills.rs::install_commands` | `plugin/innate/commands/*.md`（生成自 SKILL.md） |
| Stop hook | `configure_claude_stop_hook` | `hooks.json` → `Stop` |
| UserPromptSubmit hook | `configure_claude_prompt_hook` | `hooks.json` → `UserPromptSubmit` |
| SessionStart hook | `configure_claude_session_start_hook` | `hooks.json` → `SessionStart` |
| SubagentStop hook | `configure_claude_subagent_stop_hook` | `hooks.json` → `SubagentStop` |
| LLM / embedding 配置 | `settings.rs::configure_llm_interactive` | **不进插件** —— 仍由 `innate install` 或手编 `~/.innate/settings.json`（属于 Innate 自身状态，与 Claude Code 无关） |
| daemon watch 配置 | `settings.rs::configure_daemon_interactive` | **不进插件** —— 同上 |

> 关键结论：插件只覆盖 **Claude Code 侧的接入声明**（MCP / 命令 / hook / skill）。Innate **自身**的运行配置（LLM、embedding、daemon、`~/.innate/`）依旧由 Innate 自己管，与插件正交。

---

## 5. 生成脚本：从 SKILL.md 派生命令文件

为避免命令 body 手抄漂移，新增一个构建期脚本，把 `core/assets/SKILL.md` 的 ` ```command ` 块导出为 `plugin/innate/commands/*.md`。逻辑与 `skills.rs::parse_skill_commands` 完全一致（` ```command ` → `name:` 头 → 裸 `---` → body）。

建议形态：`scripts/gen-plugin-commands.sh`（或一个 `xtask`），CI 中校验生成结果与已提交文件一致（类似现有 `cmp skills/innate-memory/SKILL.md core/assets/SKILL.md` 的同步校验）。

伪代码：

```text
读 core/assets/SKILL.md
for each ```command block:
    解析 name: 与 --- 后的 body
    写 plugin/innate/commands/<name>.md = body
拷贝 core/assets/SKILL.md → plugin/innate/skills/innate-memory/SKILL.md
```

CI 增加一步：跑脚本 → `git diff --exit-code plugin/` 必须干净，否则报"插件资源未同步"。

---

## 6. 二进制与权限

### 6.1 二进制依赖（重要）

插件清单声明 `command: "innate"`，**假定 `innate` 在 PATH**。插件本身不携带、不安装二进制。因此用户首次接入仍需先装二进制，三选一：

1. `innate install`（现有向导，会装 PATH + 也能配 LLM/daemon）；
2. 包管理器 / Release 下载 + 手动放 PATH；
3. `innate upgrade`（已装过的自更新）。

> 文档/README 要把这一步讲清楚：**「插件负责接入，二进制负责能力」**。可在 SessionStart hook 或命令里加一个 `innate --version` 探活，缺失时提示用户安装。

### 6.2 权限放行

今天 install 会写 `permissions.allow: ["mcp__innate__*"]` 跳过每次工具确认。插件场景下：
- 插件声明的 MCP 工具通常在插件命名空间内被信任，安装时一次性确认即可；
- 若 Claude Code 仍对插件 MCP 工具逐次询问，则在文档中引导用户把 `mcp__innate__*` 加入其 `permissions.allow`（与现状一致），或保留 `innate install --claude` 仅做这一步。

> 这一条需在 §8 真机验证：确认插件安装后 MCP 工具的默认权限行为，决定是否还需要显式 allow。

---

## 7. 与现有机制的关系 / 风险

1. **`innate install` 不删除**。Codex / opencode 无插件机制；离线用户、想一并配 LLM/daemon 的用户仍走向导。插件是 Claude Code 用户的新增首选。需在 install 向导里探测「是否已通过插件安装」，避免 Claude 一端**双重配置**（用户 settings.json 与插件都声明了同一组 hook/MCP → hook 触发两次、MCP 重名）。
   - 建议：install 检测到 Claude 插件已装时，对 Claude 一端只做"已由插件接管，跳过"。

2. **Skill 三副本同步**。现有约束是 `skills/innate-memory/SKILL.md` 与 `core/assets/SKILL.md` 必须字节一致（CLAUDE.md 已强调）。插件引入第三份 `plugin/innate/skills/innate-memory/SKILL.md`。
   - 对策：插件那份**由脚本从 `core/assets/SKILL.md` 拷贝生成**，不手工维护；CI 增加三方 `cmp` 校验。

3. **hook 重复触发**。最高危风险。若用户既跑过 `innate install`（hook 写进了 settings.json）又装了插件（hook 在插件清单），同一事件会触发两次 `innate hook stop`。
   - `hook.rs` 的事件写入应保持**幂等**（session.log 事件按 idempotency key 去重——daemon 已有 idempotent events 机制，需确认覆盖此场景）。
   - 同时在迁移文档里引导用户：装插件前先 `innate uninstall`（清掉 settings.json 里的 Innate hook/MCP），或 install 自动检测并清理。

4. **版本一致性**。`plugin.json.version` 要与 `core/Cargo.toml` 的 `version` 对齐。
   - 对策：release 流程（`641f25e` 那类 release commit）里加一步：bump Cargo 版本时同步 bump `plugin.json` 与 `marketplace.json`，CI 校验三者一致。

---

## 8. 验证清单（真机）

迁移前必须用真实 Claude Code 验证以下未决项（schema 细节以当前版本为准，勿凭记忆）：

- [ ] `plugin.json` 当前 schema 的确切字段名与可省略项（`mcpServers`/`commands`/`hooks`/`skills` 入口）。
- [ ] `marketplace.json` 的 `source` 是否支持仓库内相对路径 `./plugin/innate`。
- [ ] 插件 hook 的 JSON 结构是否与用户 settings.json 的 hook 结构一致（`{"hooks":[{"type":"command","command":...}]}`）。
- [ ] 插件声明的 MCP 工具默认权限行为（是否仍需 `permissions.allow`）。
- [ ] 插件 `command` 字符串能否依赖裸 `innate`（PATH 解析），还是必须绝对路径 / `${CLAUDE_PLUGIN_ROOT}`。
- [ ] hook 中 `innate hook prompt` 的 UserPromptSubmit 注入行为在插件加载下与现状一致。

> 验证方式：在一个干净的 Claude Code 环境 `/plugin marketplace add <本仓库本地路径>` → `/plugin install innate@innate` → 跑一个会话，确认：MCP 14 工具可见、6 个 `/innate-*` 命令可用、4 个 hook 触发、skill 激活、recall→record→evolve 闭环正常。

---

## 9. 实施步骤（里程碑）

**M1 — 脚手架与生成**
1. 新建 `plugin/innate/` 目录树（§3）。
2. 写 `.mcp.json`、`hooks/hooks.json`、`plugin.json`、根 `marketplace.json`。
3. 写 `scripts/gen-plugin-commands.sh`：从 `core/assets/SKILL.md` 生成 `commands/*.md` + 拷贝 skill。
4. 跑脚本，提交生成产物。

**M2 — 真机验证（§8）**
5. 本地 marketplace 安装，逐项打勾验证清单。
6. 修正 `plugin.json` / `hooks.json` 字段直到与真机行为吻合。

**M3 — 防冲突**
7. 改 `wizard.rs`：Claude 一端检测插件是否已装，已装则跳过 MCP/hook/command/skill 写入（只保留"装二进制 + 配 LLM/daemon"）。
8. 确认 `hook.rs` / daemon 的事件幂等覆盖"双重 hook"场景；补测试。

**M4 — CI 与发布**
9. CI 增加：跑生成脚本后 `git diff --exit-code plugin/`；三方 `cmp` skill 校验；`plugin.json`/`marketplace.json`/`Cargo.toml` 版本一致校验。
10. release 流程同步 bump 三处版本。
11. README / 设计文档增补「Claude Code 插件安装」章节，明确二进制依赖（§6.1）。

**M4 验收标准**：干净环境下两条斜杠命令完成接入，recall→record→evolve 闭环跑通，且与 `innate install` 不冲突。

---

## 10. 决策点（需确认）

1. **插件目录位置**：放本仓库 `plugin/`（单仓多产物，同步方便）✅推荐 / 还是独立 marketplace 仓库（分发干净，但 skill 同步跨仓）。
2. **是否保留 `innate install` 对 Claude 的写入能力**：建议保留但默认跳过（检测到插件即让位），仅在无插件环境兜底。
3. **二进制分发**：插件不带二进制是硬约束；是否在 hook/命令里加二进制探活提示？建议加。
4. **权限**：等 §8 真机结论决定是否还需引导用户加 `mcp__innate__*`。

---

*本文档为提案。落地前请完成 §8 真机验证，schema 细节一律以当前 Claude Code 版本实测为准，不得凭记忆固化。*
