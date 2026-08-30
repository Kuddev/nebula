# Nebula Runtime Control API v1

Nebula 的运行时控制面把 GUI、CLI、Agent 与未来插件统一到同一个状态权威。CLI 不会直接
读取窗口内部结构，也不会从终端标题猜测 Pane 状态；它和其他客户端一样，通过本机回环
连接发送带版本的 JSON Lines 请求，所有窗口、Tab 与 PTY 写操作再进入当前 UI 壳的 owner
线程执行；Git worktree 准备留在 Runtime 客户端工作线程，不阻塞 GPUI。

协议 Schema：[runtime-api-v1.schema.json](runtime-api-v1.schema.json)。

## 为什么先做这个

窗口、标签、Pane、AI 任务状态已经存在，但此前对外只有 Unix 配置 IPC 和 Windows
`PING/ATTACH` 单实例协议。继续分别添加 CLI、Agent、插件入口会复制状态读取、目标选择、
错误处理和权限边界。统一控制面后，新的消费者只需复用协议，不再进入 GUI 或 PTY 私有结构。

它直接带来四类价值：

- 自动化可以可靠读取 Window/Tab/Pane/TaskState，不再 OCR 屏幕或解析标题。
- Agent 可以聚焦、开标签、分屏、发送一行纯文本 Prompt，并等待语义状态。
- GUI 与外部客户端看到同一份 snapshot 和单调 revision，问题可以复现和审计。
- 后续插件、MCP、远程运行时可以在版本、权限和兼容错误已经明确的边界上继续建设。

## 环境契约

Nebula 打开的每个**本地** pane 都带上下面这些变量。pane 里的进程据此回答"我在哪、控制面在哪"，
不必扫进程树、读端口文件或 grep 源码：

| 变量 | 含义 |
|---|---|
| `TERM_PROGRAM=nebula` | 外面这层终端是 Nebula。沿用终端生态的事实标准入口，第三方工具的既有识别逻辑不必改代码 |
| `TERM_PROGRAM_VERSION` | Nebula 版本 |
| `NEBULA_PANE_ID` | 调用者所在 pane。AI hook 用同一个值给回合状态定位 tab 圆点 |
| `NEBULA_CLI` | 提供控制面的那个可执行文件的绝对路径。便携版不一定在 `PATH` 上，所以给精确路径 |
| `NEBULA_BIN_DIR` | 上者所在目录，同时被**前置**到 `PATH`，因此裸 `nebula` 也可用 |
| `NEBULA_PANE_REMOTE=1` | 这是 SSH pane。见下 |

实现在 `nebula_app/src/agent_env.rs`，两个壳共用同一个 `apply()`。三条设计约束：

1. **只进本地 PTY。** SSH pane 注入的是 `NEBULA_PANE_REMOTE=1`，不给 `NEBULA_CLI`/`NEBULA_BIN_DIR`
   ——远端主机上没有这个二进制，把本机绝对路径送过去只会造出一个"变量有了但永远执行不了"的伪能力，
   还会绕开远端 pane 不得取用本地上下文的护栏。
2. **幂等。** 在 Nebula 的 pane 里再开一个 Nebula 是常规操作（嵌套 shell、`wsl.exe`、隔离实例）。
   `PATH` 前置与 `WSLENV` 追加都按值判重，套多少层环境块都不增长。
3. **WSL 透传。** `WSLENV` 里带 `NEBULA_CLI/p`、`NEBULA_BIN_DIR/p`——`/p` 让 WSL 把 Windows
   路径翻成 `/mnt/...`，否则来宾 shell 拿到的是一个执行不了的 `D:\…` 字面量。合并以环境表里的
   现值为基准，因此与 `shell_detect::wsl_cwd_report_env` 的 cwd 上报条目互不覆盖。

`runtime.describe` 的 `env` 段回报这份契约（变量名不必由客户端硬编码），并带
`features: ["env.pane_identity", "cli.resource_verbs"]` 供旧版本探测后回落。

环境契约解决的是**可达性**，不是**该不该调**。后者属于 Skill
（`docs/skills/nebula-runtime/SKILL.md`）。两者不能互相替代：没有契约，Skill 里写的命令是空头
承诺；把发现层全押在 Skill 上（用户得手工安装一段提示词）则是不可靠的单点——所以 `nebula env`
本身也是发现入口，见下节。

