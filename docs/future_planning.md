# Future Planning — 待做任务

> 登记尚未排期的功能任务。做完一项就把它移到 changelog 并从这里删除。

## 图片与代码阅读器

- 右侧文件树双击 PNG、JPG、JPEG、WebP 图片时，在 Nebula 内创建新的图片预览 tab。
- 图片预览使用独立 `image_viewer`，与 Markdown 中的图片渲染共用同一套解码和适配逻辑。
- 首个版本只做只读图片预览：保持比例并适配到内容卡片，沿用现有 tab 的切换、关闭和重载行为。
- 后续增加缩放、平移、原始尺寸、图片信息、动画图片和更多格式支持。
- 后续增加代码阅读器，再扩展为可编辑代码：语法高亮、行号、搜索、保存、未保存状态和原子写入。

## Windows Terminal 原生输入与窗口兼容路线

来源：2026-08-08 对照 Windows Terminal `#4999 Improved keyboard handling in Conpty`、
`TermControl`/`TerminalCore`/`TerminalInput` 实现，以及 WezTerm 的 `RawKeyEvent` 设计。
目标不是复制 WT 的 WinUI 窗口代码，而是吸收它“原始事实保留在原生后端、协议编码在终端层”
的数据流，确保 Windows 兼容性、跨平台扩展性和长期可维护性同时成立。

### 长期边界

- **Winit-first**：Winit 继续负责窗口生命周期、事件循环、多屏窗口迁移、DPI、IME 和跨平台
  事件抽象；本计划不以替换 Winit 为前置条件。
- **Native-first facts**：Windows 的 VKEY、扫描码、重复计数、extended/enhanced 位、左右
  修饰键、锁定灯状态和布局解析文本，必须在 Winit Windows 后端一次捕获后向终端适配器传递。
  应用层禁止把 Unicode 字符或物理键重新反推成 VKEY。
- **Text/key 分流**：布局、死键、AltGr、IME 产生的文本遵循操作系统文本路径；Enter、Tab、
  Backspace、方向键、功能键、独立修饰键和需要完整修饰状态的组合，遵循 Win32
  `KEY_EVENT_RECORD` 路径。
- **Capability-first**：只有终端请求 DECSET `9001` 并且 ConPTY 已启用 Win32 input mode 时，
  才发送 `CSI Vk;Sc;Uc;Kd;Cs;Rc_`；否则保持现有 VT、Kitty 或 UTF-8 兼容路径。
- **无 Shell 特判**：不能按 PowerShell 5.1、PowerShell 7、cmd、WSL、Codex 或单个符号增加
  分支。Shell 差异只能通过其公开的终端能力协商表现出来。
- **平台逻辑独立**：通用 `keyboard.rs` 只做事件分发和快捷键判断；Windows 输入编码、未来
  Linux/macOS 原始后端分别放在独立模块中，不把平台条件堆积到通用文件。

### 后续任务

- [ ] **Windows 原始事件完整性**：继续维护 WM_KEYDOWN/UP、WM_SYSKEYDOWN/UP、WM_CHAR、
  WM_DEADCHAR、焦点丢失合成事件之间的配对规则；覆盖重复按键、左右 Ctrl/Alt/Shift、
  AltGr、死键组合、IME 提交、NumLock/CapsLock/ScrollLock 和 Numpad 位置。
- [ ] **9001 协议端到端矩阵**：在 PowerShell 5.1、PowerShell 7、cmd、WSL、Codex 及普通
  Win32 console client 上验证普通 Enter、Shift+Enter、Ctrl+Space、Ctrl+Break、
  Ctrl+Alt 组合、功能键和字符释放事件；同时验证不支持 9001 的终端仍走旧路径。
- [ ] **Windows 版本与输入环境矩阵**：覆盖 Windows 10/11、不同键盘布局（US、中文、
  German AltGr、US-International dead key）、IME、远程桌面、高 DPI、多显示器热插拔，
  记录每个环境的原始事件和最终 PTY 字节序列。
- [ ] **ConPTY 生命周期契约**：把 `CreatePseudoConsole`、Win32 input mode 请求、焦点变化、
  resize、关闭和重连整理成单一状态机；对不支持或异常响应的 ConPTY 明确降级，不让协议状态
  污染普通 VT 输入。
- [ ] **Windows 原生窗口服务层**：保持 Winit 的窗口抽象，在 Windows 后端补齐 WT 风格的
  原生事实适配：`WM_DPICHANGED` 建议矩形、多显示器 DPI 缩放、工作区边界、窗口跨屏迁移、
  最大化/全屏状态和系统标题栏交互。适配层只向上提供稳定事实，不让渲染层直接依赖 HWND 消息。
