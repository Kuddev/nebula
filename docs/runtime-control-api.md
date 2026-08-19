# Nebula Runtime Control API v1

Nebula 的运行时控制面把 GUI、CLI、Agent 与未来插件统一到同一个状态权威。CLI 不会直接
读取窗口内部结构，也不会从终端标题猜测 Pane 状态；它和其他客户端一样，通过本机回环
连接发送带版本的 JSON Lines 请求，所有写操作再进入 winit 事件线程执行。

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

## CLI

```powershell
nebula ctl describe --pretty
nebula ctl snapshot --pretty
nebula ctl agents --pretty
nebula ctl read --window <WINDOW_ID> --pane <PANE_ID> --lines 120 --pretty
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
| `events.subscribe` | 从 revision 开始订阅状态变化 | `since_revision?` |
| `agents.list` | 只列出 Nebula 已识别的 Agent Pane、会话身份、状态和证据来源 | `window_id?` |
| `window.create` | 创建新窗口 | 无 |
| `window.focus` | 聚焦窗口或 Pane | `window_id?`, `pane_id?` |
| `tab.new` | 创建默认 Shell 标签 | `window_id?` |
| `pane.split` | 向右或向下分屏 | `window_id?`, `direction` |
| `pane.prompt` | 写入一行纯文本，可追加 Enter | `window_id?`, `pane_id`, `text`, `submit` |
| `pane.read` | 从真实终端 Grid 尾部读取最近逻辑行 | `window_id?`, `pane_id`, `lines` |
| `pane.wait` | 等待 Pane 到达语义状态 | `window_id?`, `pane_id`, `state`, `timeout_ms`, `after_seq?` |

`pane.prompt` 有意拒绝换行、ESC 和其他控制字符，并限制为 32 KiB。它是 Prompt 接口，不是
任意终端字节注入接口；以后若增加 raw input，必须单独定义权限和审计语义。

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

`nebula prompt --wait` 已经在内部串好这两步。独立调用 `nebula wait` 时需自己传 `--after-seq`；
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
- `method_not_found`：方法不存在。
- `target_not_found`：Window 或 Pane 已消失。
- `ambiguous_target`：Pane ID 在多个窗口中重复，必须补 Window ID。
- `invalid_state`：当前标签类型不允许该动作，例如设置页不能分屏。
- `action_failed`：窗口、Shell 或 Pane 创建失败。
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