## CLI

### 资源 + 动词

面向"模型在 pane 里临时要派个活儿"这一种用法。命名用 CLI 界的通用惯例（资源在前、动词在后，
同 `kubectl` / `docker` / `gh`）而不是自造词：别人看一眼就知道在做什么，比"短"更重要。

```powershell
nebula env --pretty                              # 我是谁、控制面在哪、有哪些命令

nebula pane list                                 # 所有 pane 压平成一维行
nebula pane read 17 --lines 80                   # 读某个 pane 的 Grid 尾部
nebula pane send 17 "cargo test" --wait          # 写一行并回车，然后等它跑完
nebula pane paste 17 --from-file task.txt --wait # 以 bracketed paste 发送有界多行文本
nebula pane wait 17 --after-seq 41               # 等某个 pane 静下来
nebula pane exec 17 -- cargo test                # 独立非 TTY 子进程，不改交互 shell 状态
nebula pane zoom 17 --zoomed true                # 显式缩放，不用 toggle
nebula pane resize 17 0.60                       # 修改直接父分屏中的目标占比

nebula agent list                                # 只有 AI CLI 的 pane
nebula agent send codex "修复登录回归" --wait    # 派任务并提交，然后等这一轮结束
nebula agent delegate codex "检查 vc skill"       # 派任务，完成后自动回传到当前 Agent 会话
nebula agent paste codex --from-file task.txt    # generation 绑定的多行任务输入
nebula agent read codex --lines 80               # 读它最近打印了什么
nebula agent wait codex --after-seq 41           # 等它这一轮结束

nebula window close 3                            # 关闭空闲窗口
nebula tab rename 2 tests --window 3             # 按窗口内零基索引重命名 tab
nebula tab move 2 0 --window 3                   # 同一窗口内移动 tab
```

它们是完整协议的**薄别名**：同一个请求函数、同一套响应信封、同样的 generation 与 `after_seq`
竞态保护，短的只是命令行，不是语义。

`pane` 与 `agent` 分成两个资源不只是为了好读——它们走的是**不同**的协议方法。Agent 路径带
generation 绑定（Codex 退出重开后不会把任务投给新会话），pane 路径没有这层保护。用两个资源名把
这条边界摆在命令行上，比塞进一个参数再靠前缀区分要难错得多：pane 用 `nebula pane list` 给出的
数字 id，Agent 用 `nebula agent list` 给出的名字或稳定 id。

`nebula env` 是发现层的实体：环境那半段**不依赖 runtime**，即使控制面没起来、端口文件过期、
或这根本不是 Nebula 的 pane，命令仍然成功返回并如实说明缺什么。一个探测命令若在"没连上"时整体
失败，调用方唯一能学到的就是"不知道"，只好去猜。它还随响应回报完整命令清单，每条给**可直接
复制执行**的样例而不是抽象签名——模型照抄一条完整命令的成功率远高于自己按参数表拼装。

`--wait` 的基线取自提交后的那张快照。用它当 `after_seq` 才让随后的等待意味着"又静下来了"而
不是"本来就是静的"，这正是把提交前的 idle 误判成"已完成"的那个经典竞态。`--no-submit` 与
`--wait` 是逻辑矛盾（没提交等什么），由参数解析直接拒绝，不会执行一半再静默停下。

`agent send` 与 `agent delegate` 的结果合同不同：前者只负责投递（可选同步等待），后者从
`NEBULA_PANE_ID` 记录调用 Agent 的位置和会话身份，登记并提交成功后立即返回。目标 Agent 的
完成 Hook 到达后，Nebula 把最多 4000 字符的结构化最终消息作为不可信 `worker_output` 自动提交
给原 Agent，由它向用户总结。回传只认原 pane 中的同一 managed generation，或普通 Agent 的
同一 provider session；不会退回当前焦点，也不会把旧任务交给同 pane 中后来启动的新会话。

### 完整协议