- [ ] **Linux/macOS 同级后端**：沿用相同的 `RawKeyEvent`/文本提交契约，分别接入 Linux
  XKB/Wayland/X11 和 macOS NSEvent/输入法原生字段；平台后端负责事实捕获，通用终端适配器
  只消费抽象后的事件，不复制 Windows 规则。
- [ ] **性能与可维护性基准**：记录每次键事件的分配、事件循环延迟、PTY 写入批次和窗口重绘
  开销；优先复用 Winit 已有的键盘状态读取和布局缓存，禁止在热路径增加第二次系统消息钩子、
  `GetKeyboardState` 或 VKEY 反查。为协议编码保留纯函数单测，平台集成用真实事件回放测试。
- [ ] **诊断与回放工具**：增加仅在显式诊断开关下启用的原始事件日志和 CSI 回放文件，日志默认
  脱敏并关闭；回放必须能重现修饰键、重复次数、UnicodeChar 和 key-up 顺序，便于长期维护。
- [ ] **替换 Winit 的独立评估**：只有在 Winit 无法满足窗口、多屏、DPI 或 IME 契约时，才另立
  架构项目比较 Win32/Wayland/AppKit 原生窗口方案；不得把窗口库替换混入键盘修复或普通功能迭代。

### WT 参考落点

- `terminal/doc/specs/#4999 - Improved keyboard handling in Conpty.md`：9001 请求、
  `KEY_EVENT_RECORD` 字段和完整示例。
- `terminal/src/cascadia/TerminalControl/TermControl.cpp`：`OriginalKey()`、扫描码、
  extended 位、CharacterReceived 与 RawWriteChar 分工。
- `terminal/src/cascadia/TerminalCore/Terminal.cpp`：字符键交给操作系统文本事件，功能键和
  修饰键走原生 key event，并在字符事件到达时恢复 VKEY/扫描码关联。
- `terminal/src/terminal/input/terminalInput.cpp`：只格式化已经存在的
  `KEY_EVENT_RECORD` 字段，不从 Unicode 文本反推 VKEY。
- WezTerm 对应的 `RawKeyEvent` 和 `encode_win32_input_mode()` 作为跨平台事件模型与回放测试
  的补充参考，不作为 Nebula 的平台依赖。

## Explorer「在此处打开终端」右键集成（安装版）

来源：2026-07-28 用户需求（含截图，参照 Windows Terminal 的右键菜单项）。

- 目录背景与目录节点右键菜单加「在 Nebula 中打开终端」，带 Nebula 图标。
- 实现路径：安装器写注册表 `HKCU\Software\Classes\Directory\shell\Nebula`（含
  `Directory\Background\shell`），`Icon` 指向安装目录的 exe，命令
  `nebula.exe --working-directory "%V"`。
- 卸载时自动清理这些注册表键，不留孤儿菜单项。
- 覆盖安装：安装器需检测已有安装（同 HKCU Uninstall 条目），存在则原地更新
  （保留用户数据目录），而不是每次都装成并存的全新副本。

## MCP / Skill 内置化

来源：2026-07-28 用户需求（CLI-Manager 调研方向：provider/SSH key/skill/MCP 内置化）。

- 在 Nebula 内管理 MCP 服务器与 skill（安装/启停/配置），类似 CLI-Manager 的生态位。

## AI 供应商与 API Key 管理

来源：2026-08-07 用户需求（设置页面增加供应商菜单，统一管理 API Key）。

- 设置页新增独立「供应商」入口，管理 OpenAI、Anthropic、Google、OpenRouter、
  Azure OpenAI 及 OpenAI-compatible 自定义服务；每项包含显示名、Base URL、模型、
  API Key 状态、启用状态与连接测试。
- API Key 不写入 `nebula_settings.txt`、TOML、日志或会话快照。Windows 使用凭据管理器，
  其他平台使用系统 Keychain/Secret Service；普通配置只保存不含密钥的 provider id 和
  credential reference。
- 密钥输入只允许新增或替换，界面仅显示「已保存」和末四位，不提供明文回显或复制；
  删除供应商时明确询问是否同时删除系统凭据。
- 支持每个 workspace 选择默认供应商/模型，但 workspace 文件只保存 provider id，
  不复制全局密钥。插件、MCP 与未来 AI Assist 通过宿主凭据代理按权限引用密钥，
  不直接读取系统凭据库。
