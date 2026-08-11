# Future Planning — 待做任务

> 登记尚未排期的功能任务。做完一项就把它移到 changelog 并从这里删除。

## 2026-08-09 产品优先级总览

详细产品范围、验收指标与分期背景见
[PRD：AI 会话指挥中心与远程开发工作流](prd-ai-session-and-remote-workflow.md)。
相对 Herdr 的本地源码证据、能力缺口和不应照搬的边界见
[Nebula 相对 Herdr 的能力缺口审计](herdr-gap-analysis.md)。
2026-08-09 进一步复核现有产品能力与运行时需求后，产品定位和前三项优先级按本节更新；
PRD 中与本节冲突的旧优先级不再作为排期依据。

本节是当前功能排期的统一口径；下方各章节继续保存需求细节、调研证据和实现备忘。
若下方条目未在标题中写明优先级，以本节为准。

### 产品定位裁定

Nebula 的目标是成为**最出色的传统 AI 终端**：终端始终是主工作表面，AI 是原生执行层，
SSH 是附带连接能力而不是产品身份。前三项投入必须直接增强本地与远端共用的终端、AI 和
长期任务体验，不以追平通用 SSH 客户端的功能全集为目标。

行业实践已经证明，自然语言转命令、失败修复、原生 AI Chat、工具循环、审批和长期记忆
只是基础能力，不能再用较浅的 Command Center 代表整个 AI 产品。长期任务还要求
server-owned Pane/进程状态、单一 Agent 状态权威、状态向上汇总、detach/reattach、冷恢复和
Agent 原生 resume 构成同一个运行时，不能拆成互不相干的面板功能。

因此作出三项调整：

1. “AI Command Center”升级为完整的 **Native AI Workbench / Agent Engine**；
   Command Center 只保留为搜索、跳转和动作发现入口。
2. 把高频、低延迟的 **Intelligent Command Loop / Failure Intelligence** 单列为第二产品支柱，
   避免日常命令被深度 Chat 的重量和延迟淹没。
3. 把 Task Runtime、Flight Control、Attention Router、常驻会话与 Semantic Resume 合并为
   **Agent Runtime & Session Continuity**，共用任务身份、事件日志和状态权威。

### 优先级定义

| 优先级 | 含义 | 排期规则 |
| --- | --- | --- |
| `P0` | 核心定位、正确性和可靠性门槛 | 当前主线优先；未达到验收线前，不以同类新功能分散投入 |
| `P0-Q` | P0 并行质量门 | 不增加产品支柱；持续守住传统终端输入、渲染和平台兼容基线 |
| `P1` | 扩展核心 AI 工作流 | 在相关 P0 地基稳定后进入；不得绕过 P0 的权限和任务合同 |
| `P2` | SSH 附带能力、跨设备、阅读体验和高级协作 | 不作为当前定位成立的前提，按依赖与用户价值排期 |
| `P3` | 机会项或当前非目标 | 需要单独 PRD、安全评审和明确维护预算后才进入正式排期 |

优先级表示产品顺序，不等同于固定发布日期。

### 三个最优先产品任务

| 顺序 | 产品任务 | 核心范围 | 为什么现在做 | 退出/验收门槛 |
| --- | --- | --- | --- | --- |
| `P0-1` | **Native AI Workbench / Agent Engine** | 原生流式对话；Pane、选区、命令失败、cwd、Git 上下文；文件/搜索/Shell/项目工具；多轮 Agent loop；审批、取消、失败恢复；会话与受控记忆；Provider 和系统凭据 | 简单命令生成已是基础能力；Nebula 必须先拥有能完成真实任务的原生 AI 执行面 | AI 可在一个可取消、可审批的流程中读取项目、运行命令、处理失败并修改文件；变更必须经过策略或用户授权；终端输出不能升级为系统指令；密钥不明文落盘 |
| `P0-2` | **Intelligent Command Loop / Failure Intelligence** | `#` 自然语言转命令；失败解释与修复；命令预览和风险；历史、补全与 AI 建议统一排序；选中输出后解释/修复；当前 Pane、新 Tab、Split 执行目标 | 这是传统 AI 终端最高频、最低摩擦的使用路径，不能要求用户每次进入 Chat | 生成命令只进入可编辑输入区且绝不默认执行；失败上下文能准确携带命令、退出码、cwd、Git 和环境；高风险动作必须确认；可无复制地升级到 Workbench |
| `P0-3` | **Agent Runtime & Session Continuity** | server-owned `TaskIdentity/TaskRuntime`；单一状态权威；版本化 CLI/socket API、事件订阅与 JSON Schema；Pane/Tab/Workspace 汇总；Attention Inbox；实时 detach/reattach；冷恢复；Agent 原生 resume；`live/lost/rebuildable` 边界 | 这是 Nebula 超过普通 AI Chat 终端的长期差异化，也是真实后台 Agent、注意力、自动化和恢复能力的共同地基 | 侧栏、Tab、通知、Workbench、Command Center、脚本/Agent 客户端和恢复快照消费同一状态；外部客户端可 `read/prompt/wait/focus/split/subscribe`；24 小时驻留和 100 次 detach/attach 基准达标；重启后不把重建伪装成仍存活；协议不匹配返回可恢复的机器可读错误 |