```powershell
nebula ctl describe --pretty
nebula ctl snapshot --pretty
nebula ctl orchestrate --file workflow.json --timeout-ms 30000 --pretty
nebula ctl agents --pretty
nebula ctl agent-fork --window <WINDOW_ID> --source-pane <PANE_ID> --name login-fixer --kind codex --pretty
nebula ctl agent-get --agent login-fixer --pretty
nebula ctl agent-prompt --agent login-fixer --generation 1 --text "修复登录回归" --pretty
nebula ctl agent-wait --agent login-fixer --generation 1 --state settled --timeout-ms 300000 --pretty
nebula ctl agent-read --agent login-fixer --generation 1 --lines 120 --pretty
nebula ctl read --window <WINDOW_ID> --pane <PANE_ID> --lines 120 --pretty
nebula ctl procs --window <WINDOW_ID> --pane <PANE_ID> --pretty
nebula ctl send-key --window <WINDOW_ID> --pane <PANE_ID> --key c --control --pretty
nebula ctl run --window <WINDOW_ID> --pane <PANE_ID> --command "cargo test" --pretty
nebula ctl exec-pane --window <WINDOW_ID> --pane <PANE_ID> -- cargo test --workspace
nebula ctl focus --window <WINDOW_ID> --pane <PANE_ID>
nebula ctl new-tab --window <WINDOW_ID>
nebula ctl split --window <WINDOW_ID> --direction right
nebula ctl prompt --window <WINDOW_ID> --pane <PANE_ID> --text "检查当前构建" --wait settled
nebula ctl wait --window <WINDOW_ID> --pane <PANE_ID> --state attention --timeout-ms 300000
nebula ctl subscribe --since <REVISION>
```

一次性命令输出一个完整响应 Envelope。`subscribe` 输出 JSON Lines：第一行是订阅确认，随后
每行是一个 `runtime.snapshot` 事件。语义内容未变化时不增加 revision，也不发送重复事件。

Pane ID 当前在 Window 内稳定，而不是进程内全局唯一。存在多个 Window 时，应同时传
`window_id` 与 `pane_id`；省略 Window 且目标不唯一会得到 `ambiguous_target`，不会猜测。

## 方法