- 与「备份与恢复」功能联动：默认且推荐完全排除 API Key；用户主动勾选敏感凭据时，
  必须二次确认，并只允许写入带口令的 Argon2id + AES-GCM 加密备份包。
- 加入连接测试、限时状态、错误脱敏和失败重试；任何服务端响应、代理错误和调试日志
  都不得包含 Authorization header、完整 Key 或可还原的请求签名。

## CLI 输入预测

- 命令行输入预测/智能补全增强（在现有 ghost 行内补全基础上扩展：历史 + 语义预测）。

## Markdown 查看器选择复制

- doc 查看 tab 里支持鼠标选择文本并复制（当前仅渲染，无选区）。

## 图片预览器

- 终端内/查看器 tab 打开图片文件的预览（侧栏文件树/SFTP 双击图片可看）。

## 超大源码文件拆分（职责划分）

- 指本仓库工程健康：`display/mod.rs`、`display/settings.rs`、`input/chrome.rs` 等
  数千行大文件按职责拆成子模块，划清渲染/状态/命中/持久化边界。
- 不是查看器功能（2026-07-28 用户澄清，勿与文件查看混淆）。

## SSH 常用命令保存器

- 保存常用命令（可按主机分组），面板里一键执行到当前/指定 SSH 会话。

## 自动化脚本执行

- 在 Nebula 内定义/管理自动化脚本（一键在指定目录/主机跑既定命令序列）。

## 定时任务

- 定时触发命令或脚本（cron 式调度），结果可通知/记录。

## Markdown 查看器目录（TOC）

- doc 查看 tab 侧边显示标题目录，点击跳转。

## Markdown 内嵌 HTML 解析

- markdown 渲染支持常见内嵌 HTML（表格、img、details 等）的降级解析显示。

## CLI 消息通知开关进设置面板

- 现有 ai_hook 通知（tab dot + toast，写入 AI CLI 配置的 hook 字段）做成设置开关：
  默认开启；关闭时自动清理曾经写入的 hook 字段，不留残留配置。

## WebDAV 同步完善

- 现有同步状态、持久化与后端代码继续保留，但交互闭环尚未完善，暂时不在设置页显示。
- 完善连接校验、凭据状态、冲突处理、过程反馈与端到端验证后，再统一开放设置入口。

## AI Assist 完善

- 现有 ai_assistant / ai_hook 实现还不完善：保留现有代码继续迭代，
  暂不写入对外 changelog（不作为已发布功能宣传）。

## SSH 错误标记到 tab + 提示条真关闭按钮

来源：2026-07-28 用户反馈（截图：SSH 失败弹红条无法关闭）。已侦察，实现路线：

- 现状两阶段：创建失败走 `window_context.rs` `spawn_tab_ssh` 弹 message_bar 红条；
  异步连接失败走 `ssh_session.rs` `spawn_session`——`render_error` 打进终端后
  `terminal.exit()` 触发 `TerminalEvent::Exit` 直接关 tab（所以错误留不住）。
- 标 tab 方案：失败分支不 `exit()`、改发 `EventType::SshPaneFailed`（EventProxy
  自带 pane id）；window_context 记 `error_panes` 集合 → `sync_chrome_tabs` 收集
  → `set_chrome_tabs` 加 errors 参数 → `chrome.rs` tab dot 处画红点（现有蓝点旁）。
- 提示条：`[X]` 文本按钮其实已存在且可点（`message_bar.rs` CLOSE_BUTTON_TEXT、
  `mouse.rs` pop_message），但用户认不出——应改成 quad 绘制的真 × 按钮带 hover。

## Markdown 选中 → 评论 → 发送到指定 tab

来源：2026-07-29 用户提供的 otty 截图（composer.md L3 弹窗）。形态：

- 在 markdown 阅读器里选中一段文本，弹出小模态，标题是「文件名 + 行号」
  （`composer.md L3`），下面用只读引用块回显选中的原文（两行截断）。
- 「Send to:」是一个下拉，列出当前所有 agent tab（条目形如
  `OC | Writing composer docs`，右侧灰字标注 agent 类型 `OpenCode`）。
- 「Comment:」多行输入框。
- 底部三个动作：左下 `Copy Message`（只复制不发送），右下 `Cancel` / `Send`
  （Send 是唯一的 accent 主按钮）。
- 价值：把「读文档时发现问题」直接接到「让某个 agent 去改」，不用手动
  复制路径行号再切 tab 粘贴。