产品编号表示用户价值和交付顺序，不代表先画 `P0-1` 的聊天界面。三个产品任务共用的底层合同必须先落地，否则后续 UI 会形成第二套任务状态、第二套命令事实和不可审计的工具执行路径。

### 最先攻克且最难的三个底层点

| 工程顺序 | 底层难点 | 为什么必须先做 | 难点实质 | 第一阶段完成定义 |
| --- | --- | --- | --- | --- |
| `T0-1` | **统一 TaskIdentity、Runtime Authority、事件日志与公共投影** | 三个产品任务以及未来脚本/Agent 客户端都需要稳定地回答“谁在什么环境做什么、现在是什么状态”；身份、事实源或 API 后补会导致 Pane、Chat、通知、恢复和插件全面迁移 | Pane 会关闭或重建，PTY/CLI hook/进程/SSH 会给出互相冲突且乱序的事实；还要让 GUI、CLI、socket 和订阅者在重连、重复事件与崩溃恢复后看到同一状态 | 定义不可复用的 task/session/run id；每类状态只有一个权威 reducer；事件可去重、重放并形成确定快照；通过同一类型源生成版本化请求/响应/事件 Schema；覆盖 working/blocked/done/idle/unknown/lost、超时、打断、异常退出、重复 request id、客户端断开和协议不匹配 |
| `T0-2` | **AI Tool/Action Contract 与审批事务内核** | Workbench 和 Command Loop 都会产生真实副作用；先做 Chat 再补权限、取消和审计会把高风险逻辑散落到 UI 与各工具 | 流式模型、并行或后台工具、用户审批、超时、取消、部分成功和重试共同构成分布式事务；Shell 不能承诺普遍回滚 | 所有工具使用版本化 schema 和稳定 action id；读/写/执行/网络分级；审批绑定参数摘要与作用域；取消、超时、重复回调和进程遗留有测试；审计记录脱敏且不保存密钥 |
| `T0-3` | **结构化命令生命周期与可信上下文边界** | 没有准确的命令、退出码、cwd、Git、选区和输出边界，命令修复只是猜测，Agent 也无法安全使用终端上下文 | 终端字节流本身不提供可靠语义，Shell、ConPTY、远端 SSH 和全屏 TUI 行为不同；屏幕输出还可能携带 prompt injection 和秘密 | 通过 shell integration/hook 与 PTY 事实建立 command id、开始/结束、退出码和 cwd 事件；保留原始来源与截断标记；终端输出按不可信数据注入；敏感内容可脱敏；本地、SSH、失败、取消和 TUI 夹具通过 |

技术依赖主链为 `T0-1 -> T0-2/T0-3 -> P0-1/P0-2 -> P0-3 完整可视化与恢复闭环`。
`T0-2` 与 `T0-3` 的 schema 可以并行设计，但都必须引用 `T0-1` 的 task/session/run id。

### 2026-08-11 Herdr 对照后的学习裁定

Herdr 最值得学习的是 runtime 合同，不是终端内 TUI 形态。完整证据与差距见
[Herdr 能力缺口审计](herdr-gap-analysis.md)。以下五项进入当前路线图：

| 顺序 | 学习项 | Nebula 的落地方式 | 验收门槛 |
| --- | --- | --- | --- |
| `L1 / P0` | **CLI、socket、事件订阅共享一个版本化控制面** | 在 `T0-1` 的 server-owned 状态上建立中性 runtime API；GUI 只是客户端之一，脚本、Agent、插件复用同一方法和 Schema | 无 GUI 客户端可创建/读取/聚焦/拆分 Pane，向 Agent 提示并等待语义状态；两客户端观察到一致身份与事件顺序；断线不杀 PTY；协议不匹配可诊断、可升级 |
| `L2 / P0` | **一个 Task 只有一个生命周期权威，并可 explain** | 分离进程识别、session identity、显示 metadata 与 lifecycle authority；hook、OSC、屏幕 manifest 和超时事实进入同一 reducer | explain 输出最终状态、权威来源、命中证据、fallback 原因、manifest 来源/版本；滚屏不改变 live 检测；延迟/乱序 hook 与返回 Shell 有状态机测试 |
| `L3 / P1` | **Git worktree 是 Workspace 一等实体** | GUI、Command Center、CLI/API 共用事务化 `create/open/remove`；Workspace 保存 repo/worktree/branch provenance | 创建前检查分支/路径/脏状态；失败只回滚本次拥有的产物；关闭 Workspace 不删除 checkout；删除 checkout 不删除分支；返回稳定 workspace/tab/pane/task id |
| `L4 / P1` | **进程外 manifest 扩展复用完整 Action Contract** | Lua、Recipe、MCP、Skill 与本地插件统一到版本化 action、权限、凭据代理和 TaskState；先支持可信本地 link/install，不以前置公开市场为目标 | manifest 校验最低版本、平台、入口、事件和权限；安装预览真实命令；取消/超时/审批/审计不能被插件绕过；插件失败不阻塞 Nebula 启动 |
| `L5 / P0-Q` | **发布、协议和文档是同一个版本合同** | Cargo、二进制 metadata、安装器、资产名、README、CHANGELOG、API Schema 和版本文档由单一发布输入生成或校验 | CI 拒绝旧版本号、缺失资产、Schema/示例漂移与无效链接；协议文档按版本冻结；当前/预览文档分流；运行时报告应用/协议/状态版本 |