| 方法 | 用途 | 关键参数 |
|---|---|---|
| `runtime.describe` | 读取应用版本、协议版本和能力列表 | 无 |
| `runtime.snapshot` | 读取完整运行时投影 | 无 |
| `runtime.orchestrate` | 一次提交强类型布局、PTY、命令与 Agent 首任务工作流 | `steps`, `on_error?` |
| `events.subscribe` | 从 revision 开始订阅状态变化 | `since_revision?` |
| `agents.list` | 只列出 Nebula 已识别的 Agent Pane、会话身份、状态和证据来源 | `window_id?` |
| `agent.start` | 在新 Tab 或指定既有 Pane 中启动命名 Agent | `window_id?`, `pane_id?`, `name`, `kind`, `cwd?`, `resume_session_id?` |
| `agent.fork` | 事务化创建独立 Git branch/worktree，再启动命名 Agent | `source_pane_id?`/`source_cwd?`, `name`, `kind`, `branch?`, `base?`, `path?`, `allow_dirty_source?` |
| `agent.get` | 按稳定 id 或名称解析 Agent generation 与 worktree provenance | `agent`, `generation?` |
| `agent.delegate` | 绑定调用方身份派发任务，并在目标回合完成后自动回传最终消息 | `agent`, `generation?`, `text`, `origin_pane_id` |
| `agent.prompt` | 向同一 Agent generation 发送纯文本 Prompt | `agent`, `generation?`, `text`, `submit?` |
| `agent.paste` | 向同一 Agent generation 发送受控 bracketed-paste 文本 | `agent`, `generation?`, `text`, `submit?` |
| `agent.read` | 读取命名 Agent 所在 Pane 的真实 Grid 尾部 | `agent`, `generation?`, `lines?` |
| `agent.wait` | 等待同一 Agent generation 的状态跃迁；被替换/退出即明确失败 | `agent`, `generation`, `state`, `timeout_ms`, `after_seq?` |
| `window.create` | 创建新窗口 | 无 |
| `window.close` | 关闭空闲窗口；忙碌 Pane 返回显式确认错误 | `window_id?` |
| `window.focus` | 聚焦窗口或 Pane | `window_id?`, `pane_id?` |
| `tab.new` | 创建默认 Shell 标签 | `window_id?` |
| `tab.close` | 按窗口内零基索引关闭空闲 Tab | `window_id?`, `tab_index` |
| `tab.rename` | 设置或清除 Tab 自定义名称 | `window_id?`, `tab_index`, `name` |
| `tab.move` | 在同一窗口内移动 Tab | `window_id?`, `tab_index`, `to_index` |
| `pane.split` | 从当前或指定 Pane 向右/向下分屏 | `window_id?`, `pane_id?`, `direction` |
| `pane.close` | 关闭空闲 Pane；忙碌 Pane 返回显式确认错误 | `window_id?`, `pane_id` |
| `pane.zoom` | 幂等设置 Pane 所在 Tab 的显式缩放状态 | `window_id?`, `pane_id`, `zoomed` |
| `pane.resize` | 设置目标 Pane 在直接父分屏中的占比 | `window_id?`, `pane_id`, `ratio` |
| `pane.prompt` | 写入一行纯文本，可追加 Enter | `window_id?`, `pane_id`, `text`, `submit` |
| `pane.paste` | 通过 bracketed paste 写入有界 UTF-8 文本 | `window_id?`, `pane_id`, `text`, `submit` |
| `pane.read` | 从真实终端 Grid 尾部读取最近逻辑行 | `window_id?`, `pane_id`, `lines` |
| `pane.procs` | 读取本地 PTY shell 为根的真实进程树 | `window_id?`, `pane_id` |
| `pane.send_key` | 按当前终端模式编码受限的命名控制键 | `window_id?`, `pane_id`, `key`, `modifiers?`, `repeat?` |
| `pane.run` | 运行单行命令，并以 OSC 133 返回真实 exit code | `window_id?`, `pane_id`, `command`, `wait?`, `timeout_ms?` |
| `pane.exec` | 在 Pane 的本地/WSL cwd 启动独立非 TTY argv 并捕获双流 | `window_id?`, `pane_id`, `argv`, `timeout_ms?`, `max_output_bytes?` |
| `pane.wait` | 等待 Pane 到达语义状态 | `window_id?`, `pane_id`, `state`, `timeout_ms`, `after_seq?` |

`pane.prompt` 有意拒绝换行、ESC 和其他控制字符，并限制为 32 KiB。它是 Prompt 接口，不是
任意终端字节注入接口。控制键走 `pane.send_key`：只开放命名键，字母必须配
`control=true`，`repeat` 上限 64；API 不接受任意 bytes 或 ANSI 字符串。

`pane.paste` 专用于确实需要保留换行的输入：只接受 UTF-8，限制 32 KiB，拒绝 ESC、NUL
与危险控制字符，并要求目标终端已启用 bracketed-paste。SSH Pane 明确拒绝本地文件/文本
转发。资源 CLI 可从字面文本、`--stdin` 或 `--from-file` 读取，但协议边界始终只接收验证后的
文本，不接收本地路径或句柄。

## 单请求编排

`runtime.orchestrate` 把模型已经确定的终端操作留在 Runtime 内执行，避免模型在每个步骤之间
重复读取 snapshot、搬运 Pane ID 和再次推理。它不是通用脚本语言：每个步骤都是 Schema 中
列出的强类型 `op`，步骤引用也是 `{ "step": "right", "field": "pane_id" }`，不执行字符串
模板、任意方法名或嵌套 Runtime 请求。

```json
{
  "steps": [
    { "id": "right", "op": "split", "direction": "left_right" },
    {
      "id": "weather", "op": "agent_launch",
      "target": { "step": "right", "field": "pane_id" },
      "name": "weather", "kind": "claude",
      "initial_prompt": "查询并简要回答今天的天气"
    },
    {
      "id": "bottom", "op": "split",
      "target": { "step": "right", "field": "pane_id" },
      "direction": "top_bottom"
    },
    {
      "id": "formula", "op": "agent_launch",
      "target": { "step": "bottom", "field": "pane_id" },
      "name": "formula", "kind": "codex",
      "initial_prompt": "输出几组复杂数学公式供终端渲染测试"
    }
  ],
  "on_error": "stop"
}
```