## SSH 代理（HTTP CONNECT / SOCKS5）

来源：2026-08-04 用户需求「访问境外小鸡有个代理方便得多，http 或 socks5，放到设置里面」。
已调研上层目录的 zap / tabby / wezterm / kitty，结论如下。

### 三家的做法各不相同

| 项目 | 实现方式 | 粒度 | UI 位置 |
| --- | --- | --- | --- |
| zap | 自实现 HTTP CONNECT（hyper）+ SOCKS5 | 全局 `ProxyMode: Off/System/Custom` + URL + 用户名密码 + `no_proxy` 绕过列表 | 独立「网络」设置页 |
| tabby | 调 russh 的 `newSocksProxy`/`newHttpProxy` | 每主机 `socksProxyHost/Port`、`httpProxyHost/Port` | SSH 主机编辑器 |
| wezterm / kitty | 不自己实现，走 OpenSSH `ProxyCommand`，spawn 外部进程 | 跟随 `~/.ssh/config` | 无 UI |

证据：
- `zap/app/src/settings_view/network_page.rs:33` 引入 `ProxyMode`，`:146`–`:154` 是
  Off/System/Custom 三态；密码不进 settings，单独走 `settings::network_secrets::ProxyCredentials`。
- `zap/crates/websocket/src/proxy.rs:97` `resolve_proxy()` 定义环境变量优先级
  （`HTTPS_PROXY`→`ALL_PROXY`），`:188`–`:256` 是 HTTP CONNECT 实现。
- `tabby/tabby-ssh/src/session/ssh.ts:392`–`:410` 按 profile 字段三选一建 transport；
  字段定义在 `tabby-ssh/src/api/interfaces.ts:34`–`:37`。
- `wezterm/wezterm-ssh/src/sessioninner.rs:329`–`:355`：Windows 上 `cmd /c <proxy_command>`
  + socketpair 接管 stdio。

**注意 zap 的代理只服务 HTTP/WebSocket（AI 请求），它的 SSH 并不走这套。** 三家里
只有 tabby 真把 SSH 接到了自建代理上。

### 建议给 Nebula 的形态：全局默认 + 每主机覆盖

单取任一家都有缺口——只做全局则「这台特殊」无解，只做每主机则十几台境外机要填十几遍。
取 zap 的全局三态模型（含 `no_proxy` 绕过列表，正好覆盖「国内机器不该绕代理」），
叠加 tabby 的每主机覆盖（跟随全局 / 强制直连 / 自定义）。

### 实现路线（零新依赖）

- **接入点**：russh 0.62.2 有 `client::connect_stream(config, stream, handler)`
  （`russh-0.62.2/src/client/mod.rs:995`），接受任意
  `AsyncRead + AsyncWrite + Unpin + Send + 'static`。自己完成代理握手，再把 stream
  交进去即可，不需要任何代理 crate。
- **改造点**两处 `client::connect`：`ssh_session.rs:529`（`authenticated_session`）
  与 `ssh_session.rs:737`（`test_connect`）。走代理时换成 `connect_stream`。
- **新模块** `ssh_proxy.rs`：SOCKS5 握手（RFC 1928，含 RFC 1929 用户名/密码认证）
  + HTTP CONNECT（含 Basic 认证）。两者合计约 150 行。
- **Cargo**：`tokio` 需补 `net` feature（`nebula_app/Cargo.toml:75` 当前只有
  `fs/io-util/rt-multi-thread/sync/time`；`TcpStream` 目前是靠 russh 传递启用的，
  别依赖 feature unification，显式写上）。
- **配置**：`SshProfileAuth`（`ssh_profiles.rs:39`）加 proxy 覆盖字段，沿用该文件
  已有的 `#[serde(default, skip_serializing_if)]` 向后兼容写法；全局那份放设置页。

### 两个必须记住的坑

1. **连接池 key 要带上代理身份**。`pool_key()` 现在只有目标地址，改了代理设置后
   会复用旧代理建立的连接，表现为「改了设置没生效」。
2. **SOCKS5 用域名模式（ATYP=0x03）把主机名交给代理解析，不要本地解析成 IP**。
   访问境外机器时本地 DNS 往往被污染或根本解析不到，本地解析等于代理白配。
   HTTP CONNECT 天然是域名形式，不受影响。

## 强杀 Nebula 时连带清理子进程（Job Object）

来源：2026-08-04 用户反馈「nebula 卡死强行关闭后，关联的 claude code / codex
必须一起关掉，否则内存泄漏」。