明确不学习的部分：不把 Nebula 改成依赖外层终端的 TUI；不以 Herdr 的 CLI completion 脚本
替换现有行内 ghost 补全；不以 remote runtime attach 替换原生 SSH/SFTP；不在权限、凭据代理和
协议版本稳定前先建设公开插件市场。

### 其余功能包排期

| 优先级 | 功能包 | 排期裁定 | 前置依赖 | 退出/验收门槛 |
| --- | --- | --- | --- | --- |
| `P0-Q` | Windows 原生输入、ConPTY 与窗口兼容路线 | 作为传统终端可信度的长期质量门，与三大产品任务并行维护，不占用第四个产品支柱 | 原始事件事实、9001 协议、平台适配层 | 协议/布局/IME/多屏矩阵通过；不支持 9001 时正确降级 |
| `P0-Q` | 发布、协议与版本文档一致性 | 先修当前 `1.0.0` / `0.9.0` / README `0.6.0` 漂移，再把一致性固化进 release CI；不能靠每次发布人工搜字符串 | 单一版本源、协议版本、Schema 生成、版本化文档 | Cargo/二进制/安装器/资产/README/CHANGELOG 一致；CI 检查旧版本引用、资产存在性、Schema fixture、链接和协议兼容错误 |
| `P1` | Git worktree Agent Fork、Cross-Pane Context Bus、Markdown 评论发送 | 在 Runtime 与 Action Contract 上扩展深度 Agent 协作；worktree 先成为带 Git provenance 的 Workspace 一等实体 | 任务 ID、Git 检查、内容脱敏、目标权限、runtime API | worktree `create/open/remove` 事务覆盖脏状态、路径/分支冲突和失败回滚；关闭不删除 checkout、删除不删除分支；上下文创建和发送前可预览目标与范围；接收内容不会未经确认执行 |
| `P1` | MCP / Skill、受控 Recipe 与本地插件合同 | 扩展 Agent 能力，但必须复用宿主权限、凭据代理、版本化 Action Contract 和公共 runtime API；公开市场仍为 P3 | Tool/Action Contract、Provider、secret store、runtime API | 本地 link/install、manifest 版本/平台/权限校验、信任预览和进程外执行可用；安装/启停/升级可回滚；插件不能绕过审批或直接读取系统密钥；失败不阻塞宿主启动 |
| `P2` | Host Chain / ProxyJump、端口转发、可靠 SFTP、SSH 图片粘贴、轻量主机组织 | SSH 是附带能力；保留可靠连接闭环，但不与三个 P0 支柱争抢主线资源 | 统一 host id、逐跳凭据、SSH runtime、独立传输事务 | 多跳可定位失败 hop；转发状态与真实 listener 一致；1 GiB 传输可恢复且失败不留下半个最终文件；本机图片可安全上传并引用 |
| `P2` | WebDAV、图片/代码阅读、Markdown 增强、外观 | 提升跨设备与阅读体验，不作为 AI 终端定位成立的前提 | 同步冲突模型、查看器状态、统一焦点 | 功能完整、键盘可达、错误可恢复；同步冲突可处理；大文件不阻塞 UI |
| `P2` | 大文件模块按职责拆分 | 只由实际 P0/P1 修改牵引，降低核心功能变更风险 | 明确模块所有权和行为基线 | 行为与性能无回归，边界有测试；不做无业务收益的纯搬运 |
| `P3` | 定时任务、完整 Vault、多协议、公开插件市场、多云 Provider、自治多主机运维 | 当前偏离传统 AI 终端主线，或安全与维护成本过高 | 单独产品论证 | 必须另立 PRD、安全评审和维护成本评估 |

### 下方既有条目映射