首个无 `target` 的 `split` 相对当前聚焦 Pane 执行，因此常见自然语言布局不需要先请求
snapshot。后续步骤只引用前面成功步骤的 receipt；未来引用、不存在的步骤、重复 ID、未知字段
都在任何 UI 动作发生前被拒绝。普通 Pane 不做隐式回滚：中途失败时已完成动作保留，receipt
返回 `failed_step`、逐步 `error` 和 `partial`，调用者可从失败点续作。

`agent_launch` 是“绑定既有 Pane 启动 Agent + ready 握手 + 投递首任务”的原子产品语义。
Runtime 先启动 workflow 中的所有 Agent，再并行等待各自被 registry 观察到且离开启动中的
`running` 状态，随后投递 `initial_prompt`。这让多个 Claude/Codex 的冷启动互相重叠，同时
避免启动命令尚在 submit barrier 中时第二次输入触发 `input_in_progress`。成功 receipt 只返回
`window_id`、`pane_id`、`agent_id`、`generation`、ready 状态和 `submitted` 等必要字段，不附带
每一步的完整 snapshot。

## 命名 Agent 与隔离 worktree

`agent.start` 提供稳定的 `agent_id + generation + name`。冷启动命令只开放经过各 Provider
官方 CLI 文档核实的 `claude`、`codex`、`opencode`、`cursor`、`pi`、`omp` 与 `kimi`；其中
Cursor 的实际可执行文件名是 `agent`。恢复/会话分叉只在对应 Provider 有已核实语法时出现，
不会由显示名或检测 slug 猜命令。`agent.fork` 在此基础上增加 Git 隔离，完整顺序是：

1. 由 `source_pane_id` 从 RuntimeHub 权威 snapshot 解析 cwd；没有 Pane 时必须提供绝对
   `source_cwd`。SSH Pane 明确返回 `remote_worktree_unsupported`。
2. 校验 Git、source dirty 状态、base commit、branch 与目标路径。默认拒绝 dirty source；
   只有调用者明确传 `allow_dirty_source: true` 才跳过该检查。
3. 在 Runtime 工作线程创建新 branch 与 worktree。默认 branch 为 `nebula/<agent-slug>`，默认
   目录为主仓库同级的 `<repo>-worktrees/<agent-slug>`；既存 branch/path 都返回冲突，不覆盖。
4. 把新 worktree 作为 cwd 交给真实 Tab/PTY 创建链，注册 Agent，再发送经过验证的启动命令。

成功响应和之后的 `agent.get` 都包含同一份 `worktree`：`repo_root`、`source_root`、`path`、
`branch`、`base_commit`、`created`。创建 Tab 或启动 Agent 明确失败时，Nebula 只删除本次事务
确认创建成功的 worktree 与 branch；已有目录、已有分支和用户其他 worktree 不在回滚范围。
若 UI dispatch 超时，结果处于未知态，服务端返回 `runtime_timeout`、
`details.cleanup_deferred=true` 和 worktree provenance，并保留 checkout，避免晚到的 Tab 使用
一个刚被删除的 cwd。

`agent.fork` 只创建隔离环境并启动 Agent，不抢跑派发任务。调用者取得响应中的 generation 后，
再用 `agent.prompt` 派活、`agent.wait` 等状态、`agent.read` 取结果；这样 CC 或另一个 Agent 能把
多个独立 worktree 作为并行工位调度，而不会让它们同时修改同一个工作树。

`agent.delegate` 是 Agent 间需要自动回传时的专用方法。服务端先验证 `origin_pane_id` 当前确实
运行着可稳定识别的 Agent，再把目标解析成精确 `agent_id + generation`，最后复用
`agent.prompt` 的真实 PTY 投递路径。Provider 的完成 Hook 不含 Nebula task id，因此同一目标
generation 同时只允许一个委派；重复完成事件在首个事件消费 pending task 后不会再次回传。
回调若早于发起方自己的回合结束，会先排队并由 Agent 状态看门狗重试；`running` 会继续等待，
`attention`/`waiting_input` 不会被自动输入覆盖。发起 pane 关闭、provider session 被替换或
managed generation 改变时，旧回调直接丢弃，不会寻找替代 pane。

## 进程、控制键与真实命令结果