### 调研结论：Windows Terminal 也没做，我们遇到的是同一个病

- **Windows Terminal 正式代码不用 Job Object**，`AssignProcessToJobObject` 只出现在
  测试代码里（`terminal/src/host/ft_host/`、`WindowsTerminal_UIATests/`、
  `src/tools/closetest/`）。生产路径靠 `ConptyClosePseudoConsole`
  （`terminal/src/winconpty/winconpty.cpp:586`）+ `_piClient.reset()`
  （`ConptyConnection.cpp:543`、`:696`）。这正是 wt 被任务管理器强杀后 pwsh 常常还活着的原因。
- kitty、Kaku 同样没做。
- 真正做了的是下面两个 Rust 项目，**写法可以直接抄**。

### 抄法一：zap（用 `win32job` crate，最省事）

`zap/crates/command/src/windows.rs:81`–`:119`：

```rust
fn create_internal(self) -> Result<(), win32job::JobError> {
    let job = win32job::Job::create()?;
    let mut info = job.query_extended_limit_info()?;
    info.limit_kill_on_job_close();
    info.limit_breakaway_ok();
    if !self.kill_children_on_close {
        info.limit_silent_breakaway_ok();
    }
    job.set_extended_limit_info(&info)?;
    if self.assign_current_process { job.assign_current_process()?; }
    if let Some(process) = self.assign_process { job.assign_process(process)?; }
    Box::leak(Box::new(job)); // 句柄活到进程结束
    Ok(())
}
```

启动时一句 `JobObject::new().kill_children_on_close().assign_current_process().create()`
就把整棵子进程树纳管（`windows.rs:111`–`:119`）。

**它踩过的坑写在 `windows.rs:41`–`:43`**：
> assigning some processes to jobs that already contain other processes (i.e. `pwsh.exe`)
> 会失败，所以一个 job 只放一个进程。

配合 `limit_breakaway_ok()` 理解——这两条是同一个问题的两面，实现时别漏。

### 抄法二：CLI-Manager（纯 `windows-sys` 手写，无新依赖）

`CLI-Manager/src-tauri/src/process_job.rs` 全文约 60 行：`CreateJobObjectW` →
`JOBOBJECT_EXTENDED_LIMIT_INFORMATION.BasicLimitInformation.LimitFlags =
JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` → `SetInformationJobObject` →
`AssignProcessToJobObject(job, child.as_raw_handle())`，`Drop` 里 `CloseHandle`，
另给一个 `terminate()` 走 `TerminateJobObject` 主动收割。每个 child 一个 job。

**Nebula 该选这条**：`windows-sys 0.59` 已经在依赖里（`nebula_app/Cargo.toml:129`），
只需给 features 列表补 `"Win32_System_JobObjects"`，不引入新 crate。

### 落点

ConPTY 的 `CreateProcessW` 在 `nebula_terminal/src/tty/windows/conpty.rs:274`。
需要决定是「每个 pane 一个 job」（对齐 zap/CLI-Manager 的单进程单 job，避开 pwsh 冲突）
还是「进程级一个大 job」（最省事但撞 pwsh 问题）。倾向前者。

另需确认 mux 驻留进程是否要排除——若保留「关掉窗口、会话还在后台」的用法，
那条链不能进 job。

### 顺带澄清一个误解

Job Object **不会让任务管理器里的内存显示变大**。它是内核对象，本身占几 KB 内核内存，
不计入任何进程的工作集；任务管理器把子进程折叠到父进程下累加显示，依据的是父子关系与
AUMID，与 job 无关。真正让内存变大的恰恰是现在孤儿进程不死的状态。

## 数学公式渲染管线统一（终端侧对齐阅读器侧）

> **2026-08-04 已落地**（terminal_math.rs / math/mod.rs / math/compile.rs）：
> 分方向溢出预算 `BLEED_INTO_BLANK=0.45` / `BLEED_INTO_PROSE=0.15`，clip 跟
> 预算走；display 公式右侧空白时放开到视口宽（`widen_right`）；最小字号并入
> `math::MIN_READABLE_MATH_PX`；另加 `math::OPTICAL_SCALE=1.21`（KaTeX 同款
> 补偿，Latin Modern x-height 0.431 vs 等宽 0.53–0.56）在 `compile_formula`
> 单点生效，缓存键保持名义字号。实测：夹在正文中的公式墨迹与邻行字形零重叠
> （像素级验证），空行邻居的 display 块与 inline 均达到正文光学尺寸。
> 已知边界：单行 display 公式被正文直接上下夹住时仍受 1.3 行预算限制而缩小
> ——这是「不许压正文」的物理上限，真实 AI 输出的 `$$` 块前后几乎总有空行。