| 优先级 | 既有 planning 条目 |
| --- | --- |
| `P0` | 版本化 Agent 控制面；状态 authority/explain；AI 供应商与 API Key 管理；CLI 输入预测；AI Assist 完善；CLI 消息通知开关进设置面板；子代理等待状态与主任务进度不同步 |
| `P0-Q` | Windows 原生输入与窗口兼容路线；发布、协议与版本文档一致性 |
| `P1` | Git worktree Agent Fork；MCP / Skill 与本地插件合同；自动化脚本执行；Markdown 选中/评论/发送到指定 tab |
| `P2` | SSH 常用命令保存器；SSH 错误标记到 tab + 提示条真关闭按钮；SSH 代理（HTTP CONNECT / SOCKS5）；SSH 会话粘贴本机剪贴板图片；强杀 Nebula 时连带清理子进程（Job Object）；背景色真调色板与自定义 HEX；图片与代码阅读器；Explorer 右键集成；Markdown 选择复制/目录；图片预览器；超大源码文件拆分；WebDAV 同步完善 |
| `P3` | 定时任务；Markdown 内嵌 HTML 解析；未来可能加入的完整 Vault、多协议、公开插件市场和自治多主机运维 |

## 新增待排期事项（2026-08-10）

### P0-2 CLI 补全统一排序与语义增强

- 当前已经有基于命令历史、常用目录、`PATH` 可执行文件和文件路径的 ghost 行内补全；
  后续工作是在现有能力上合并语义/AI 候选，不得重复实现第二套基础补全。
- 历史、目录、命令、路径和语义候选共用来源标识、排序合同与接受行为；`Tab` 无 ghost 候选时
  继续交给 Shell 原生补全，不能破坏 PowerShell、Nushell、Fish 等自身行为。
- 建议只进入可编辑输入区，用户确认后才发送到 Shell；不能自动执行。建议计算不能阻塞 PTY
  输出和输入，网络模型建议必须显式显示来源并可关闭。

### P2 代码编辑器

- 在文件树中打开源码时提供只读代码阅读器，先覆盖 UTF-8 文本、行号、搜索和基本语法高亮。
- 后续再加入编辑、保存、未保存状态和原子写入；大文件需要分块读取，不能一次性阻塞 UI。
- 编辑器必须复用现有标签页、焦点、滚动和错误提示，不另起一套窗口状态。

### P2 Markdown / TXT 编辑器

- Markdown 与纯文本文件使用同一套文本编辑器基础能力打开：UTF-8 编码、搜索、光标、选区和撤销/重做。
- Markdown 先保留源码编辑视图，同时提供可切换的预览；TXT 不附加格式化行为，避免把普通文本误当成 Markdown。
- 保存使用临时文件加原子替换，明确显示未保存状态；无法写入时保留编辑内容并给出可读错误。

### P2 SSH 连接页主机图标对齐

- 连接进度页左上角的主机图标改为复用侧栏和“设置 → SSH”使用的实际主机图标解析结果。
- 保持同一套墨迹宽度测量、光学缩放和垂直居中规则，避免连接页的服务器图标与实际主机类型不一致。
- 覆盖自动识别、用户手动指定图标、未知图标回落三种情况，并加入截图验收。

### P2 SSH 会话粘贴本机剪贴板图片

- 在 SSH 会话中触发粘贴时，若本机剪贴板只有图片数据，先显示文件名、格式、尺寸、大小和远端目标目录，用户确认后再上传；文本粘贴保持现有行为。
- 图片经当前 SSH 会话的 SFTP 通道上传到受控远端临时目录，采用随机文件名和原子落盘，禁止覆盖现有文件、路径穿越及跟随不可信符号链接。
- 上传成功后，根据当前前台 CLI 的能力注入远端文件路径或安全引用；无法确认 CLI 支持方式时只粘贴带 Shell 转义的远端路径，不自动执行命令。
- 明确处理剪贴板格式转换、同名冲突、大小上限、连接中断、部分文件回滚、远端权限不足和重连；失败时不得把本机路径误发给远端。
- 临时文件必须有可解释的生命周期：会话关闭时提示清理，异常退出后可在下次连接中识别并清理；用户选择保留的文件不得被自动删除。
- 验收至少覆盖 PNG/JPEG/WebP、包含空格和非 ASCII 字符的路径、只读远端目录、上传取消、断线恢复、多 Pane 并发和不支持 SFTP 的服务器。

### 执行约束

- 下方条目是详细需求来源，本节只决定产品顺序，不替代原始验收细节。
- 已经部分或全部落地、但仍留在本文件中的条目，实施前必须按当前代码、测试和 changelog 重新核实，避免重复开发。特别是 SSH 代理、图片预览、加密备份等近期代码路径。
- 已明确标记“已落地”的“数学公式渲染管线统一”不进入当前排期；完成真实性核实后应移入 changelog 并从 planning 删除。
- P0 新功能必须先定义可重复的可靠性、权限或状态机验收；没有证据时不得宣称“彻底解决”。
- P0 的 UI 不得自行维护工具、命令或任务的第二份事实；所有表面必须消费 T0 合同。
- SSH、SFTP 和端口转发是附带能力；若需求扩展到通用运维产品，应另立 PRD，不在原任务内顺带膨胀。