`pane.procs` 在 Windows 通过 Toolhelp 从 PTY shell pid 遍历真实后代进程。原生 SSH 只看得到
本地传输进程，因而明确返回 `remote_process_unavailable`，不会冒充远端进程树。

`pane.run` 依赖 Shell integration 的 OSC 133 `CommandStart`/`CommandDone`。只有观察到
`CommandDone` 携带的真实 exit code 才返回 `finished`/`failed`；没有集成或没有 exit code 时
返回 `exit_code_unavailable`、`run_start_timeout` 或 `run_aborted`，绝不把未知结果伪造成 0。

`pane.exec` 与 `pane.run` 是两种刻意分开的执行语义：它直接接收 argv，不经过 shell 展开，
在 Pane 当前上报的本地 cwd 中启动独立 non-TTY child，不写入 Grid、history 或交互 shell
环境。WSL Pane 通过对应 distribution 和 guest cwd 执行；SSH Pane 返回
`remote_exec_unsupported`。stdout/stderr 始终并行排水，每条默认最多保留 1 MiB、可配置上限
16 MiB；响应的 `stdout`/`stderr` 是直接字符串，`capture` 分别报告 encoding、总字节数、保留
字节数和截断状态。超时会回收整个子进程树，并保留已捕获输出与 `timed_out: true`。

## Agent 状态与终端读取

`RuntimePane.agent` 只在 Nebula 能把当前程序归一为已知 AI CLI 时出现；普通长命令不会被
误列为 Agent。Agent 对象包含规范化 `kind`、显示名、hook 上报的可选 `session_id`，以及
`state_source`：

- `hook`：CLI hook 的生命周期边沿，是最高置信度事实。
- `screen`：声明式屏幕规则补偿漏失 hook；`state_rule` 给出命中的规则 id。
- `process`：只确认进程身份，不能把它提升成“已完成”或“需要批准”的强结论。

侧栏、托盘、`runtime.snapshot`、`agents.list`、`pane.wait` 共用同一个
`RuntimeTaskState` 投影。优先级为失败、attention、等待输入、运行、完成、空闲；
`RuntimeHub` 再对真正的状态跃迁盖 `state_change_seq`，因此外部客户端不需要自行解析标题或
终端文本来猜生命周期。

`pane.read` 直接锁定 pane 的 `Term`，通过 `bounds_to_string` 读取 Grid/scrollback；它不使用
截图或渲染快照，也不会改变用户的滚动位置、选区和光标。范围固定锚定 buffer 底部，所以用户
正翻看历史时结果仍稳定。`lines` 默认 120，范围 1..=2000，响应最多 1 MiB，并返回：

- `requested_lines` / `returned_lines`：请求与实际返回的终端逻辑行数。
- `history_available`：当前仍保留的 scrollback 行数。
- `truncated`：更早的 buffer 内容或超出响应字节上限的内容未返回。
- `task_state`、`exited`、`exit_reason?`：读取时的生命周期边界。

SSH pane 只有进入 `Ready` 后允许读取或写入。解析、连接、认证、打开 Shell 或 Failed 阶段会
返回 `ssh_not_ready`，避免把密码提示和连接错误屏误当成远端 Agent 的正常输出。

默认 GPUI 产品当前只有一个 workspace window，稳定 `window_id` 为 1。该窗口上的
snapshot/focus/tab.new/pane.split/pane.prompt/pane.read 都操作真实 workspace；GPUI 尚未建立
第二个拥有独立 hook/runtime 接收器的 workspace，因此 `window.create` 明确返回
`runtime_unavailable`，不会伪造一个新窗口 id。旧 winit 壳仍支持真实多窗口创建。

## 等待语义与 `state_change_seq`

每个 Pane 都带一个单调递增的 `state_change_seq`，只在 `task_state` 真正发生跃迁时 +1。它
存在的原因是「等待空闲」本身有竞态：命令提交后 Shell 需要几十毫秒才会转入 `running`，若
此时直接查询状态，Pane 仍是提交前的 `idle`，等待会立刻返回，调用方误以为命令已经跑完。
默认 GPUI 壳在 `pane.prompt` 成功提交 Enter 的同步动作里先建立 `running` 边沿，再由真实
Shell/hook 结束事件归位；因此即使命令在 120ms Runtime pump 的两拍之间完成，`--wait` 也能
观察到提交后的新 `state_change_seq`，不会把提交前的 `idle` 当成结果。

