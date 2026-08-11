# Nebula 相对 Herdr 的能力缺口审计

> 状态：2026-08-11 本地源码审计。  
> Nebula 快照：`1.0.0`，`cb0b3891dbfa8ab6f46d7e71ac90b39b7cdfc422`。  
> Herdr 快照：`0.7.5`，`2a20e90a026936d0d5b96823d74e2e4fe13a166f`。  
> 范围：只比较 `D:/temp_build/nebula` 与 `D:/temp_build/herdr` 的本地内容；后续上游变化需要重新核实。

## 结论

Nebula 与 Herdr 不是同一种产品形态：

| 产品 | 当前核心形态 | 已形成的长板 |
| --- | --- | --- |
| Nebula | 自带窗口、渲染器、ConPTY 和远程客户端的 Windows 原生终端 | 统一的 Windows 输入与桌面体验、行内 ghost 补全、原生 SSH/SFTP、富内容查看、点击通知直达 Pane |
| Herdr | 运行在现有终端中的 Agent multiplexer 与后台 runtime | 稳定 CLI/socket API、可寻址的 Workspace/Tab/Pane/Agent、状态等待与事件订阅、worktree 工作流、进程外插件合同 |

因此，Nebula 最值得补的不是 Herdr 的 TUI 外观，而是其背后的**运行时控制面、状态权威、
可诊断性和安全工作流合同**。Nebula 已有的 Windows 原生终端、补全、SSH/SFTP 和富内容能力
必须保留，不能为了追平 Herdr 而退回“依赖外层终端提供产品体验”的形态。