## [P2] 背景色真调色板与自定义 HEX

来源：2026-08-08 用户需求（设置 → 外观 → 背景色）。

- 背景色选择器改为真实调色板：预置色块直接展示最终颜色，不再用文字列表模拟颜色选项。
- 支持输入自定义十六进制颜色；至少接受 `#RRGGBB`，校验失败时保留原值并给出明确的内联错误。
- 预置色与自定义色共用同一份选中、预览、应用和持久化状态，避免弹层色块、输入框与实际终端背景不同步。
- 打开选择器时必须回显当前有效颜色；应用前规范化为统一格式，自定义色不能因不在预置调色板中而丢失。
- 优先级：`P2`。

## 图片与代码阅读器

- 右侧文件树双击 PNG、JPG、JPEG、WebP 图片时，在 Nebula 内创建新的图片预览 tab。
- 图片预览使用独立 `image_viewer`，与 Markdown 中的图片渲染共用同一套解码和适配逻辑。
- 首个版本只做只读图片预览：保持比例并适配到内容卡片，沿用现有 tab 的切换、关闭和重载行为。
- 后续增加缩放、平移、原始尺寸、图片信息、动画图片和更多格式支持。
- 后续增加代码阅读器，再扩展为可编辑代码：语法高亮、行号、搜索、保存、未保存状态和原子写入。

## Windows 原生输入与窗口兼容路线

来源：2026-08-08 对 ConPTY 9001 输入规格、Win32 `KEY_EVENT_RECORD` 和现有
`RawKeyEvent` 数据流的核验。目标是把原始事实保留在原生后端、把协议编码放在终端层，
确保 Windows 兼容性、跨平台扩展性和长期可维护性同时成立。

### 长期方案裁定(2026-08-09)

问题：Nebula 既要保持 Windows 原生输入保真，也要为多平台保留一致的事件契约；
winit 是否仍满足性能、兼容性和可维护性要求？

**裁定：保留 vendored winit fork，不换库、不自建 window 层。键位走三层结构，
每个平台只维护搬运原生事实的薄适配器：**

1. **采集层**(per-platform,薄):winit fork 扩展只做「原生事实搬运」——Windows 已有
   `RawKeyEventInfo`(VK/扫描码/repeat/extended/control_key_state/布局字符,一次捕获);
   Linux 未来搬 xkb keysym+utf8+evdev code,macOS 搬 NSEvent keyCode/characters。
   **fork 治理红线:只允许加只读字段/扩展 trait,禁止在 fork 里做策略(编码、过滤、映射)。**
2. **契约层**(跨平台,一处):`keyboard.rs` 只认 winit KeyEvent + 平台补充字段,做分发
   与快捷键判断,零平台分支堆积。
3. **编码层**（跨平台，一处）：`terminal_input.rs` 按终端能力
   （legacy/扩展键盘协议/win32-input）从契约模型编码，纯函数、可单测。VT 编码天然
   平台无关；win32-input 仅在
   Windows 有数据源。

三维度论证:

- **性能**:键盘不是热路径(人类输入 <100 事件/秒;热路径在 PTY 吞吐/渲染/reflow,与窗口
  库无关)。winit 的 Windows 事件循环就是标准 Win32 消息泵,无附加层。换库零收益。
- **兼容性**：兼容缺口全部在「信息保真」而非库本身。winit 上游确实丢原生事实（不暴露
  repeat_count/control_key_state/WM_CHAR 原始流,上游服务通用 GUI 永远不会收这些终端专用
  API），但 fork 扩展已经在 Shift+Enter（9001 记录）与 Esc
  （uChar=0→27，见 docs/hard_lessons.md）两条路径验证了 Win32 输入保真。替换窗口库、
  引入 C 窗口依赖或自建三套原生后端都会扩大平台维护面，并要求重写既有 UI 层。
- **可维护性**:多端键位的维护面 = 每平台 100–300 行事实搬运;编码器永远一份。fork 成本
  已被 vendor 策略锁定(保持 0.30.13 API 面、backport 上游),扩展是加法不改上游行为,
  rebase 冲突面小。Win32 输入兼容性的本质是「原始 KEY_EVENT 事实不丢」，而不是窗口代码——
  这一性质已通过采集层达成。

已知可接受偏差(记录在案,不视为缺口):key-up 无 WM_CHAR,修饰组合的 up 记录 uChar 回落
到无修饰基础值（原生记录与 down 使用相同值）；ConPTY 侧对 up 记录的 uChar 不敏感。