来源：2026-08-04 用户要求「公式和文字重叠 + 显示小」「一套管线需要统一起来，
公式最好随着字体大小变化，而不是走独立的大小」。

### 现状：两套尺寸决策

| | 文件 | 尺寸决策 | 可用垂直空间 |
| --- | --- | --- | --- |
| 阅读器 | `display/markdown_view.rs` | `fit_math_run():520` 只在**宽度**不够时缩 | 行高 `BODY_LINE = 1.55`（`:60`），充足 |
| 终端 | `display/terminal_math.rs` | `prepare_overlays():1262` 宽高**同时**参与 fit | 行高固定 1.0 cell |

终端侧 `fit = min(可用宽/渲染宽, 可用高/渲染高, 1.0)`，而**可用高度取决于源码占了几行**
——同一条公式写成一行还是三行，渲染出来就是两种大小。这就是「忽大忽小」与「显示小」的根源。
inline 公式尤其明显：`$x^2$` 的上标让 `metrics.height` 约 1.2 cell，除以 1 行可用高度
直接缩到 83%，永远比正文小一圈。

### 上一轮的修法为什么造成重叠

`MAX_VERTICAL_BLEED = 0.6`（`terminal_math.rs:40`）**无条件**给上下各 0.6 行溢出预算，
同时 `draw_overlays` 的 clip 改成跟着墨迹放开（`:1379`、`:1381`）。但：

- `CoverageMask::build()`（`:1292`）只把 `overlay.spans` 里的**源码行**登记为「跳过绘制」，
  溢出到的邻行没有登记；
- 于是公式墨迹和邻行正文字形画在同一片像素上 = 重叠。

而 `expand_top` / `expand_bottom`（`:896`–`:918`）**已经算出邻行是否整行空白**，
却只用来挪居中盒，没有参与溢出决策。这是现成的信息被浪费。

### 统一方案：字号锁定终端字号，只有宽度真放不下才缩

1. **字号 = 终端字号**。基础设施已就绪：`display/mod.rs:5480` 传的
   `math_pixel_size = glyph_cache.font_size.as_px()`，本来就跟随 Ctrl+滚轮，
   是 `fit` 把它缩回去了。
2. **高度不参与 fit**，改为分方向的溢出预算，由 `expand_top`/`expand_bottom` 决定：
   - 邻行整行空白 → 允许溢出约 0.45 行（`bounds()` 已借走半行，剩下的半行给它用完，
     不会碰到再上一行的墨迹）；
   - 邻行有正文 → 只允许约 0.15 行，即只吃行间空隙（终端字形的墨迹不填满 cell），
     再多就压到正文上。
   fit 的分母改成「bounds + 该方向允许的溢出」——实践中 `$$` 前后几乎总有空行，
   于是绝大多数公式直接拿到终端字号，不缩。
3. **clip 跟溢出预算走，不再跟墨迹走**。超预算的部分宁可裁掉也不许压到正文。
4. **宽度**：display 公式独占整行，可用宽度不该限制在源码 span 内，允许从
   `bounds.left` 向右用到视口边界，宽度也就几乎不再触发缩放。

### 顺手该合并的重复定义

`MIN_MATH_PIXEL_SIZE`（`terminal_math.rs:31`）与 `MIN_FITTED_MATH_PX`
（`markdown_view.rs:62`）是同一个语义、同一个值 6.0，应该收进 `math/` 下共用一份。

## 子代理等待状态与主任务进度不同步

来源：2026-08-07 用户截图反馈。等待子代理时，过程记录先显示
`Finished waiting / No agents completed yet`，主任务却仍持续显示 `Working` 数分钟，
两套状态互相矛盾，用户无法判断仍在执行、已经超时，还是等待流程已经退出。

- 等待结束但没有代理完成时，必须明确区分“超时”“被打断”“仍有代理运行”，不能统一显示
  `Finished waiting`。
- 主任务状态应与 agent 列表使用同一份运行事实；没有活跃 agent 时不得继续显示
  `Waiting for agents`，仍在执行本地工作时应切换成对应的工作状态。
- 显示活跃 agent 数、已等待时长和最近一次状态变化，并提供可立即停止等待的操作。
- 增加状态机测试，覆盖零 agent、部分完成、全部完成、超时、用户打断及 agent 异常退出。

## 备忘

- （空）