本审计对应的路线图裁定见 [future_planning.md](future_planning.md#2026-08-11-herdr-对照后的学习裁定)。

## 自动补全边界

Herdr 的 `completion` 与 Nebula 的行内补全不是同一种能力：

- Herdr 支持 `herdr completion <shell>`，为 Bash、Elvish、Fish、PowerShell 和 Zsh 生成
  **Herdr CLI 参数补全脚本**。本地证据是 `../../herdr/src/cli/spec.rs:101-110`、
  `../../herdr/src/cli/completion.rs:1-85` 和覆盖五种 Shell 的测试
  `../../herdr/src/cli/spec.rs:1272-1285`。
- 在 Herdr `src/` 中没有发现由 Herdr 自己绘制、基于命令历史/常用目录/`PATH`/文件路径的
  fish 风格行内 ghost-text。Pane 内 Shell 自己的 Fish/Zsh/PSReadLine 补全仍然可以正常工作。
- Nebula 的 `nebula-completions`、`NebulaHistory`、`DirectoryHistory` 与输入/渲染路径已经实现
  终端宿主级补全；`Tab` 在没有 ghost 候选时仍交给 Shell 原生补全。相关实现见
  [`nebula-completions/src/lib.rs`](../nebula-completions/src/lib.rs)、
  [`nebula_app/src/nebula_history.rs`](../nebula_app/src/nebula_history.rs)、
  [`nebula_app/src/directory_history.rs`](../nebula_app/src/directory_history.rs) 和
  [`nebula_app/src/input/keyboard.rs`](../nebula_app/src/input/keyboard.rs)。

结论：终端级自动补全是 Nebula 的现有优势，不是需要向 Herdr 学习的缺口。Herdr 的 CLI
补全脚本可以作为未来 Nebula 公共 CLI 的配套能力，但不能替代现有 ghost 补全。

## 缺口总览

| 优先级 | 缺口 | 直接影响 | 路线图归属 |
| --- | --- | --- | --- |
| `P0` | 缺少供脚本和 Agent 使用的稳定公共控制面 | Agent 不能可靠读取 Pane、创建工作区、发送任务、等待状态或订阅事件 | `P0-3` / `T0-1` |
| `P0` | Agent 状态权威、检测证据和诊断合同不完整 | hook、进程、OSC、屏幕状态冲突时难以解释谁是事实源 | `P0-3` / `T0-1` |
| `P1` | Git worktree 仍未成为 Workspace 一等能力 | 多 Agent 并行修改同一仓库时缺少安全隔离闭环 | Git worktree Agent Fork |
| `P1` | Lua profile 之外缺少稳定扩展/动作合同 | Command Center、Recipe、MCP、Skill 容易各自建立调用和权限模型 | `T0-2` / 扩展合同 |
| `P1` | 本地 runtime 尚未形成可复用的远程附加协议 | 原生 SSH 能连接主机，但不能像本地一样控制远端 Nebula runtime | `P0-3` 稳定后扩展 |
| `P0-Q` | 发布版本、安装文档和协议文档缺少单一事实源 | 用户可能下载旧包，客户端/服务端不兼容时也缺少明确诊断 | 发布/协议/文档质量门 |
| `P2` | 平台与发布形态较重 | Windows 之外不可用，安装包依赖 helper、字体和目录结构 | 平台长期路线 |
| `P1` | 丰富 UI 的异步任务与副作用动作尚未统一 | 重复提交、不可取消、错误反馈和焦点行为仍可能不一致 | Task UX / Action Gate |

## 1. 公共 Agent 控制面

### Herdr 已有能力

Herdr 的 CLI 与 raw socket API 使用同一控制面，能够：

- 创建、列出、聚焦、重命名和关闭 Workspace、Tab；
- 拆分、交换、聚焦、缩放、读取、关闭 Pane，并向 Pane 发送输入；
- 启动、读取、提示、等待、聚焦 Agent；
- 订阅运行时事件并等待输出或状态变化；
- 输出与当前二进制匹配的 JSON Schema；
- 在客户端与服务端协议不匹配时返回机器可读错误。

本地证据：`../../herdr/website/src/content/docs/socket-api.mdx:6-47`、
`../../herdr/src/cli/spec.rs:5-44`、`../../herdr/src/api/mod.rs:20-90`。

### Nebula 当前边界

Nebula 内部已经有 IPC、会话驻留和 Pane 状态，但公开 CLI 的主要子命令仍是配置、通知测试、
AI hook 安装和 SSH；Unix `msg` 的公开消息也只有创建窗口、设置配置和读取配置。源码见
[`nebula_app/src/cli.rs`](../nebula_app/src/cli.rs)。这不是可供外部 Agent 依赖的稳定运行时 API。

### 学习方向

1. 服务端拥有 Workspace/Tab/Pane/Task 的身份与事实，GUI 只是一个客户端。
2. CLI wrapper、raw socket 和未来插件共用同一份版本化方法定义，不能各自维护行为。
3. API 使用中性的 runtime 名称，不把 sidebar、card、row 等 UI 概念写入协议。
4. 查询与控制分离；读操作默认无副作用，写操作具有稳定 action/request id、超时和幂等语义。
5. 最小控制面至少覆盖 `list/get/read/focus/split/send/prompt/wait/subscribe`。
6. Schema、错误码和协议版本随二进制发布；版本不匹配时保留 status/upgrade/recovery 路径。

### 第一阶段验收

- 一个无 GUI 的测试客户端能创建 Pane、读取最近输出、发送文本并等待语义状态变化。
- 两个客户端同时观察同一 Task 时得到一致身份与事件顺序。
- 客户端断开不会杀死 server-owned PTY；重连后可继续读取状态。
- 超时、客户端取消、重复 request id、服务端重启和协议不匹配都有确定、机器可读结果。
- API Schema 与实现由同一类型源生成，CI 检查示例和 fixture 没有漂移。

## 2. Agent 状态权威与可诊断检测

### Herdr 已有能力

Herdr 先识别 Pane 的前台进程，再为每个 Pane 选择一个状态权威：完整 lifecycle hook 可以成为
`idle/working/blocked` 的权威；不完整 hook 只提供 session identity，状态仍由底部实时屏幕
manifest 判断。检测读取 live bottom buffer，而不是用户滚动后的 viewport；manifest 支持本地覆盖、
远程更新、热重载和 `agent explain` 证据输出。

本地证据：`../../herdr/website/src/content/docs/agents.mdx:10-83`。

### Nebula 当前边界

Nebula 已经为 Claude、Codex、Pi、OpenCode 安装直接集成，并有 OSC/BEL、进程名和远端 hook
桥接等兜底信号。但这些事实主要服务当前侧栏和通知，尚未形成可由所有 UI/API 共同消费的
统一 authority reducer，也没有与 Herdr `agent explain` 同等级的用户可见诊断合同。

### 学习方向与验收

- 每个 Task 同一时刻只有一个 lifecycle authority；session identity、显示 metadata 和生命周期
  状态分开建模，不能因为某个 hook 能提供 session id 就让它覆盖全部状态。
- 每次状态转换保留来源、证据、时间、序列号和被抑制的竞争信号，乱序事件可确定归并。
- 检测规则使用可版本化 manifest，支持不重启应用的重载；规则匹配底部实时快照，不能被滚屏干扰。
- 提供 `explain` 结果：当前 Agent、最终状态、权威来源、命中规则、可见证据、fallback 原因、
  manifest 来源与版本。
- 覆盖异常退出、Esc 中断、审批结果、延迟 hook、子 Agent 结束、远端断线和返回 Shell 的测试。

## 3. Git worktree 作为 Workspace 一等能力

### Herdr 已有能力

Herdr 的 `worktree create/open/remove` 把 Git checkout 直接映射为 Workspace。创建时可复用已有
分支或从 base/HEAD 建新分支；打开已有 checkout 会复用对应 Workspace；删除显式调用
`git worktree remove`，脏 checkout 需要再次确认，且永不删除分支。

本地证据：`../../herdr/website/src/content/docs/cli-reference.mdx:139-146`、
`../../herdr/website/src/content/docs/configuration.mdx:84-97`。

### Nebula 当前边界

Nebula 已经在 [Command Center 提案](nebula-command-center-and-unique-features.md#git-worktree-fork)
和 [AI 会话/远程工作流 PRD](prd-ai-session-and-remote-workflow.md#96-git-worktree-agent-fork)
中定义 Git worktree Agent Fork，但当前源码还没有对应运行时命令或事务。

### 学习方向与验收

- Worktree 是带 Git provenance 的 Workspace，不是一次性 Shell 脚本。
- 创建前检查仓库、分支、路径和未提交修改；路径规范化后才允许进入事务。
- 失败只回滚本次创建且确认归 Nebula 所有的 checkout/分支，不碰既有路径、分支或用户修改。
- 关闭 Workspace 与删除 checkout 分开；删除分支始终是另一项显式操作。
- GUI、Command Center、CLI/API 调用同一个事务实现，并返回 workspace/tab/pane/task identity。

## 4. 扩展与动作合同

### Herdr 已有能力

Herdr 插件是进程外 manifest package，可声明 actions、event hooks、terminal panes 和 link handlers；
插件通过完整 CLI/socket API 回调 runtime。安装时校验 manifest、最低 Herdr 版本并展示信任预览，
同时明确说明第三方代码不在沙箱中，用户必须审核来源。

本地证据：`../../herdr/website/src/content/docs/plugins.mdx:6-52`。

### Nebula 当前边界

Nebula 的 Lua quick-launch profile 适合启动命名命令，但不能表达动作参数、目标 Pane、审批、
失败重试、任务状态和事件订阅。现有 Command Palette 仍以静态动作列表为主，缺口已经记录在
[Command Center 提案](nebula-command-center-and-unique-features.md#二现有面板缺少什么)。

### 学习方向与验收

- `Tool/Action Contract` 是 GUI、Lua、Recipe、MCP、Skill 和插件的唯一执行入口。
- 第一阶段只做本地 link/install 与进程外 manifest，不以公开市场为前置条件。
- manifest 包含稳定 id、版本、最低 Nebula 版本、平台、入口、事件、权限声明和配置目录。
- 安装预览列出构建/启动命令与权限；第三方进程不能直接获得系统凭据，只拿受限 reference。
- 插件动作进入相同审批、取消、超时、审计、幂等和 TaskState 流程，不能绕开宿主安全策略。
- 插件失败不得阻止 Nebula 启动，也不得破坏 server-owned runtime 状态。

## 5. 远程 runtime 与命名会话

Herdr 支持独立 named sessions，也支持本地 thin client 通过 SSH 附加远端 Herdr server；本地按键
配置和桌面能力可以在远端会话上工作。证据见
`../../herdr/website/src/content/docs/persistence-remote.mdx:16-68`。

Nebula 的原生 SSH/SFTP 在 Windows 工作站场景更完整，但“连接一台主机”不等于“附加远端
Nebula runtime”。合理学习顺序是：

1. 先稳定本地 server-owned runtime 与版本化协议。
2. 再让同一协议可以经受认证的 SSH transport 访问远端 runtime。
3. 保留原生 SSH/SFTP 作为普通远程开发能力，不用 thin-client 模式替换它。
4. named session 用于真正隔离的任务集合、socket、持久状态和生命周期，不把每个 Tab 都升级成 session。

远程协议在本地并发客户端、断线重连、版本协商和权限边界通过前不进入实现。

## 6. 发布、协议与文档一致性

Nebula 当前存在可直接验证的版本漂移：

- [`nebula_app/Cargo.toml`](../nebula_app/Cargo.toml) 与根 [CHANGELOG.md](../CHANGELOG.md)
  已是 `1.0.0`；
- [README.md](../README.md) 的发布下载示例仍写 `NebulaTerminal-v0.6.0-windows-x64.zip`；
- [docs/release-notes/unreleased.md](release-notes/unreleased.md) 仍把 `0.9.0` 称为当前版本。

Herdr 的本地仓库保存按版本冻结的文档快照，并由当前二进制输出匹配的 API Schema。这一点应作为
工程合同学习，而不是只修三处字符串：

- 版本号、产物名、下载链接、安装器 metadata、CHANGELOG 和文档快照来自同一发布输入；
- release CI 在发布前拒绝旧版本号、缺失资产、Schema 漂移和无效链接；
- 协议文档按版本冻结，当前/预览文档分流；
- 运行时报告应用版本、协议版本和状态文件版本，兼容性错误不能伪装成普通连接失败。

## 7. 平台与分发形态

Herdr 以单个 Rust 二进制运行在用户已有终端中，主要覆盖 Linux/macOS，并提供 Windows beta。
Nebula 当前聚焦 Windows，发布包还包含 ConPTY/AI helper、字体、文档和许可目录。这是原生能力
带来的成本，不应简单判定为架构错误，但需要明确：

- 不能在没有 Windows 输入、IME、DPI、通知和 SSH/SFTP 等价验收前仓促宣布跨平台；
- helper 缺失、版本不匹配或字体未安装时应可靠降级，并给出可恢复诊断；
- 安装器、便携包和源码构建共享运行时资源发现规则；
- 长期跨平台工作复用终端核心和协议，不把 Windows Win32 策略带入通用模块。

## 8. 丰富 UI 带来的业务状态债务

Nebula 比 Herdr 拥有更多图形化工作流，也因此承担更多异步状态。现有
[UI 质量审计](nebula-ui-quality-audit.md) 已指出：SFTP 有进度与取消，但文件/Git 刷新、
Markdown、SSH 建连、图片解码等没有统一 Task UX；Settings/SFTP/旧控件的键盘焦点未完全统一；
SSH 连接、保存、删除和上传下载也缺少统一 Action Gate。

学习 Herdr 的“状态与 runtime 分离、render 只投影状态”原则时，应落到 Nebula 自己的 UI：

- 后台工作统一进入 `TaskState`，明确阶段、进度能力、取消和最终结果；
- 副作用动作具有 `Idle/Running/Completed/Failed` 与 request id，Running 时禁止重复提交；
- GUI 不自行推断 Agent/传输/命令成功，全部消费 runtime 事实；
- 焦点、错误和恢复属于客户端展示状态，但不得复制业务状态机。

## 最值得学习的顺序

1. **版本化公共控制面**：让 Nebula 从“能观察 Agent 的终端”升级为“Agent 也能可靠使用的 runtime”。
2. **单一状态权威与 explain**：先让状态可相信、可解释，再扩展更多 Agent 图标和特判。
3. **事务化 worktree Workspace**：解决多个 Agent 并行修改时最直接的数据隔离问题。
4. **统一扩展/动作合同**：让 Command Center、Workbench、Recipe、MCP、Skill 和插件复用权限与任务模型。
5. **发布/协议/文档合同**：消除版本漂移，为未来多客户端和插件兼容奠定可信基线。

## 不应照搬

- 不把 Nebula 改成运行在其他终端里的 TUI；窗口、渲染、ConPTY 和桌面整合是现有优势。
- 不用 Herdr 的 CLI completion 脚本替换 Nebula 行内 ghost 补全；两者应并存。
- 不把原生 SSH/SFTP 降级成只有远端 runtime attach；两条工作流服务不同需求。
- 不在 `Tool/Action Contract`、权限、凭据代理和协议版本稳定前先建设公开插件市场。
- 不为了功能数量一次性复制 Herdr 所有 Agent manifest；先建立权威、证据和诊断合同。

## Herdr 本地证据索引

- 仓库与版本：`../../herdr/Cargo.toml:1-10`
- 产品边界：`../../herdr/README.md:25-32`
- CLI completion：`../../herdr/src/cli/spec.rs:101-110`、`../../herdr/src/cli/completion.rs:1-85`
- 公共控制面：`../../herdr/website/src/content/docs/socket-api.mdx:6-47`
- Agent 权威与检测：`../../herdr/website/src/content/docs/agents.mdx:10-83`
- 远程与命名会话：`../../herdr/website/src/content/docs/persistence-remote.mdx:16-68`
- Worktree：`../../herdr/website/src/content/docs/cli-reference.mdx:139-146`
- 插件合同：`../../herdr/website/src/content/docs/plugins.mdx:6-52`

上面的 `../../herdr` 路径是本地并列仓库证据，不是 Nebula 发布包的一部分。对外引用时使用固定提交
`2a20e90a026936d0d5b96823d74e2e4fe13a166f`，避免把后续 Herdr 行为误写成此次审计结论。