落地进度(2026-08-09):契约层第一步完成——`terminal_input.rs` 定义 `KeyInput`
键盘事实结构,采集只发生在 `From<&winit KeyEvent>` 一处,win32 编码器成为契约结构上的
纯函数。修饰组合(Shift+Enter、Ctrl+Enter、Ctrl+Space、Ctrl+Backspace、AltGr、裸修饰键、
UTF-16 代理对、Esc 双向)首次获得字节级单测;测试值全部取自真实捕获的事实表,禁止猜测。
配合 `scripts/win32_input_matrix.ps1`(PostMessage 无打扰回放,断言 node 通道基线)形成
「纯函数单测 + 端到端回放」双层防线。后续：扩展/legacy VT 编码器迁到同一契约；焦点报告
已按 9001 契约在对应会话无条件发送（宿主自消费，仅向 1004 订阅者透传，双向验证零泄漏）。

落地进度(2026-08-09 第二步):kitty/legacy VT 编码器整体迁入 `terminal_input.rs`,
`keyboard.rs` 中约 470 行旧编码器(`build_sequence`/`SequenceBuilder` 一族)删除,编码层
对 winit 类型的最后依赖消除——三平台自此共用同一份纯函数编码器。`KeyInput` 契约补上
`key_without_modifiers`(kitty 备用键上报需要无修饰基键)。新增 11 个跨平台 vt 单测
(Esc 消歧 CSI 27u、Shift+Enter 修饰位、Ctrl+A 码点、legacy 方向/功能键、事件类型标记
释放、左 Shift 专属 keysym、关联文本码点、numpad 门控、win32/kitty 优先级),在任意 OS
上都可编译运行——「编码层跨平台一处」的可见形态。迁移后矩阵回放 PASS,字节与基线逐位一致。

落地进度(2026-08-09 第三步):可靠性三件套落地。(1) 失焦合成 key-up:
`terminal_input::build_focus_loss_key_ups` 纯函数,失焦瞬间对按住的修饰键按当前协议
(win32 记录 / kitty ALL_KEYS+EVENT_TYPES)合成 release,门控与真实 `key_release` 一致,
挂钩在 `touch.rs on_focus_change`(先合成 up 再发 CSI O);左侧变体近似为已知偏差。
(2) 输入延迟打点:`input/latency.rs`,`NEBULA_INPUT_LATENCY=1` 启用,分段
key→pty / wake→frame / key→frame 走 `nebula_debug_log`;全局单槽、无回显字节归因为
已知偏差。(3) ConPTY 生命周期:`drain_recv_channel` 每轮排空只应用最新 resize
(keep-last,零定时器不吞终值);`CreatePseudoConsole` 带 0x4 失败去 flag 重试
(端到端自门控降级,assert panic 消灭);传输死亡(读/写/poll 错误)统一发
`Event::PtyFailure(reason)` + `terminal.exit()` 收尾,app 侧消息栏+日志,僵尸 tab 消灭。
未竟:降级分支(in-box 拒 0x4)在 Win11 开发机无法自然触发,靠代码审查覆盖;
resize 合并与 create 重试无独立单测(需 mock EventedPty/ConptyApi,记为后续)。
矩阵回放已知偏差:被测 exe 首次启动若被实时扫描/磁盘抖动卡住,会丢头部键(窗口就绪
慢于 ready+1s 预算)或整树在脚本 7s 收尾时被杀(尾部键全失,探针连 90s timeout 行都
来不及写);同一 exe 重跑即恢复逐位一致。判 FAIL 前先重跑一次,脚本加自动重试记为后续。

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
- [ ] **Windows 原生窗口服务层**：保持 Winit 的窗口抽象，在 Windows 后端补齐 Win32
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

### ConPTY 9001 参考落点

- `terminal/doc/specs/#4999 - Improved keyboard handling in Conpty.md`：9001 请求、
  `KEY_EVENT_RECORD` 字段和完整示例。
- `terminal/src/cascadia/TerminalControl/TermControl.cpp`：`OriginalKey()`、扫描码、
  extended 位、CharacterReceived 与 RawWriteChar 分工。
- `terminal/src/cascadia/TerminalCore/Terminal.cpp`：字符键交给操作系统文本事件，功能键和
  修饰键走原生 key event，并在字符事件到达时恢复 VKEY/扫描码关联。
- `terminal/src/terminal/input/terminalInput.cpp`：只格式化已经存在的
  `KEY_EVENT_RECORD` 字段，不从 Unicode 文本反推 VKEY。
- `RawKeyEvent` 和 `encode_win32_input_mode()` 作为跨平台事件模型与回放测试的边界，
  不向通用层泄漏具体窗口后端类型。

## Explorer「在此处打开终端」右键集成（安装版）

来源：2026-07-28 用户需求（含右键菜单截图）。

- 目录背景与目录节点右键菜单加「在 Nebula 中打开终端」，带 Nebula 图标。
- 实现路径：安装器写注册表 `HKCU\Software\Classes\Directory\shell\Nebula`（含
  `Directory\Background\shell`），`Icon` 指向安装目录的 exe，命令
  `nebula.exe --working-directory "%V"`。