正确用法是先取基线、再等待跃迁：

1. 发 `pane.prompt`，从响应的 `snapshot` 中读出该 Pane 的 `state_change_seq`。
2. 把它作为 `after_seq` 传给 `pane.wait`，服务端就只承认 `state_change_seq > after_seq`
   的观察结果。

`nebula ctl prompt --wait` 已经在内部串好这两步。独立调用 `nebula ctl wait` 时需自己传
`--after-seq`；
省略则退回「立即匹配当前状态」的旧语义，仅适合观察一个已在运行的 Pane。

计数器按 (窗口, Pane) 记账，因为 Pane ID 只在窗口内唯一；序号从 1 开始，`0` 不是合法基线。
`runtime.describe` 的 `features` 含 `pane.wait.after_seq` 时表示服务端支持该参数——旧版本会
静默忽略未知参数并保留竞态，所以有严格要求的客户端应当检查这个特性串而非仅看 `capabilities`。

## 兼容与错误

请求必须声明 `protocol: "nebula.runtime"` 和 `version: 1`。协议名或版本不匹配时，服务端
返回 `protocol_version_mismatch`，并在 `details.supported_versions` 中列出可用版本。

常见机器可读错误码包括：

- `invalid_request`：Envelope 或 JSON 无效。
- `invalid_params`：参数类型、Prompt 内容或超时范围无效。
- `invalid_reference`：编排步骤引用了未来、失败或不含目标字段的步骤。
- `method_not_found`：方法不存在。
- `target_not_found`：Window 或 Pane 已消失。
- `ambiguous_target`：Pane ID 在多个窗口中重复，必须补 Window ID。
- `invalid_state`：当前标签类型不允许该动作，例如设置页不能分屏。
- `action_failed`：窗口、Shell 或 Pane 创建失败。
- `agent_name_conflict` / `agent_identity_mismatch`：名称已被活跃 Agent 使用，或 generation 已变化。
- `agent_exited` / `agent_replaced`：等待的稳定 Agent 身份已退出或被同 Pane 中的新会话替换。
- `origin_not_agent` / `origin_identity_unavailable`：委派来源不是 Agent，或尚无可绑定的 generation/session。
- `delegation_in_progress`：同一目标 generation 已有一个等待完成 Hook 的委派。
- `agent_ready_timeout`：Agent 未在期限内完成进程身份观察与启动就绪握手。
- `dirty_source`：`agent.fork` 的源工作树有未提交变更，且调用者未显式允许。
- `branch_conflict` / `worktree_path_conflict`：目标分支或目录已存在；Nebula 不覆盖。
- `git_unavailable` / `invalid_base` / `invalid_branch`：Git 能力或 revision/ref 校验失败。
- `remote_worktree_unsupported`：本地 Runtime 不能替 SSH Pane 创建远端 Git worktree。
- `remote_process_unavailable`：本地 Runtime 不能从 SSH 传输进程推导远端进程树。
- `exit_code_unavailable`：当前 Shell 没有提供可信的 OSC 133 exit code。
- `ssh_not_ready`：SSH Pane 尚未进入可安全读写的 Ready 阶段，或连接已经失败。
- `runtime_unavailable`：当前壳/生命周期没有该动作所需的真实 owner；不会伪造成功。
- `timeout`：`pane.wait` 未在期限内观察到目标状态。`details` 会带上 `after_seq` 与最后
  观察到的 `observed_state_change_seq`，用于区分「Pane 一直没动」和「跃迁了但没到目标态」。

## 本机边界

服务端只监听 `127.0.0.1`，发现文件位于 Nebula 数据目录下的 `runtime.port`，其中包含随机
token。无 token 的连接会被静默丢弃。该边界用于阻止其他本机用户误调用，不承诺抵御已经
能读取当前用户文件或注入当前用户进程的攻击者。

旧版 `PING/ATTACH` 文本请求仍由同一服务端接受，保证单实例与会话重新挂载不因协议升级
失效；新功能只通过版本化 JSON 协议开放。