- 卸载时自动清理这些注册表键，不留孤儿菜单项。
- 覆盖安装：安装器需检测已有安装（同 HKCU Uninstall 条目），存在则原地更新
  （保留用户数据目录），而不是每次都装成并存的全新副本。

## MCP / Skill 内置化

来源：2026-07-28 用户需求（provider/SSH key/skill/MCP 内置化）。

- 在 Nebula 内管理 MCP 服务器与 skill（安装/启停/配置），形成统一的扩展管理入口。

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

## SSH 密钥口令输入进应用内模态（替换 CredUI）

来源：2026-08-09 用户反馈「不应该弹出系统弹窗」（截图：CredUI 密钥口令框）。
当日已修根因：russh 丢了 `rsa` feature 导致所有 RSA .pem 解析失败，被误判成
「密钥有口令」而弹 CredUI；现在解析失败直接报错，只有真正加密的密钥才进
口令流程。剩余工程：

- 真正受口令保护的密钥，口令输入仍走 `ssh_credentials.rs`
  `CredUIPromptForCredentialsW`（用户名+密码双框的系统对话框，语义错位——
  只需要一个口令框）。密码认证的 `PromptPassword` 与 keyboard-interactive
  的 MFA 输入同样走 CredUI，一并替换。
- 方向：复用 `NebulaConfirm::BackupPassphrase` 的应用内单行掩码输入模式。
  难点是请求-响应通道：SSH 认证在 tokio 后台任务里，UI 在窗口线程——需要
  `EventType::SshSecretRequest`（EventProxy 带 pane id）+ oneshot 回传，
  连接卡片期间模态置于卡片之上。
- 「记住口令」勾选沿用现有凭据管理器存储
  （`Nebula/SSH/KeyPassphrase/<sha512>` 命名空间不变）。

## Markdown 选中 → 评论 → 发送到指定 tab

来源：2026-07-29 用户提供的交互截图（composer.md L3 弹窗）。形态：

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

来源：2026-08-04 用户需求：为境外主机配置 HTTP 或 SOCKS5 代理。
工程上存在三种常见粒度：应用全局代理、每主机覆盖，以及直接服从
`~/.ssh/config` 的 `ProxyCommand`。Nebula 的设置页负责前两者，OpenSSH 配置继续作为
高级用户的兼容入口。凭据不得写入普通 settings；环境变量优先级与 HTTP CONNECT / SOCKS5
握手必须由连接层统一处理。

### 建议给 Nebula 的形态：全局默认 + 每主机覆盖

只做全局配置无法处理特殊主机，只做每主机配置又会制造大量重复输入。因此采用全局三态
（关闭 / 跟随系统 / 自定义，含 `no_proxy` 绕过列表）并叠加每主机覆盖
（跟随全局 / 强制直连 / 自定义）。

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

### 实现裁定

仅关闭 ConPTY 不能保证宿主进程被强制终止时整棵子进程树退出。Windows Job Object 可以用
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 建立明确的生命周期边界，但把已经属于其他 job 的进程
重新分配进去可能失败，因此不能用一个进程级大 job 粗暴接管所有 shell。

Nebula 直接使用现有的 `windows-sys`：`CreateJobObjectW` 创建对象，
`SetInformationJobObject` 写入 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，
`AssignProcessToJobObject` 绑定 child，并由 RAII 在 `Drop` 中 `CloseHandle`。主动终止走
`TerminateJobObject`。这条路线无需新增 crate，且便于为每个 pane 单独管理生命周期。

### 落点

ConPTY 的 `CreateProcessW` 在 `nebula_terminal/src/tty/windows/conpty.rs:274`。
采用「每个 pane 一个 job」，避免进程级大 job 与已有 shell job 发生归属冲突。

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

## 字体集合统一：系统族与导入族合并为一个 DirectWrite 集合

来源：2026-08-11 用户提问「系统族和导入的私有族难道不能统一吗」。做字体选择器
行内预览（WYSIWYG）时暴露的架构裂缝。

### 现状：两个互不相通的集合

`renderer/text/font_rasterizer.rs` 的 `Rasterizer` 同时持有两套字体来源：

- `system: crossfont::Rasterizer` — crossfont 0.8.1 的 `DirectWriteRasterizer::new`
  里硬初始化 `FontCollection::system()`，**没有暴露注入点**，改不了它查哪个集合。
- `private_collection: FontCollection` — 内置 Maple 加用户导入的字体文件，
  经 `CustomFontCollectionLoaderImpl` 组装。

两者从不合并，因为 DirectWrite 的 `IDWriteFontCollection` 是**不可变对象**：没有
`AddFontFile` 之类的接口，不能往系统集合里追加。

于是每个「按族名拿字体」的操作都得写两遍查找：

- `load_preferred_font`：先 `system.load_font`，失败落 `load_embedded_font`
- `preview_font_key`（`glyph_cache.rs`，本次 WYSIWYG 新增）：同样的双路径
- `family_loads` / `font_family_available`（`glyph_cache.rs`）：**只查系统集合**
  —— 已知不一致，导入字体是走 `add_private_font` 那条独立分支验证的

### 目标形态

把系统字体文件与私有文件一起灌进一个 `CustomFontCollectionLoaderImpl`，得到单一
合并集合，所有按族名的查找走一条路径。

```
合并 FontCollection
├─ 系统字体文件（枚举出 FilePath）
└─ 私有文件
   ├─ 内置 Maple（include_bytes!）
   └─ 用户导入的 .ttf / .otf
```

### 实现路线

1. 枚举系统字体的**文件路径**。dwrote 0.11.5 没有包 `IDWriteFontSet`（`lib.rs`
   导出里只有 `FontCollection` / `FontFile` / `FontFallback`），所以要么自己写
   winapi 绑定拿 `GetSystemFontSet`，要么退用 `FontCollection::system()` 逐族
   `create_font_face().get_files()`。后者不需要新绑定，但在装了几百个字体的机器上
   是实打实的开销，必须懒加载 + 缓存（`system_font_families` 上已有同类告诫）。
2. 用合并集合替换 `private_collection`，`load_embedded_font` 成为唯一族查找入口。
3. 上面三处双路径塌缩成单路径。
4. crossfont 的 `system` 字段仍要保留：它承载 OS 级 fallback（`MapCharacters`）
   与 metrics，那部分不受集合合并影响。

### 收益与风险

**收益**：查找路径唯一；`family_loads` 的导入字体盲区自动消失；将来做可配置
fallback 链（issue #33）时有一个统一的族命名空间可映射。

**风险**：合并集合要持有全部系统字体文件的 `FontFile` 句柄，内存与首次构建耗时
都会涨。必须实测装了 500+ 字体的机器上的冷启动数字，超过 ~50ms 就改后台线程
预热 + 首帧回退双路径。

### 关联

issue #33 的三个洞里，「导入字体拿不到 OS fallback」（`rasterize_once` 走
embedded 分支就绕开了 crossfont 的系统路径）在集合统一后**仍然存在**——那是
fallback 链的问题，不是集合的问题，两件事分开做。

## PowerShell 集成脚本末尾的 Clear-Host 会吞掉用户 $PROFILE 的输出

来源：2026-08-11 修复「新增 $PROFILE 函数不生效」时发现的连带问题。已确认存在，
但用户裁定先不动，等真有人反馈再定位。

### 背景

`nebula_terminal/src/tty/windows/mod.rs:956` 的默认 shell 曾硬编码 `-NoProfile`，
导致用户 `$PROFILE` 里新增的函数在新建 tab 后不存在，而 Windows Terminal 正常。
该参数已移除，并在 `mod.rs:1078` 加了回归测试
`default_powershell_loads_the_user_profile_and_ends_with_the_integration` 守住。

### 遗留问题

`$PROFILE` 恢复加载之后，注入脚本 `NEBULA_PROMPT_PS1` 末尾的 `Clear-Host`
（`mod.rs:693`）会在 profile 执行完之后清屏。因此用户 profile 里的欢迎语、
fastfetch、oh-my-posh 横幅等**任何启动输出都会被抹掉**。

WT 不清屏——它的 `defaults.json:41` 只写裸 `powershell.exe`，一个参数都不加，
shell integration 靠用户自己往 `$PROFILE` 里写 OSC 133。我们选的是自动注入路线，
那么注入就必须是「追加」而不是「替换」用户环境；`Clear-Host` 违背了这条。

现象会是：用户报告「函数生效了，但我的 profile 输出没了」——这是第二个 issue，
不是第一个的复发。

### 待定的处理方向

`Clear-Host` 当初的用途是盖掉 PowerShell 版本横幅，但默认参数里已经有 `-NoLogo`，
两者职责重叠。可能的做法（未裁定）：

- 直接删掉 `Clear-Host`，靠 `-NoLogo` 抑制横幅。风险是若 profile 本身有噪声输出，
  首屏不再干净——但那本来就是用户自己的选择。
- 只在检测到用户无 `$PROFILE` 时清屏。引入分支，且要判断三个 profile 路径
  （AllUsers/CurrentUser × AllHosts/CurrentHost），复杂度不低。
- 保留现状，在设置里给一个「启动时清屏」开关。

倾向第一个：删掉最简单，且与 WT 语义一致。但要先确认 `-NoLogo` 在
Windows PowerShell 5.1 和 pwsh 7 上都真的抑制了横幅（5.1 的 `-NoLogo` 历史上
有过不生效的版本）。

### 复现方式

在 `$PROFILE` 里写一行 `Write-Host "hello from profile"`，新建 tab。函数可用，
但这行输出看不到。

## 备忘

- （空）
