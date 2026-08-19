# Changelog / 更新日志

Every release entry is provided in English and Simplified Chinese.

每个版本条目均同时提供英文和简体中文说明。

## 1.1.0 - 2026-08-18

### English

#### Added
- **A rebuilt interface as the default window** — 1.1.0 packages open a new GPU-accelerated shell: the terminal grid, sidebar, settings, launcher and document views are all rendered by it. `--legacy-shell` still starts the previous winit UI, which stays in the codebase.
- **AI session forking from the sidebar** — right-clicking a claude or codex tab offers "Fork AI session", opening a new tab that resumes the same conversation through the CLI's own fork syntax. Eligibility is recomputed when the menu opens, so a session that has just reported its id is immediately forkable.
- **AI turn-state detection** — sidebar tabs show running, finished, waiting-for-input and failed states for claude and codex, driven by the CLIs' own hook payloads plus screen-state evidence. A watchdog releases zombie "working" states so the spinner cannot spin forever.
- **A full settings page in the new shell** — every key-value setting from the legacy UI is migrated with the same section navigation, and Settings opens as one reusable tab instead of a floating window.
- **Tab rename, color labels and single-tab export** — rename a tab in place, tag it with one of seven brand colors, or export just that tab as a workspace file.
- **Split panes, drag-to-split and workspace session restore** — dragging a tab onto a pane edge splits it, and the split tree with per-pane launch identity comes back on the next launch.
- **AI provider settings and a runtime control CLI** — provider credentials and models are configurable in Settings, and a runtime API lets scripts open tabs, run commands and query state.
- **A real GPUI agent-control loop** — the default shell now publishes its live workspace into the same RuntimeHub used by CLI waits and subscriptions. `nebula ctl agents` returns canonical agent identity, lifecycle state and evidence source; `nebula ctl read` reads the real terminal Grid/scrollback tail without moving the user's viewport; focus, prompt and wait act on the exact window/pane pair. SSH panes reject reads and prompts until their shell is ready, and unavailable GPUI multi-window creation now returns an explicit error instead of a synthetic success.
- **Isolated Git-worktree agents** — `nebula ctl agent-fork` creates a new branch and worktree on a Runtime worker thread, opens a real Tab/PTY in that checkout, and starts a named Codex or Claude generation. The stable Agent record keeps its branch, base commit and paths for later `agent-get`/prompt/read/wait calls. Dirty sources, existing branches/paths and SSH sources fail explicitly; a confirmed UI launch failure rolls back only resources created by that request, while an uncertain UI timeout keeps the checkout instead of invalidating a late-created pane.
- **A packaged Nebula Runtime skill and protocol contract** — release ZIPs and installers include the versioned Runtime API documentation/schema plus a `nebula-runtime` Skill that drives the list → observe → read → focus → prompt → wait workflow and treats terminal output as untrusted data.
- **Built-in box-drawing geometry** — frames, blocks and separators are drawn geometrically in the render contract instead of relying on font glyphs, so they align in any font.
- **Terminal mouse semantics parity** — application mouse reporting, right-click behavior and copy-on-select match the legacy shell.
- **A Markdown and image reader** — documents render with rich text, native math and images, and PNG/JPEG/WebP/BMP open in their own viewer tab.

- **Two completion styles with a switch** — Settings → Terminal → Completion adds a "Completion style" choice between the existing inline ghost text and a new popup candidate list. The popup gathers history, frecency directories, PATH commands, and filesystem matches (up to 8, with source tags), navigates with Up/Down, accepts with the configured accept key (Tab/Right), and dismisses with Esc without re-opening until the line changes. A command-palette action toggles the style, and the choice persists as `completion_style`.
- **A real color picker for the custom background** — the background-color popup now leads with a saturation/value plane and a hue bar; dragging picks continuously with live terminal preview and persists on release. The preset swatches and the hex field remain, and all three inputs stay in sync (including hue retention across gray/black/white picks).
- **Clipboard screenshot paste, locally and over SSH** — pasting with an image-only clipboard (e.g. right after Win+Shift+S) now converts the bitmap to PNG and pastes a file path instead of nothing. Local panes write a temp file; SSH panes upload via the existing SFTP stack to `/tmp/nebula-paste-<ts>.png` in the background and then type the remote path into the pane — so image-accepting CLIs like codex and claude receive a usable path on both sides.
- **Configurable new-tab placement** — Settings → Interaction can place newly created tabs immediately after the active tab or at the end of the tab list. The choice persists, while restored tabs keep their original ordering semantics.
- **Configurable terminal cell width** — Settings → Appearance offers Compact and Relaxed cell-width modes. Changing the mode immediately rebuilds font metrics, the terminal grid, pane layout, and PTY dimensions, and the choice persists across launches.
- **Ordered font fallback chains** — `font_family` accepts comma-separated families such as `JetBrains Mono, LXGW WenKai, Cascadia Code`. The first family remains the primary face, later families fill missing glyphs in order for regular, bold, italic, and bold-italic text, and the embedded Maple font remains the final fallback. System and imported/private fonts now use the same family lookup path for validation, preview, and rendering. (#33)
- **Middle-click to close tabs** — middle-clicking a tab or its close control follows the existing close flow, including confirmation when required.
- **Audible terminal bell** — BEL (`\a`) now plays the system notification sound in addition to the visual bell, so an AI CLI finishing a turn is audible even from another tab. Throttled so a bell-happy program cannot machine-gun it; disable with `bell.audible = false`. (#37)
- **AI conversations resume across restarts** — panes that had a claude/codex conversation open when Nebula closed (or crashed) now type the exact resume command (`claude --resume <id>` / `codex resume <id>`) into the restored shell automatically. Session identity comes from the CLIs' own hook payloads; a claude detected without an id falls back to `claude --continue` in the restored directory. Injection only targets plain restored shells (never SSH/profile seeds), ids are validated before anything reaches the terminal, and Settings → Advanced offers the "resume AI conversations on restore" switch (`resume_ai`, on by default).
- **System tray icon with agent attention** — a resident tray icon flips to an amber-dot state whenever any AI CLI stops and waits for input, even with every window buried or minimized. Right-click lists all agent panes with their state (running / waiting) and jumps straight to the source pane through the same focus path as toast clicks; left-click goes to the most urgent pane. The tray mirrors the sidebar badges (one source of truth) and can be turned off in Settings → Advanced (`tray`, on by default).
- **Line numbers for plain-text files (GPUI shell)** — double-clicking `.txt` / `.log` / `.json` / `.jsonl` files in the file tree now opens the code viewer with line numbers and per-line virtualization (same as source files) instead of the markdown document view; Markdown keeps the rich reader.
- **Clickable file and URL links in the GPUI shell** — OSC 8 hyperlinks and matched URLs draw a dashed underline in the cell's own foreground color. Hovering shows a preview (`decoded path · Ctrl+click`); Ctrl+click opens local `file://` paths in Explorer and other URIs with the default handler.
- **Configurable terminal bell in the GPUI shell** — Settings → Profiles → Terminal bell chooses Off, Flash, Sound, or Flash + sound. A BEL still plays the throttled system beep, briefly flashes the pane, toasts when the window is unfocused, and dots background tabs. The choice persists as `bell` (`none` / `visual` / `audible` / `both`, default both).
- **WYSIWYG font picker and folder-picker startup directory (GPUI shell)** — the font dropdown renders each family in its own face, strips junk extensions from display names, and the import button is just "Import font". Startup directory is a folder picker with "inherit current" / Clear, matching the legacy shell.

#### Fixed

- **Explorer context-menu launches join the resident instance** — "Open in Nebula" used to be treated as explicit intent and always started an independent window, so tabs detached in the resident process looked lost. A directory launch without `-e` now attaches the existing instance first (restoring its tabs) and opens the directory as a new tab in that window, brought to the foreground; without a resident instance it still starts standalone. The runtime API's `tab.new` gained an optional `cwd` parameter to carry this.
- **Imported terminals become selectable in the default-shell dropdown** — the dropdown's hit test counted only detected shells while the list also renders imported quick-launch profiles, leaving the trailing rows visible but unclickable. Both sides now share one count.
- **Git panel no longer blanks on repositories owned by another user** — `git status`/`diff` in the drawer run with a per-invocation `safe.directory` exemption, fixing the silently empty Git view on `\\wsl$\…` roots and elevated-owner checkouts where the same commands work in the user's own shell. Failures now leave a debug-log trace instead of a silent blank.
- **Visible Windows windows recover from stuck render gates** — startup-time occlusion misreports and missing frame callbacks can no longer leave an already visible window accepting input without repainting. Occlusion is ignored unless the window is actually minimized, the existing 1 Hz window heartbeat idempotently releases stale `occluded` and `has_frame` gates, and the watchdog is now armed at window creation so even a freeze before the first frame self-heals within a second. (#21, #32)
- **Drive-root context menu launches work** — right-clicking the background of `D:\` (any drive root) and choosing "Open in Nebula" failed with "invalid working directory": Explorer expands `%V` to `D:\`, whose trailing backslash escapes the closing quote on the command line. The mangled path is now repaired, and the error's log hint uses PowerShell syntax (`$env:NEBULA_LOG`) so copy-paste resolves. (#36)
- **Multi-line paste stops interrupting codex** — the multi-line paste confirmation now only fires when newlines are actually headed to a shell that would execute them line by line. Applications in bracketed-paste mode (codex, vim, modern PSReadLine) receive the paste as one atomic chunk, so the warning no longer blocks them. The dark-theme "Enter" keycap on the confirm dialog's accent button also derives from the button's own ink instead of the near-black panel color. (#35)
- **Math overlays survive markdown-unescaped delimiters (GPUI shell)** — some AI CLIs render their markdown before printing and eat the backslashes of `\[ \]` / `\( \)` (markdown punctuation escapes), leaving bare `[ … ]` blocks and `(\sqrt{…})` spans that never overlaid. The shared scanner now recognizes those bare forms when the content carries a known TeX command (`\int`, `\frac`, …) and the brackets sit alone on their line edges; JSON arrays, `[INFO]` logs and regex `(\d+)` stay literal. Explicit TeX lengths (`\\[6pt]`, `\kern`) also regained their point-to-pixel scale in the GPUI shell, matching the legacy renderer.
- **Markdown reader fills the reading column (GPUI shell)** — soft line breaks inside a paragraph (hand-wrapped README prose) rendered as hard line breaks, leaving the right side of the column empty and ragged. They now merge into spaces per CommonMark, with CJK neighbours joining directly.
- **Markdown images actually load (GPUI shell)** — the document tab now resolves relative image paths against the document's own directory, loads absolute local paths from disk, and fetches `http(s)` images through a ureq HTTP client on the background executor (gpui ships a null client by default), so README logos, shields badges (SVG included) and screenshots finally display. Animated GIF/WebP (local or remote) are flattened to a still PNG first, because gpui panics when a multi-frame image's frame index is reused on a static image.
- **Raw HTML in Markdown renders like GitHub (GPUI shell)** — README-style HTML now matches GitHub's semantics: `align="center"` on `<p>` / `<div>` / headings centers the block, consecutive `<img>` badges inside one paragraph flow on a single centered row instead of stacking one per line, inline `<br/>` breaks in document order (it used to be hoisted above the text it followed), and `<a><img/></a>` linked badges are no longer dropped — the image renders and carries its link.
- **Codex `notify` no longer wraps itself into an unusable config** — when another notify wrapper (codex-computer-use) re-registered without recognizing Nebula's helper, it serialized the whole existing array into its own `--previous-notify` argument; Nebula wrapped that again, and the escaped backslashes doubled every round until `config.toml` reached 130 MB and Codex refused to start. The helper mark now counts as "already wired" wherever it appears — including inside a JSON string — so Nebula only heals its own path and never re-wraps. A serialized `notify` over 8 KB is refused outright as a backstop against any other form of inflation, and `nebula setup-ai --remove` persists `ai_hooks=0` so auto-wiring stays off across launches. (#38)
- **Tab context-menu shadow no longer thickens with the tab count (GPUI shell)** — the component library's context-menu extension keys its state by a fixed element id, so every tab row resolved to the same state and re-rendered the same open menu at the same anchor. The menu panel is opaque, but its shadow is not: with eight tabs the drop shadow was composited eight times. The tab menu now draws exactly once from the workspace root, like the file-tree menu.
- **The terminal cursor no longer drifts after a resize** — conhost collapses the screen buffer non-deterministically on resize and repaints nothing, so the cursor could end up rows away from where the shell thinks it is. Nebula now reconciles the real position through an AttachConsole probe after each resize and re-aligns the grid.
- **The user PowerShell profile loads in the default shell** — the default launch skipped `$PROFILE`, so prompts, aliases and PSReadLine settings defined there never applied. (#30)
- **Reflow survives a shrink-then-grow round trip** — narrowing and then widening the window no longer drops wrapped content, and the window now has a real minimum size.
- **Cursor focus and blink, and a startup render-gate freeze** — the cursor state machine follows focus correctly, and a watchdog releases a startup gate that could leave the window visible but unpainted. (#21)
- **Esc reaches Claude Code in the new shell** — the win32-input encoder filled the control-key `uChar` with 0, which OpenConsole drops, so Esc never arrived at CLIs reading the byte stream.
- **The window-close confirmation accounts for busy processes** — closing while a build or an AI CLI is running now names the program instead of closing silently.
- **Notifications anchor to the bottom-right corner.**
- **The acrylic background composites at the configured opacity.**
- **Shell picker brand icons are no longer blurry** — they are pre-scaled to the integer device pixel size instead of being stretched by the GPU.
- **Built-in glyphs fill the cell in Relaxed width mode.**
- **Tab launch identity and full-row hit areas** — a restored tab keeps the program it was launched with, and the whole row is clickable rather than just the label text.

#### Improved

- **File/Git side-panel refreshes no longer block rendering** — directory traversal and Git status subprocesses now build a snapshot on a worker thread while the existing view stays usable. Completed snapshots are swapped in atomically; stale-root results are discarded and active search results are not overwritten.
- **Tab disclosure and file-tree menus in the GPUI shell** — the TABS collapse control uses the shell's linear chevrons instead of Nerd Font glyphs. File-tree context menus no longer pick up the drawer drop shadow, so they match the tab menus.
- **The font picker previews candidates in their own glyphs** — each family is drawn in its own face before you commit to it.

### 简体中文

#### 新增
- **重写的界面成为默认窗口** — 1.1.0 安装包启动全新的 GPU 加速界面：终端网格、侧栏、设置、启动器与文档视图全部由它渲染。旧的 winit 界面仍保留在代码中，用 `--legacy-shell` 启动。
- **侧栏 AI 会话分叉** — 右键 claude 或 codex 标签会出现「分叉 AI 会话」，用 CLI 自己的 fork 语法开一个新标签接续同一段对话。资格在菜单打开当下重算，刚上报 session id 的会话立刻就能分叉。
- **AI 回合状态识别** — 侧栏标签会显示 claude 与 codex 的运行中、已完成、等待输入与失败四种状态，依据来自 CLI 自身的 hook 载荷加屏幕状态证据。看门狗会释放僵尸「运行中」状态，转圈不会一直转下去。
- **新界面的完整设置页** — 旧界面的所有设置项与分区导航全部迁移，设置以一个可复用的标签打开，不再是浮动窗口。
- **标签重命名、颜色标记与单标签导出** — 可以就地重命名标签、用七种品牌色之一标记，或只把这一个标签导出为工作区文件。
- **分屏、拖拽分屏与工作区会话恢复** — 把标签拖到窗格边缘即分屏，分屏树与每个窗格的启动身份会在下次启动时恢复。
- **AI 服务商设置与运行时控制 CLI** — 服务商凭据与模型可在设置中配置，运行时 API 让脚本可以开标签、执行命令与查询状态。
- **默认 GPUI 壳的真实 Agent 控制闭环** — 默认界面把正在运行的 workspace 发布到 CLI wait/subscribe 共用的 RuntimeHub。`nebula ctl agents` 返回权威的 Agent 身份、生命周期状态与证据来源；`nebula ctl read` 从真实终端 Grid/scrollback 尾部读取且不移动用户视口；focus、prompt、wait 始终操作精确的 window/pane 组合。SSH pane 在 Shell Ready 前拒绝读写；GPUI 尚不可用的多窗口创建明确报错，不再伪造成功。
- **Git worktree 隔离 Agent** — `nebula ctl agent-fork` 在 Runtime 工作线程创建新分支与 worktree，在该 checkout 中打开真实 Tab/PTY，并启动带稳定 generation 的命名 Codex/Claude。Agent 记录持续携带 branch、base commit 与路径，后续 `agent-get`/prompt/read/wait 都能核验同一工位。dirty source、既存分支/路径与 SSH source 明确报错；UI 明确启动失败只回滚本请求创建的资源，UI 超时状态不确定时保留 checkout，不让晚到 Pane 的 cwd 突然失效。
- **随包 Nebula Runtime Skill 与协议合同** — ZIP 和安装器同时携带版本化 Runtime API 文档、Schema 与 `nebula-runtime` Skill，固化“列出 → 观察 → 读取 → 聚焦 → 派活 → 等待”流程，并把终端输出按不可信数据处理。
- **内置制表符几何绘制** — 边框、方块与分隔线在渲染契约中按几何绘制，不再依赖字体字形，换任何字体都能对齐。
- **终端鼠标语义对齐** — 应用级鼠标上报、右键行为与选中即复制均与旧壳一致。
- **Markdown 与图片阅读器** — 文档带富文本、原生公式与图片渲染，PNG/JPEG/WebP/BMP 在独立的查看标签中打开。

- **两种补全样式可切换** — “设置 → 终端 → 补全”新增「补全样式」：在既有的行内灰字与新的弹窗候选列表之间选择。弹窗汇总历史命令、常用目录、PATH 命令与文件系统匹配（至多 8 项，带来源标签），↑/↓ 选行、按补全接受键（Tab/→）接受、Esc 关闭且同一行不再重弹；命令面板提供切换动作，选择以 `completion_style` 持久化。
- **自定义背景色改用真调色盘** — 背景色浮层顶部新增饱和度/明度取色面与色相条，按住拖动即连续取色、终端实时预览、松手落盘。预设色板与 16 进制输入保留，三种输入互相同步（灰/黑/白取色时保留既有色相）。
- **剪贴板截图粘贴（本地与 SSH）** — 剪贴板只有位图没有文本时（如 Win+Shift+S 之后），粘贴会把位图转成 PNG 并粘出文件路径：本地 pane 写入临时文件；SSH pane 经既有 SFTP 栈后台上传到远端 `/tmp/nebula-paste-<时间戳>.png` 再把远端路径敲进会话——codex/claude 这类接受图片路径的 CLI 在两侧都能直接用。
- **可配置新标签页插入位置** — “设置 → 交互”可选择把新建标签页插在当前标签页之后，或放到标签列表末尾。选择会持久化；恢复会话中的标签页仍保持原有顺序语义。
- **可配置终端单元格宽度** — “设置 → 外观”新增“紧凑”和“宽松”两种单元格宽度模式。切换后会立即重算字体度量、终端网格、分屏布局和 PTY 尺寸，并在下次启动时保留选择。
- **有序字体 fallback 链** — `font_family` 支持逗号分隔的字体族，例如 `JetBrains Mono, 霞鹜文楷, Cascadia Code`。第一个字体族仍是主字体，后续字体族按顺序为常规、粗体、斜体和粗斜体补齐缺失字形，内置 Maple 字体始终作为最后兜底。系统字体与导入/私有字体现在统一通过同一族名查找路径完成校验、预览和渲染。（#33）
- **中键关闭标签页** — 在标签页或其关闭控件上单击鼠标中键，会进入既有的关闭流程；需要确认时仍会正常弹出确认。
- **终端铃声** — BEL（`\a`）现在在视觉铃声之外播放系统提示音：AI CLI 在别的标签页里完成回合也听得见。内置节流，刷铃声的程序不会连成机关枪；`bell.audible = false` 可关闭。（#37）
- **AI 对话跨重启接续** — 关闭（或崩溃）时某个 pane 里还开着 claude/codex 对话的，冷恢复后会自动把 resume 命令（`claude --resume <id>` / `codex resume <id>`）敲进恢复出来的 shell。会话身份来自 CLI 自己的 hook 载荷；识别到 claude 但没有 id 时退化为在恢复目录里 `claude --continue`。注入只针对恢复出的裸 shell（绝不注入 SSH/Profile 首格），id 上屏前先做字符集校验；“设置 → 高级”提供「恢复时自动接续 AI 对话」开关（`resume_ai`，默认开）。
- **托盘图标 agent 提醒** — 常驻系统托盘图标：任一 AI CLI 停下来等输入时翻转为橙点 attention 态，窗口全被压住或最小化也看得见。右键列出所有 agent pane 及状态（运行中/等待输入），点击经 toast 同一条聚焦路径直达来源 pane；左键直达最需要人的那个。托盘与侧栏徽章同一事实源；“设置 → 高级”可关（`tray`，默认开）。
- **纯文本文件带行号打开（GPUI 壳）** — 文件树双击 `.txt` / `.log` / `.json` / `.jsonl` 现在进代码查看器：行号 + 按可视行虚拟化（与源码文件同款），不再进 markdown 文档视图；Markdown 仍走富文本阅读器。
- **GPUI 壳可点击文件与 URL** — OSC 8 超链接和匹配到的 URL 用格子自身前景色画虚线下划线。悬停显示预览（`解码后的路径 · Ctrl+点击`）；Ctrl+点击用资源管理器打开本地 `file://`，其余 URI 走默认打开方式。
- **GPUI 壳可配置终端铃声** — “设置 → 配置文件 → 终端铃声”可选关 / 闪烁 / 声音 / 闪烁 + 声音。BEL 仍播放节流后的系统提示音、短暂闪一下 pane、窗口失焦时出 toast、后台 tab 打点。选择以 `bell` 持久化（`none` / `visual` / `audible` / `both`，默认两者都开）。
- **GPUI 壳所见即所得字体选择器与启动目录** — 字体下拉用各族自己的字形渲染，展示名剥掉多余扩展名，导入按钮改为「导入字体」。启动目录改为选文件夹，「继承当前目录」/「清除」，与旧壳一致。

#### 修复

- **资源管理器右键打开并入驻留实例** — 「在 Nebula 中打开」此前被当作显式意图而总是启动独立窗口，驻留进程里 detached 的标签看起来就“丢了”。带目录、无 `-e` 命令的启动现在会先 ATTACH 既有实例（找回原有标签），再在该窗口新开定目录标签并置前；没有驻留实例时仍独立启动。runtime API 的 `tab.new` 为此新增可选 `cwd` 参数。
- **导入的终端在默认 Shell 下拉中可以选中** — 下拉的命中测试此前只统计检测到的 shell，而列表还渲染了导入的快速启动配置，末尾几行看得见点不中。绘制与命中现在共用同一计数。
- **Git 面板在他人所有的仓库上不再空白** — 抽屉里的 `git status`/`diff` 以单次调用范围的 `safe.directory` 豁免运行，修复 `\\wsl$\…` 根目录与提升权限检出下“自己 shell 里 git status 正常、面板却空白”的问题；失败现在会留下调试日志而不是无声空白。
- **Windows 可见窗口可从卡死的渲染门控中恢复** — 启动期的遮挡误报或帧回调丢失不再让可见窗口陷入“能接收输入但不重绘”。窗口没有真正最小化时会忽略遮挡误报，既有的 1 Hz 窗口心跳也会以幂等方式释放卡住的 `occluded` 与 `has_frame` 门控；看门狗现在在建窗时即刻武装，首帧之前的冻结也能在一秒内自愈。（#21、#32）
- **盘符根目录右键启动可用** — 在 `D:\` 这类盘符根目录背景右键「在 Nebula 中打开」此前报「无效的工作目录」：资源管理器把 `%V` 展开成 `D:\`，末尾反斜杠在命令行上转义了收尾引号。被吃掉的路径现在会被修复；错误提示里的日志变量改用 PowerShell 语法（`$env:NEBULA_LOG`），复制即可用。（#36）
- **多行粘贴不再打断 codex** — 多行粘贴确认现在只在换行真的会被 shell 逐行执行时弹出。处于 bracketed paste 模式的应用（codex、vim、新版 PSReadLine）会把粘贴当作一个整体接收，警告不再拦路。确认框里深色主题下 accent 主按钮上的「Enter」键帽也改从按钮自身墨色派生，不再是一块近黑色。（#35）
- **公式覆盖层兼容被 markdown 反转义的定界符（GPUI 壳）** — 部分 AI CLI 先渲染 markdown 再上屏，`\[ \]` / `\( \)` 的反斜杠被当作 markdown 标点转义吃掉，屏幕上只剩裸 `[ … ]` 块与 `(\sqrt{…})`，公式从不渲染。共享扫描器现在识别这些裸形态：内容须携带已知 TeX 命令（`\int`、`\frac`……）且方括号独占行首/行尾；JSON 数组、`[INFO]` 日志、正则 `(\d+)` 保持原样。GPUI 壳里 TeX 显式长度（`\\[6pt]`、`\kern`）的 pt→px 换算也已对齐旧壳。
- **Markdown 阅读器铺满阅读列（GPUI 壳）** — 段落内的软换行（README 手工折行的正文）此前渲染成硬换行，右侧留出一大片参差空白。现按 CommonMark 合并为空格，中日韩相邻行直接相连不插空格。
- **Markdown 图片真正能加载（GPUI 壳）** — 文档 tab 现在把相对图片路径按文档所在目录解析、绝对路径直接读盘、`http(s)` 图源经后台 ureq 客户端拉取（gpui 默认装的是空客户端），README 的 logo、shields 徽章（含 SVG）与截图终于都能显示。本地和网络的 GIF / 动画 WebP 会先压成单帧 PNG：gpui 在多帧图的帧下标被静态图复用时会直接 panic。
- **Markdown 里的原生 HTML 按 GitHub 语义渲染（GPUI 壳）** — README 常用的 HTML 写法现在与 GitHub 渲染一致：`<p>` / `<div>` / 标题上的 `align="center"` 让整块居中；同一段落里连续的 `<img>` 徽章在同一行居中横排（此前一枚一行竖着摞）；行内 `<br/>` 按文档顺序断行（此前会被提到所跟文本之前）；`<a><img/></a>` 链接徽章不再整个丢失——图片正常显示且带链接。
- **Codex `notify` 不再自我包装成起不来的配置** — 另一个 notify 包装器（codex-computer-use）重新注册时若认不出 Nebula 的 helper，会把整个旧数组序列化进自己的 `--previous-notify` 参数；Nebula 再包一层，转义反斜杠每轮翻倍，最终把 `config.toml` 撑到 130 MB，Codex 直接起不来。现在只要 helper 标记出现在**任何位置**（含 JSON 字符串内部）就算已接线，Nebula 只自愈自己的路径、绝不再包。序列化后超过 8 KB 的 `notify` 一律拒写，兜住其他形态的膨胀；`nebula setup-ai --remove` 会持久化 `ai_hooks=0`，重启后也不再自动装回。（#38）
- **Tab 右键菜单阴影不再随标签数变厚（GPUI 壳）** — 组件库的右键菜单扩展用固定元素 id 存状态，于是每个标签行都解析到同一份状态、把同一个已打开的菜单重复画在同一锚点上。菜单面板不透明，阴影不是：8 个标签就叠 8 层投影。现在 Tab 菜单与文件树菜单一样，由 workspace 根只画一次。
- **窗口缩放后终端光标不再漂移** — conhost 在 resize 后会以非确定的方式塌缩屏幕缓冲区且零重绘，光标可能停在离 shell 认知好几行的位置。Nebula 现在在每次 resize 后通过 AttachConsole 探针对账真实光标位置并重新对齐网格。
- **默认 Shell 会加载用户 PowerShell 配置文件** — 默认启动此前跳过了 `$PROFILE`，其中定义的提示符、别名与 PSReadLine 设置从未生效。（#30）
- **先缩小再放大后 reflow 不再丢内容** — 窗口变窄再变宽不会丢掉折行内容，窗口也有了真正的最小尺寸。
- **光标焦点与闪烁，以及启动期渲染门控冻结** — 光标状态机正确跟随焦点，看门狗会释放可能让窗口可见却不重绘的启动门控。（#21）
- **新界面里 Esc 能送到 Claude Code** — win32-input 编码器把控制键的 `uChar` 硬填成 0，而 OpenConsole 会丢弃这种事件，读字节流的 CLI 因此收不到 Esc。
- **关闭窗口确认会把忙碌进程算进去** — 构建或 AI CLI 正在运行时关闭窗口，会指名是哪个程序而不是直接关掉。
- **通知贴到右下角。**
- **背景模糊按配置的不透明度合成。**
- **Shell 选择器品牌图标不再发虚** — 图标按整数物理像素预缩放，不再由 GPU 拉伸。
- **“宽松”宽度模式下内置字形填满单元格。**
- **标签启动身份与整行命中区** — 恢复出的标签保留它启动时的程序，整行都可点击而不只是标签文字。

#### 改进

- **文件/Git 侧栏刷新不再阻塞渲染** — 目录遍历与 Git 状态子进程改为在工作线程中生成快照，旧内容在刷新期间仍可正常使用。完成后的快照会整体替换；根目录已变化的过期结果会被丢弃，正在显示的搜索结果也不会被覆盖。
- **GPUI 壳 Tab 折叠箭头与文件树菜单** — TABS 折叠控件改用壳自带的线性 Chevron，不再用 Nerd Font 字形。文件树右键菜单不再叠上抽屉阴影，观感与 Tab 菜单一致。
- **字体选择器用候选字体自己的字形预览** — 每个字体族在确认前就以自己的字面呈现。

## 1.0.0 - 2026-08-10

### English

#### Updated

- **Updated network settings** — the page now has three clear choices: No proxy, Follow system, and Use proxy. The network test is at the top of the page, and changing the choice or address takes effect immediately without restarting Nebula.
- **Updated proxy routing** — Follow system reads the operating system proxy only; terminal variables such as `ALL_PROXY`, `HTTP_PROXY`, and `HTTPS_PROXY` no longer change SSH or SFTP routes. The old per-host proxy field is removed from `ssh_profiles.json`, while OpenSSH `ProxyJump` remains supported.

#### Fixed

- **Fixed inline formulas split by terminal wrapping** — a formula such as `$e^{i\\pi}+1=0$` still renders when the terminal wraps it onto the next screen row, while a real line break still ends the inline formula.
- **Fixed the network page showing an obsolete direct-host row** — all three modes now avoid displaying the unused bypass-host input.

#### Improved

- **Improved settings color feedback** — selected and hovered controls now use one softly tinted version of the active theme color in both light and dark themes, avoiding abrupt green-to-blue transitions.
- **Improved saved-host rows** — host cards use the same pale theme-tinted surface as the selected navigation item, so settings sections read as one consistent interface.

#### Added

- **Encrypted backup and restore** — Settings can export selected Nebula-owned data into an AES-256-GCM archive protected by an Argon2id-derived passphrase, then authenticate and restore it with path-traversal and symlink-parent checks. Appearance, configuration, sanitized SSH profiles, sync, assistant data, sessions, directory and command history, and imported fonts can be selected independently.
- **A dedicated image viewer tab** — double-clicking PNG, JPEG, WebP, or BMP files in the file tree opens an independent image tab that scales the decoded image to the content area. Markdown images reuse the same decoder and renderer instead of maintaining a second image path.
- **A dedicated SSH settings page** — the sidebar's ordered SSH hosts now have a two-line card view with connect, edit, hide, add-host, and immediate `~/.ssh/config` import actions under Settings → SSH. The global network mode and proxy address live on their own Network page; the existing sidebar remains a fast connection entry point.
- **Shared overlay scrollbars** — Markdown, tab lists, and SSH hosts now use the same unobtrusive scrollbar: a 3 px thumb appears only for overflowing hovered content, with a forgiving 12 px pointer target plus drag and track-click navigation.
- **A unified Files/Git drawer header** — Files and Git are two centered slots in one segmented control. The file tools row keeps the current root on one line and provides follow-current-terminal (`Alt+R`), new-terminal-here (`Alt+T`), and reveal-in-file-manager (`Alt+O`) actions with matching hover tips.
- **Input latency probes** — launching with `NEBULA_INPUT_LATENCY=1` logs per-segment timings (key event → PTY write, PTY wakeup → frame, key → frame) through the debug log, turning "typing feels slow" into a number that names the slow segment. Disabled probes cost one initialized-`OnceLock` read.
- **SSH connections can go through a jump host or the system proxy** — `ProxyJump` from `~/.ssh/config` remains supported, and SSH/SFTP connections use the shared global network setting. The system mode reads the Windows system proxy instead of terminal environment variables; multi-hop chains are rejected with an explanatory error.
- **The font picker lists installed system fonts** — the font selector merges Windows system font families with imported ones: monospaced families (as reported by DirectWrite, not guessed from glyph widths) are shown by default, the full list is one click away, and a real search box — caret, selection, and clipboard shortcuts included — filters by display name. Enumeration is lazy and stays off the startup path. Applying a font is transactional: validated first, rolled back on failure; proportional families are marked, and a startup fallback shows a notice. (Thanks to @Sakyvo.)
- **A compact appearance preset** — Settings → Appearance gains an interface density choice: Standard keeps the current look, while Compact trims padding, steps every corner radius one rung down the existing ladder, and forgoes decorative glows across the title bar, sidebar, settings, command palette, file drawer, and dialogs. No new visual values are introduced — compact takes the next smaller step of the existing spacing and radius ladders, expressed as relations (`radius::overlay(density)`, `control::row(density)`) rather than a parallel constant table. (Thanks to @Sakyvo.)

#### Fixed

- **Codex can distinguish Shift+Enter from Enter on Windows** — Nebula now enables ConPTY's Win32 input mode when creating a pseudo console, tracks the application's DECSET 9001 request, and emits the Win32 key records expected by console applications. Codex can therefore insert a newline with Shift+Enter instead of receiving an ordinary Enter submission.
- **Esc reaches Claude Code and other byte-stream readers under Win32 input mode** — the Win32 key records emitted for control keys carried `UnicodeChar=0`, while a real keyboard reports Esc=27, Enter=13, Tab=9 and Backspace=8; the bundled OpenConsole drops an Esc record without its character, so programs that consume the translated byte stream (node/Ink, hence Claude Code) never saw Esc at all, while VK-based readers (Codex) were unaffected. Control keys now carry their true `KEY_EVENT_RECORD` character (the OS text with modifiers applied when available, e.g. Ctrl+Enter→LF), and `scripts/win32_input_matrix.ps1` replays the key matrix against a recorded baseline to keep it that way. Root-cause walkthrough in `docs/hard_lessons.md`.
- **ConPTY sessions receive window focus reports** — focus in/out (`CSI I`/`CSI O`) was only sent when an application subscribed via DECSET 1004, so ConPTY could not synthesize the `FOCUS_EVENT_RECORD`s Win32 console programs read. Following the ConPTY keyboard spec, focus reports are now also sent whenever Win32 input mode is active; the host consumes them itself and still forwards the VT form only to 1004 subscribers, verified for both zero leakage and correct delivery.
- **Held modifiers no longer dangle across Alt+Tab** — switching windows mid-chord delivers the modifier's real key-up to the newly focused window, so any protocol stream that had reported the key-down (Win32 records or extended keyboard events) left the application believing the modifier was held forever; worst case, the first plain keystroke after returning was read as a Ctrl-chord and could kill a running task. Nebula now synthesizes protocol-correct key-ups for every held modifier at the moment focus is lost.
- **Live window drags no longer flood ConPTY with resizes** — every queued intermediate size used to reach `ResizePseudoConsole`, and the console host performs a full viewport reflow per call. The PTY event loop now applies only the newest size per channel drain: intermediate sizes are superseded, the final size always lands, and the slower the host reflows the harder the coalescing works.
- **Nebula starts even when the in-box ConPTY rejects Win32 input mode** — `CreatePseudoConsole` was called unconditionally with the Win32-input flag and a failure hit a process-killing assert. The call now retries without the flag and returns a real error instead of dying; a flagless host never requests DECSET 9001, so the input stack self-gates down to legacy VT end to end.
- **A crashed console host no longer leaves a zombie tab** — when the PTY transport died without the shell exiting (host crash, pipe or poller failure), the I/O thread quit silently and the tab kept accepting input into the void. The failure now surfaces its reason on the message bar, lands in the debug log, and runs the normal session teardown.
- **Occasional stalls while typing Chinese in PowerShell** — the renderer pushed the IME caret rectangle to the input method every frame, cursor blink and output scrolling included, and on Windows each push is a chain of synchronous cross-process IMM32 calls into the input-method host; whenever that host was busy, the render thread waited on it. The rectangle is now deduplicated and only pushed when it actually changes, and the cache is invalidated on focus changes, IME enabling, and IME re-association so the first composition after returning never lands at a stale position.
- **Missing AI hook warnings no longer repeat every frame** — the notice is emitted only when hook availability changes into the missing state, preventing an unavailable optional hook from producing an unbounded warning loop.
- **Installer version metadata matches the application** — the Inno Setup fallback version and numeric file version now match Nebula 0.9.0; release builds continue to inject the package version automatically.
- **A specified private key no longer triggers the system passphrase dialog** — the russh dependency had dropped its default `rsa` feature while switching to the ring backend, so every RSA private key — including the classic PKCS#1 `.pem` downloaded from cloud consoles — failed to parse locally; the failure was misclassified as "the key has a passphrase", popped the Windows credential dialog, and finally reported "the server rejected the key" although the server never saw it. The `rsa` feature is restored, a parse failure is reported as the local problem it is (only genuinely encrypted keys enter the passphrase flow), and the footer connection test never prompts — it reports "key is passphrase-protected" instead, honouring its no-interaction contract.
- **The terminal card's corners no longer show a pale fringe** — the rounded card and the shell's concave corner patches were two independently anti-aliased arcs blended in sequence, which mathematically leaks a sliver of the clear color along the arc (and the desktop behind the window once transparency is on). The corner patches now render underneath the card inside the backdrop pass, making the card's own arc the only visible seam, and the clear fallback uses the composited shell tone shared with the chrome strips.

#### Improved

- **The launcher palette now follows the grouped Shell/SSH design** — the three filters are reduced to All, SSH, and Shell; quiet chips remain text-only until hover or selection, while the selected state gets a restrained pill and hairline ring. Chip geometry uses real UI-font metrics so multi-column labels and double-digit counts stay inside the pill. Recommended shells, all shells, and SSH hosts use smaller section captions, full-width hairlines, equal-height rows, 28 px neutral icon tiles, stable panel height, a softer outer radius, and shared search/filter/list geometry. Opening it also dims the surrounding workspace so the active surface is unambiguous.
- **Settings navigation is compact, icon-led, and quieter** — the rail now matches the 196 px reference shell with 32 px rows, 2 px gaps, dedicated vector icons, wider internal label spacing, and a neutral low-contrast selected state. The backup surface has also been reorganized around an automatic-backup summary, an export/restore segmented action, and a grouped manifest with descriptions and sizes; its sidebar entry remains hidden until restore preview, conflict handling, and rollback are ready.
- **The Windows installer can register Nebula as a command** — "Add Nebula Terminal to the user PATH" is selected by default, `nebula.exe` is also registered through Windows App Paths for Win+R, and uninstall removes only the PATH entry that the installer added. Existing Explorer directory and directory-background context-menu registrations remain included in the installer.
- **File-tree status is quieter and stable** — directories use neutral theme colors, ignored paths detected through batched `git check-ignore` are dimmed without changing sorting or filtering, and hovering a row no longer shifts its contents.
- **Saved hosts in Settings → SSH show their OS icon** — each row now draws the same per-host icon as the sidebar (real ink width measured, then optically scaled to one target size), replacing the earlier neutral status ring; `auto` and unrecognized ids fall back to the generic terminal shape. The icon says which machine this is — it does not invent an online state.
- **Overlay panels draw from one component set** — the command palette, the Ctrl+K launcher, the Ctrl+Shift+O session picker, and combobox dropdowns now share one `overlay_list` module for option rows, icon tiles, identity chips, footer hints, and the query caret, and the SSH settings row actions share one outline-button widget. Selection and hover treatments are now identical everywhere; the migration also removed a hidden asymmetry where selected pills were 2 px taller than hover pills.
- **The Network page shows only what the selected mode needs** — the page keeps the mode selector, a manual proxy address when needed, and the network test. Obsolete local proxy scans, per-host overrides, and direct-host input are no longer presented; saved SSH hosts use the global network setting while OpenSSH `ProxyJump` remains a backend capability.
- **One selection tint everywhere** — the settings navigation, the sidebar's active tab, and the Network page's choice rows now share the design prototype's accent-soft selection wash (≈ rgb(52,71,99) composited on the dark theme); light themes stay neutral automatically because the token derives from each theme's accent.
- **The key-bindings page gains groups, search, and clash warnings** — actions are grouped under Global / Tabs / Panes / Side panels / Terminal with quiet section headers only (no frames), a search box filters actions and keys as you type and folds empty groups away, duplicate bindings paint both keycaps in the danger tint plus a warning naming which action stops firing, keycaps follow the prototype's tiering (surface base, thick bottom lip, dim ink lifted on hover), and hovering a row reveals the Rebind affordance.

### 简体中文

#### 更新

- **更新网络设置** — 网络页现在只保留三个直观选项：“不使用代理”“跟随系统”“使用代理”。“测试网络”放在页面最上方，切换选项或修改地址会立即生效，不需要重启 Nebula。
- **更新代理读取方式** — “跟随系统”只读取操作系统代理，不再受 `ALL_PROXY`、`HTTP_PROXY`、`HTTPS_PROXY` 等终端环境变量影响。`ssh_profiles.json` 中旧的主机代理字段已移除，但 OpenSSH 的 `ProxyJump` 仍然支持。

#### 修复

- **修复行内公式被终端换行拆开后不显示** — `$e^{i\\pi}+1=0$` 即使跨到下一屏幕行也会正常渲染；真正的换行仍会结束行内公式。
- **修复网络页残留“直连目标主机”输入行** — 三种模式都不再显示这条已经不用的设置。

#### 改进

- **改进设置页的颜色反馈** — 选中和悬停使用当前主题的同一套主题色，浅色与深色模式都保持柔和、干净的过渡，不再出现绿色跳成蓝色的突兀变化。
- **改进设置页主机行** — 已保存主机使用淡色主题底，和导航选中状态保持一致，减少不同区域之间的颜色跳变。

#### 新增

- **加密备份与恢复** — 设置页可将选中的 Nebula 数据导出为 AES-256-GCM 加密归档，密钥由 Argon2id 从口令派生；恢复前会完整认证，并防止路径穿越和符号链接父目录写入。外观、配置、已脱敏的 SSH 配置、同步、助手数据、会话、目录与命令历史以及导入字体均可独立选择。
- **独立图片查看器标签页** — 在文件树中双击 PNG、JPEG、WebP 或 BMP 会打开独立图片标签页，并按内容区域等比适配。Markdown 图片复用同一套解码与渲染路径，不再维护重复实现。
- **独立 SSH 设置页** — 侧栏同源、保持原顺序的 SSH 主机现在以双行卡片呈现，并提供连接、编辑、隐藏、添加主机和立即导入 `~/.ssh/config`；全局网络模式和代理地址集中在“设置 → 网络”。侧栏仍保留为快速连接入口。
- **共享 OverlayScrollbar** — Markdown、标签列表和 SSH 主机列表统一使用克制的浮层滚动条：内容溢出且鼠标进入时才显示 3px thumb，实际命中区为 12px，并支持拖动和点击轨道跳转。
- **文件/Git 一体化抽屉头部** — 文件与 Git 成为同一分段控件内居中的两个槽位；文件工具行单行显示当前根目录，并提供跟随当前终端（`Alt+R`）、在此新建终端（`Alt+T`）和在资源管理器中打开（`Alt+O`），hover 提示与快捷键保持一致。
- **输入延迟打点** — 以 `NEBULA_INPUT_LATENCY=1` 启动后，调试日志会记录分段耗时（按键事件→PTY 写入、PTY 唤醒→帧、按键→帧），把“打字感觉慢”变成能指出慢在哪一段的数字。探针关闭时的开销仅为一次已初始化 `OnceLock` 读取。
- **SSH 可经跳板机或系统代理连接** — `~/.ssh/config` 的 `ProxyJump` 仍然支持，SSH/SFTP 连接统一使用网络页的全局设置；“跟随系统”读取 Windows 系统代理，不读取终端环境变量。多级跳板链会得到明确报错。
- **字体选择器列出系统已安装字体** — 字体选择器把 Windows 系统字体族与导入字体合并去重：默认只显示等宽家族（由 DirectWrite 权威判定，不用字形宽度启发式），一键展开全部；弹层顶部是正经搜索框（光标、选区、剪贴板快捷键俱全），按显示名过滤。枚举惰性执行，不进启动路径。应用字体是事务性的：先验证再持久化，失败回滚到当前字体；非等宽家族有标记，启动回退时也会给出提示。（感谢 @Sakyvo。）
- **紧凑外观预设** — 设置 → 外观新增界面密度选项：标准保持现状；紧凑减少留白、把所有圆角在既有阶梯上整体下移一档，并去除顶栏、侧栏、设置、命令面板、文件抽屉与对话框的装饰性光晕。不引入任何新的视觉数值——紧凑取的是既有间距与圆角阶梯的更小一档，以关系函数（`radius::overlay(density)`、`control::row(density)` 等）表达，而非平行常量表。（感谢 @Sakyvo。）

#### 修复

- **Windows 下 Codex 现在能区分 Shift+Enter 与 Enter** — Nebula 创建伪控制台时启用 ConPTY Win32 输入模式，跟踪应用发出的 DECSET 9001 请求，并发送 Windows 控制台程序所需的 Win32 按键记录。因此 Codex 中 Shift+Enter 会插入换行，不再被当作普通 Enter 提交。
- **Win32 输入模式下 Esc 能到达 Claude Code 等字节流读取方** — 控制键的 Win32 按键记录此前带着 `UnicodeChar=0`，而真实键盘上 Esc=27、Enter=13、Tab=9、Backspace=8；随附的 OpenConsole 会丢弃不带字符的 Esc 记录，导致按翻译后字节流消费输入的程序（node/Ink，即 Claude Code）完全收不到 Esc，按虚拟键码识别的程序（Codex）则不受影响。现在控制键携带真实 `KEY_EVENT_RECORD` 字符值（可用时取操作系统带修饰的文本，如 Ctrl+Enter→LF），并新增 `scripts/win32_input_matrix.ps1` 按基线回放键位矩阵防止回归。完整根因见 `docs/hard_lessons.md`。
- **ConPTY 会话现在能收到窗口焦点报告** — 焦点进出（`CSI I`/`CSI O`）此前只在应用通过 DECSET 1004 订阅时发送，ConPTY 因此无法合成 Windows 控制台程序读取的 `FOCUS_EVENT_RECORD`。按 ConPTY 键盘规范，Win32 输入模式激活时也发送焦点报告；宿主自行消费并仅向 1004 订阅者透传 VT 形式，零泄漏与正确送达均已验证。
- **修饰键不再因 Alt+Tab 悬挂** — 按住修饰键切窗时，真实的 key-up 发给了新聚焦的窗口，导致所有上报过 key-down 的协议流（Win32 记录或扩展键盘事件）让应用永远认为修饰键仍被按住；最坏情况下，切回后按下的第一个普通键会被解读为 Ctrl 组合并可能误杀正在运行的任务。Nebula 现在在失焦瞬间为所有按住的修饰键合成符合协议的 key-up。
- **拖动窗口不再向 ConPTY 倾泻 resize** — 此前队列中每个中间尺寸都会到达 `ResizePseudoConsole`，而控制台宿主每次调用都做整个视口的 reflow。PTY 事件循环现在每轮排空只应用最新尺寸：中间尺寸被后来者取代、最终尺寸必达，且宿主 reflow 越慢合并力度越大。
- **内置 ConPTY 拒绝 Win32 输入模式时 Nebula 仍能启动** — `CreatePseudoConsole` 此前无条件携带 Win32 输入标志，失败即触发进程级 assert。现在会去掉标志重试并返回真实错误而非直接崩溃；未带标志创建的宿主不会请求 DECSET 9001，输入栈端到端自动降级到传统 VT。
- **控制台宿主崩溃不再留下僵尸标签页** — PTY 传输在 shell 未退出时死亡（宿主崩溃、管道或轮询器故障）此前会让 I/O 线程静默退出，标签页继续把输入吞进黑洞。现在故障原因会显示在消息栏、写入调试日志，并走正常的会话收尾流程。
- **PowerShell 中文输入偶发卡顿** — 渲染层此前每帧都把 IME 光标矩形推给输入法（光标闪烁、输出滚动都算帧），而 Windows 上每次推送是一串同步跨进程的 IMM32 调用，直达输入法宿主进程；宿主一忙，渲染线程就跟着等。现在推送前做值去重，矩形没变就不再打扰输入法；焦点切换、输入法启用与重新关联时作废缓存强制重推，回焦后的第一次组词不会落在陈旧位置。
- **AI hook 缺失警告不再逐帧重复** — 只有 hook 可用性切换到“缺失”状态时才提示一次，避免可选 hook 不存在时产生无上限的警告循环。
- **安装器版本信息与应用一致** — Inno Setup 的兜底版本和数字文件版本改为 Nebula 0.9.0；正式构建仍会自动注入包版本。
- **终端卡四角不再泛白边** — 圆角卡片与壳层凹角补片是两条各自抗锯齿的弧按先后顺序混合，弧线上必然漏出一丝清屏色（开启透明度后直接漏出窗口后面的桌面）。凹角补片改为在 backdrop 里画在卡片**之下**，卡片自己的圆弧成为唯一可见接缝；清屏兜底色也换成与 chrome 条带同源的壳层合成色。
- **指定私钥不再触发系统口令弹窗** — russh 依赖在切换 ring 后端时把默认的 `rsa` feature 一并关掉了，导致所有 RSA 私钥（包括云厂商控制台下载的经典 PKCS#1 `.pem`）本地解析失败；这个失败被误判成「密钥有口令」，弹出 Windows 凭据对话框，最终还报成「服务器拒绝了指定的私钥」——其实服务器根本没见过这把钥匙。现在补回 `rsa` feature，解析失败按本地问题如实报错（只有真正加密的密钥才进入口令流程），页脚的「测试连接」也绝不弹框——改为报告「私钥受口令保护」，兑现无人值守承诺。

#### 改进

- **启动器面板改为 Shell / SSH 分组设计** — 筛选项收敛为“全部、SSH、Shell”；未选中 chip 保持纯文字，悬停或选中才出现克制的胶囊底，选中态带很弱的发丝描边。chip 宽度按 UI 字体真实度量计算，双列文字和两位数计数都不会越出圆角底。推荐 Shell、所有 Shell 与 SSH 主机使用更小的分组标题、贯穿内容宽度的细分隔线、等高条目、28px 中性图标底块、更柔和的面板圆角和固定高度；搜索、筛选、列表继续共用同一套几何。打开面板时同时压暗周围工作区，当前操作层级更明确。
- **设置导航更紧凑、图标化且更安静** — 左侧栏对齐 196px 原型，使用 32px 条目、2px 间隔、独立矢量图标、更宽松的内部文字间距，以及低对比度的中性选中态。备份页也重排为自动备份摘要、导出/恢复分段操作和带描述/大小的分组清单；在恢复预览、冲突策略与回滚流程完成前，左侧入口暂时隐藏。
- **Windows 安装器可以把 Nebula 注册为命令** — “将 Nebula Terminal 添加到当前用户 PATH”默认勾选，同时通过 Windows App Paths 注册 `nebula.exe`，因此 Win+R 可以直接启动；卸载时只移除安装器自己加入的 PATH 条目。资源管理器目录及目录背景的右键菜单注册仍随安装包提供。
- **文件树状态更安静且不抖动** — 目录使用主题中性灰，批量 `git check-ignore` 识别出的忽略项只降低显示强度、不参与排序或过滤，鼠标悬浮也不再移动行内容。
- **设置 → SSH 的已保存主机显示各自的系统图标** — 每行左缘画与侧栏同源的 per-host 图标（按真实墨迹宽度光学缩放到统一尺寸），替代此前的中性状态环；`auto` 与未识别的 id 回落通用终端形状。图标只回答“这是哪台机器”，不发明在线状态。
- **浮层面板共用一套组件** — 命令面板、Ctrl+K 启动器、Ctrl+Shift+O 会话快跳与下拉框的选项行、图标底块、身份 chip、页脚提示和查询光标统一收进 `overlay_list` 组件，SSH 设置页的行内动作也共用一个 outline 按钮组件。各处的选中/悬停形态从此完全一致——迁移顺带消除了一个隐藏不对称（选中药丸此前比悬停药丸高 2px）。
- **网络页按所选模式显示内容** — 页面保留模式选择、需要时的代理地址和网络测试；旧的本机代理扫描、每主机覆盖和“直连目标主机”输入不再显示。已保存主机统一使用网络页的全局设置，OpenSSH `ProxyJump` 仍由后端支持。
- **选中色全局统一为一个 token** — 设置导航、侧栏活动标签与网络页单选行共用原型的 accent-soft 选中底（深色主题合成后 ≈ rgb(52,71,99)）；浅色主题的这个 token 本身就是中性灰，无需另设分支。
- **按键映射页新增分组、搜索与冲突提示** — 动作按 全局 / 标签页 / 窗格 / 侧栏面板 / 终端 分组，只用段标题与间距分隔（无框保持干净）；搜索框即输入即过滤动作名与按键，被滤空的组连标题一起收起；重复绑定会把两行键帽标成 danger 色，并配一条写明「谁不生效」的警告；键帽按原型分级（surface 底、厚底边、弱墨 hover 提亮）；悬停行浮现「改键」入口。

## 0.9.0 - 2026-08-02

### English

#### Added

- **Idle tabs show their shell** — a quiet tab row (no badge, not hovered) shows its shell as a small dim tag at the right edge: `pwsh`, `cmd`, `bash`, `ubuntu` (WSL distributions by name), and `ssh` for SSH tabs. Any badge (spinner, dot, attention…) outranks it, and the tag steps aside when a long name reaches the right edge.
- **Section headers count what is in them** — the sidebar's TABS and SSH HOSTS headers carry a small count chip, and the disclosure chevron moved to the front of the title so several sections line their chevrons into one column. The count stays visible while a section is collapsed: it is the only information left there, and "42 hosts" versus "3 hosts" is what decides whether you scroll or search.
- **WSL distributions in the folder picker** — "Browse folder" dialogs now pin every registered WSL distribution (`\\wsl.localhost\<distro>`) to the top of their sidebar, so picking a Linux directory no longer depends on the system's "Linux" navigation node being present. (#12)
- **Drag to resize the panels** — Settings → Interaction gains "Drag to resize panel widths". With it on, the sidebar's right edge and the file/git drawer's left edge become drag handles (the pointer turns into a resize cursor over the ±4 px grip), and dragging either panel all the way to its window edge closes it rather than stopping at a minimum width. Off by default, and turning it on asks for confirmation first, because a width drag reflows the terminal live. The drag itself is double-throttled — the panel edge tracks the pointer every frame, but the grid is only re-laid when the width has crossed a whole cell and at most once every 80 ms, so a fast drag cannot turn into a reflow storm. The SSH HOSTS divider is always draggable regardless of the switch — it only redistributes height inside the sidebar and never touches the grid, and the height you drag it to is honoured even when you have fewer hosts than fit. All three sizes persist.
- **Restore last tabs on launch is now a setting** — Settings → Advanced → Sessions gains "Restore last tabs on launch (also after a crash)", on by default. Turning it off always starts clean; the snapshot keeps being written either way, so workspace export and crash diagnostics still work.
- **Window background blur (Windows 11)** — Settings → Appearance gains "Background blur". With it on, whatever is behind the window is blurred through the terminal background (Acrylic), and the window-opacity slider becomes the strength of the tint laid over that blur, so one control still governs "how much shows through". Mica was tried and dropped: it samples the desktop wallpaper rather than the actual window behind, which reads as a flat colour wash instead of a blur. Note that a translucent background forces text anti-aliasing from subpixel to grayscale — that is a system-level constraint of transparent windows, not a rendering regression.
- **The command palette is grouped** — rows now sit under quiet section headers with a hairline rule running to the panel edge: Working directory, Tabs, Jump, View, Workspace, Appearance, Settings, and the shell/profile list. The **Working directory** group carries the focused pane's path on the header itself — written once instead of repeated on every row — and holds "Copy path" and "Reveal in file manager"; "New tab" joins it only when a new tab would actually open there (a configured startup directory takes precedence, and then the row stays under Tabs rather than sitting under a heading that lies). Toggle commands ("Show tab sidebar", "Drag to resize panels") carry a check mark for their current state, so a switch that is already on reads differently from an action that would turn it on. Grouping is applied after ranking, so a group's internal order is still most-recently-used first (or best fuzzy match while typing), and the headers track the scrolled window rather than the top of the list.
- **SSH connection progress** — connecting to a saved host now shows the four real stages (resolve, TCP, authenticate, open session) instead of a blank pane. The stages are call sites in Nebula's own SSH implementation rather than guesses parsed from a client's output, so the progress is truthful. The page waits 350 ms before appearing: connections that finish faster than that never flash a progress screen, and hovering a host in the sidebar shows the state inline instead of taking over the pane.

#### Fixed

- **A crash is now told apart from a clean exit** — the session snapshot records whether the process reached its teardown. After an unclean exit (crash, force-kill, power loss) the restored window says so in a transient success toast instead of restoring silently; and when the crash-loop breaker trips after three failed launches, the offending session is moved to `session.crashed.json` instead of being overwritten by the next autosave a second later — previously the only evidence of a restore-crash loop destroyed itself.
- **Recovery notices use the right layer** — a completed recovery is a short success toast, while a crash-loop breaker notice remains in the message bar with the quarantined session path available for follow-up.
- **The message-bar close button stays visible with CJK text** — wide characters are measured in terminal columns and the close control is drawn as a reusable UI widget, so long multilingual notices no longer push the button off-screen; its plate and hover state make the control visibly clickable.
- **Sharp text and icons in the SSH host editor** — the identity strip's name, the avatar shape and the icon list previously scaled up bitmaps rasterized at the terminal font size, visibly blurry next to the design; they now re-rasterize at their true size. Caret placement in the enlarged name field follows the true glyph advance, so clicking the tenth character no longer lands the caret a column off.
- **The icon picker no longer shows form text through its panel** — overlay backgrounds were submitted in the same batch as the form's fills, which all paint beneath text; overlays (the icon list, the test-status tooltip) now paint after the form's text, so nothing bleeds through.
- **Test-connection failures show the whole reason** — the status line used to flatten the error into one truncated line with the full text hidden behind a hover tooltip. Failures now wrap up to four lines right in the dialog; the tooltip remains only for over-long tails.
- **Spinner rings render as smooth rings** — the beaded look came from translucent dots doubling their alpha wherever they overlap, and the active tab's ring composited against the sidebar background instead of the light pill it actually sits on, which read as a dark circle. Ring colors now pre-compose against the row's real background.
- **The "needs your answer" badge shows up for codex** — codex's notify pipe only reports "turn complete", so interactive question prompts could only ever show the unread dot. On turn completion Nebula now checks the visible tail of the screen for prompt markers ("enter to submit", "(y/N)"…) and upgrades the badge to the attention marker.

#### Improved

- **The icon picker's search box is a real input field** — click to place the caret, drag to select, Shift+arrows / Home / End, Ctrl+A/C/V, with the same caret-blink rhythm as every other field; the hint still shows while it is empty.
- **New SSH hosts default to password authentication** — "Auto" needs a configured key to succeed, which made the old default a guaranteed first failure for most people. Existing hosts keep whatever they saved.
- **The raised-hand badge is withdrawn for now** — even with wider fingers and tighter gaps the shape did not read as a hand at badge size. "Needs your answer" is an amber dot until the icon is redrawn; it still outranks the blue unread dot, and the two are told apart by colour rather than by shape.
- **Sidebar rows breathe wider** — the row inset narrowed from 8 px to 4 px, giving the width to names, badges and shell tags; the sidebar's own width is unchanged.
- **Every text field shares one caret** — caret position, selection, click-to-place and the blink rhythm moved out of the individual dialogs and into the input component itself. Previously each field carried its own copy, which is why the SSH editor's enlarged name field and the icon search box each drifted from the others in small ways; they now behave identically, and new fields inherit the behaviour instead of re-implementing it.

### 简体中文

#### 新增

- **静默标签页显示 shell 类型** — 没有任何徽章、未悬浮的标签行，右侧以最淡的小字显示该标签的 shell 短标：`pwsh`、`cmd`、`bash`、`ubuntu`（WSL 按发行版名），SSH 标签显示 `ssh`。任何徽章（转圈、圆点、「等你批准」…）都比它优先；名字长到顶着右缘时短标自动让位。
- **分组标题显示数量** — 侧栏的 TABS 与 SSH HOSTS 标题带一颗数量 chip，折叠箭头移到标题前面，多个分组的箭头因此对齐成一条竖线。折叠之后数量仍然常驻：那时它是这一段仅剩的信息量，而「42 台」和「3 台」直接决定人是滚动找还是直接搜。
- **文件夹选择器可直达 WSL** — 「浏览文件夹」对话框把每个已注册的 WSL 发行版（`\\wsl.localhost\<发行版>`）钉进侧栏顶部，不再依赖系统资源管理器是否显示「Linux」节点。（#12）
- **侧栏可拖拽调节** — 设置→交互新增「拖拽调节侧栏宽度」。开启后左侧栏右缘与文件/Git 抽屉左缘成为拖拽把手（±4px 热区上光标变为调整宽度形态）；把左侧栏一路拖到最左、或把抽屉一路拖到最右，就直接收起该面板，而不是卡在最小宽度上。默认关闭，且开启前会先确认——宽度拖动会实时重排终端内容。拖动本身走双重节流：面板边缘每帧跟手，但网格只在宽度跨过一整个单元格时才重排，且最快 80ms 一次，快速拖动不会演变成 resize 风暴。SSH HOSTS 分界线不受该开关约束、始终可拖：它只在侧栏内部重新分配高度，碰不到终端网格；主机数量撑不满时，拖出来的高度也照样生效。三处尺寸都会保存。
- **启动恢复会话可开关** — 设置→高级→会话新增「启动时恢复上次的标签（异常退出后同样恢复）」，默认开启。关掉就永远干净启动；快照照写不误，工作区导出与崩溃诊断仍然可用。
- **窗口背景模糊（Windows 11）** — 设置→外观新增「背景模糊」。开启后窗口背后的内容透过终端底色被模糊（Acrylic），窗口不透明度滑杆随之变成盖在模糊层上的着色强度——「透出多少」仍然只由一个控件管。Mica 试过后放弃：它取的是桌面壁纸而不是窗口背后的真实内容，看起来是一层平涂色调而非模糊。另外，半透明背景会把文字抗锯齿从次像素降为灰度，这是透明窗口的系统级约束，不是渲染退化。
- **命令面板分组了** — 行归到安静的分组标题下，标题右侧一条发丝线拉到面板右缘：工作目录、标签页、跳转、视图、工作区、外观、设置，以及 shell / profile 列表。**工作目录**组把聚焦终端的路径挂在标题上——写一次，而不是每行重复一遍——组里是「复制路径」和「在资源管理器中显示」；「新建标签页」只在它确实会开在那个目录时才并入（配了启动目录时启动目录优先，那时这行留在标签页组，而不是挂在一个说谎的标题下）。开关类命令（「显示标签侧栏」「拖拽调节侧栏宽度」）带勾选态，已经开着的开关与「点了会开」的动作因此读起来不一样。分组是在排序**之后**做的稳定划分，组内仍然是最近用过的优先（打字时是模糊得分优先）；表头跟着滚动窗口走，而不是钉在列表开头。
- **SSH 连接进度** — 连接已保存的主机时不再是一片空白，而是显示四个真实阶段（解析、TCP、认证、建立会话）。这四个阶段是 Nebula 自己的 SSH 实现里的调用点，不是从某个客户端的输出里猜出来的，所以进度是可信的。连接页有 350ms 门槛：比这更快连上的连接不会闪一下进度屏；在侧栏悬浮某台主机时状态就地显示，不接管整个终端面板。

#### 修复

- **崩溃与正常退出现在分得清了** — 会话快照记录进程有没有走完收尾。上次是异常退出（崩溃、强杀、断电）时，恢复后会用自动消失的 success toast 明说，而不是悄悄恢复；连续三次启动失败触发断路器时，那份会话被挪到 `session.crashed.json` 而不是被一秒后的自动保存盖掉——此前「一恢复就崩」的唯一现场会自己销毁。
- **恢复提示分到正确的层** — 已完成的恢复成功提示走短暂的 success toast；断路器提示仍留在消息栏，并保留隔离会话路径供后续处理。
- **中文消息栏的关闭按钮始终可见** — 按终端显示列宽处理宽字符，关闭控件改为可复用 UI 组件自绘，中文长消息不会再把按钮挤出屏幕；常态底板与悬停反馈让它明显可点击。
- **SSH 主机编辑器的大字与图标不再发糊** — 身份条的名字、头像形状与图标列表此前是把按终端字号栅格化的位图硬放大，对着原型一眼可见的糊；现在按真实字号重新栅格化。放大后名字框的光标换算按真实字形步进，点第十个字不再偏一格。
- **图标选择器不再透出表单文字** — 浮层的底与表单同批提交时全部沉在文字之下；现在浮层（图标列表、测试状态提示）在表单文字之后单独提交，什么都透不上来。
- **「测试连接」失败显示完整原因** — 此前被压成单行截断，全文藏在悬浮层里；现在失败原因直接折行铺开（最多四行），只有超长的尾巴才收进悬浮层。
- **转圈指示器是平滑的圆环** — 珠链感来自半透明圆点在重叠处 alpha 翻倍；活动标签上的环还拿侧栏深底做合成，画在浅色药丸上就成了一圈黑。环的颜色现在按所在行的真实底色预合成。
- **codex 的「等你回答」徽章能亮了** — codex 的 notify 只报「回合完成」，交互式提问也只能落成未读圆点。现在回合结束时检查屏幕尾部的提问特征（"enter to submit"、"(y/N)"…），命中即把徽章升级为「等你批准」。

#### 改进

- **图标搜索框是一个正经输入框** — 点击定位光标、拖选、Shift+方向键 / Home / End、Ctrl+A/C/V，光标闪烁节律与其它输入框一致；空着时仍显示提示语。
- **新建 SSH 主机默认密码认证** — 「自动」要先配好私钥才走得通，拿它当默认等于让多数新手先撞一次失败。已保存的主机保持原样。
- **手掌徽章暂时下线** — 指再粗、缝再窄，徽章尺寸下还是读不成一只手。「等你批准」改用琥珀色圆点，等图标重画后再回来；它依然压过蓝色未读点，两者靠颜色而不是形状区分。
- **侧栏行更宽** — 行的左右内缩从 8px 收到 4px，宽度让给名字、徽章和 shell 短标；侧栏总宽不变。
- **所有输入框共用同一套光标** — 光标位置、选区、点击定位与闪烁节律从各个对话框里下沉到输入组件本身。此前每个输入框各存一份，SSH 编辑器放大后的名字框、图标搜索框于是各自在细节上跑偏；现在行为完全一致，新加的输入框直接继承，不必再实现一遍。

## 0.8.0 - 2026-07-29

### English

#### Added

- **Shell picker shortcut (Ctrl+K)** — opens the same shell/profile list the "+" chevron does, and closes it on a second press. The binding appears in Settings → Key bindings and can be rebound; by default it takes over the shell's `kill-line` (Ctrl+K → `\x0b`).
- **SSH connection testing** — the SSH host editor can now test its unsaved destination, password, authentication mode, and private keys before saving, with a 12-second timeout and inline connecting, success-time, or failure status.
- **Configurable quick-terminal shortcut** — Settings → Key bindings now exposes the global quick-terminal shortcut. Captured changes are applied immediately and remembered; if the operating system rejects a conflicting shortcut, Nebula keeps the previous working binding and shows the registration failure in the row.
- **Workspace export/import** — the command palette gains "Export workspace…" and "Open workspace…", and a tab's right-click menu gains "Export as workspace…". A workspace file (`.nebula-workspace.json`) records the tab list, each tab's full split layout (axis, ratio, per-pane working directory), tab names and colors, and each tab's launch identity: WSL/custom shells reopen with their shell, SSH tabs reconnect to their saved destination automatically. Files are portable across machines and platforms — a directory that does not exist on the importing machine falls back to the default one, and a shell program the machine lacks (e.g. `wsl.exe` on Linux) falls back to the default shell in the saved directory instead of dropping the tab.
- **Crash recovery now restores split layouts** — the continuously written session snapshot (1 Hz) uses the same schema as workspace files, so after a crash or force-kill Nebula reopens with every tab's split tree, ratios, per-pane directories and SSH/WSL launch identities — not just one pane per tab as before.
- **Grok is recognised as a running program** — a tab running `grok` or `grok-cli` shows the official xAI mark in its icon slot, the same way `claude` and `codex` already do. The light or dark mark is picked to match the chrome ink, and it is downsampled to the exact physical pixel size of the slot and optically centred on its ink, so it stays sharp at every DPI. (Thanks to @Sakyvo.)
- **Tab reveal motion is configurable** — Settings → Interaction gains a "Tab reveal" choice. `Slide` (default, unchanged behaviour) eases a tab into place when it becomes visible; `Instant` puts it there immediately. Drag-to-reorder keeps its spring in both modes. (Thanks to @Sakyvo.)

#### Fixed

- Nebula now always starts at its standard size — the configured `window.dimensions`, or the default 116×30 grid **priced at the config base font size**. Two regressions could previously blow the first window up to near-fullscreen: the launch replayed the saved window size from the session file (stale or wrong-unit values included), and the new font-size persistence fed the Ctrl+wheel zoom into the startup sizing formula, so 116 columns of an enlarged cell filled the screen. Startup now ignores both; a persisted zoom still renders after launch, it just shows fewer columns in the standard-sized window.
- Fixed the sidebar and other interface text zooming together with Ctrl+wheel, plus the hairline seams that appeared once the terminal font left its base size. Root cause was architectural: chrome shares the terminal's single font system, and several chrome paths (sidebar captions, tab/host labels, settings headings, the SSH editor, the message-queue entry) drew through the document-text path that follows the terminal zoom **by design**. All chrome typography now goes through a dedicated UI-anchored text path that rasterizes at the UI base size with its own baseline math, layout steps by the base font's actually-rasterized cell metrics, click targets read the same layout the pixels were drawn with, and the anchor state is correct from the very first frame of a restored session.
- Terminal font zoom is now clamped to a sane range (logical 4–64 px). Previously a stuck modifier key or trackpad burst could scroll the size past 180 px, where each wheel notch changes the size by under 1 % and zooming back out felt broken.
- Fixed the resize HUD ("80 × 24") drawing its label outside the centered box whenever the sidebar was open: the box was centered in window pixels while the text was centered on the terminal grid, whose origin carries the sidebar's asymmetric padding. Both now share one coordinate system, and the HUD no longer inflates with the terminal zoom it reports.
- Fixed the directory tree not following tab switches: an open SFTP panel no longer captures the drawer forever — the view routes by the focused pane (local tabs get the directory tree back, the SFTP connection stays warm), switching to a remote pane no longer blanks the tree, and WSL tabs launched as `wsl -d <distro>` now follow the shell's directory through `\\wsl$`.
- Fixed math rendering in WSL and remote SSH sessions: `$$ … $$` and `\[ … \]` display formulas printed by claude, codex, pi and similar tools running behind `wsl.exe` or `ssh.exe` now render natively — block detection no longer depends on spotting the AI CLI in the local process tree, which cannot see through WSL or SSH.
- Display blocks now accept physics-style implicit products such as `E = mc^2`; inline `$…$` keeps the stricter shape checks so shell text like `$foo^bar$` stays literal.
- The first rendered display formula in a pane now unlocks inline `$…$` rendering there, so remote AI sessions get inline math without local process detection.

#### Improved

- Reworked the SSH host editor into a compact content-sized dialog with consistent 32 px controls, helper text below the destination, a segmented authentication selector, deliberate label/control and section spacing, a lightweight private-key section, visibly blinking text carets, and clearly separated Test / Cancel / Save actions.
- Reworked the Shell / Profile picker into cards: content-sized height with a maximum ten-row viewport, a "Recommended" section carrying a taller hero card (name above, full program path below, Enter chip on the right) and an "All options" section, neutral initial selection, soft accent selection, a bordered default-shell badge, and a keyboard-hint footer. Paths keep their drive/root context and ellipsize at the tail. Hover and click hit-test against the layout's per-row rectangles, so section captions and the gaps between rows no longer highlight as if they were rows.
- Picker rows are no longer drawn as bordered cards. A row is transparent until you hover or select it, because the panel-to-row lightness difference already separates them — five ringed white rectangles in a column read as a form, not as a list. Selection uses a neutral wash rather than the accent: the accent budget is spent once per screen, on the current row.
- All option paths now start at one shared column, so they line up in a single vertical edge. Right-aligning them made every row's path *start* float with its own length (`cmd.exe` began nearly 300 px right of Nushell's), so scanning the list meant hunting left and right for each line.
- One keycap recipe everywhere — every place that shows a key combination (the picker's Ctrl+K badge, the command list's shortcut hints, Settings → Key bindings, confirm dialogs) draws the same chip: hairline ring, panel fill, chip radius. A combination is **one** chip carrying the whole string, matching the familiar Windows convention; the per-key split it replaces is a macOS form, where `⌘ ⇧ ⌥` are single-character glyphs that tile evenly — `Ctrl` `Shift` `Alt` do not.
- Chip backgrounds now mean "you can click this". The picker footer's ↑↓ / Enter / Esc hints are keyboard legends, not buttons, so they dropped their chips and separate the key from its description by ink weight instead.
- Text carets are visibly alive. The blink phase now starts from your last edit instead of running off the wall clock, so a caret is lit the instant a field takes focus (previously it had a 50 % chance of appearing up to half a second late, which read as "this field isn't focused"). The caret holds steady while you type and only breathes once you stop, and the cadence follows the system's `GetCaretBlinkTime`, including the accessibility setting that turns blinking off. The command palette was also missing from the fast-redraw list, so its caret froze once the open animation ended.
- The command palette's caret is a 1.5 px beam instead of a full-cell glyph, which lets the placeholder and your actual query start at the same x — typing the first character no longer shifts the line sideways.
- Confirm dialogs and the SSH host editor now cast a real drop shadow and share the app's corner radius. Both previously drew only a hairline and a fill, so a dialog that demands an answer floated lower than the command palette you can dismiss with Esc. The SSH editor's dim was also a hard-coded 67 % black that ignored the theme, veiling light themes more heavily than dark ones.
- Dropdown lists are fully opaque and cast a shadow instead of a glow. A glow spreads brightness outward without establishing height, which on light themes just grayed the area around the list.
- Light-theme overlays are white-based: the command palette, the SSH host editor and the settings surfaces no longer inherit a silver/slate fill from the theme family.
- The palette's right edge now lines up. The Ctrl+K badge, the hero card's Enter chip, the command list's shortcut chips and the footer's Esc hint previously measured from four different insets — the footer even measured from the panel instead of the card column — leaving a ragged right margin that drifted with font size.
- Aligned Settings with the new navigation-and-row system: equal-height hairline groups, consistent right-aligned controls, restrained section headings, theme previews, and editable keycaps share one layout and interaction model.

### 简体中文

#### 新增

- **Shell 选择器快捷键（Ctrl+K）** — 打开与 "+" 旁 chevron 相同的 shell/profile 列表，再按一次收起。该键位在“设置 → 按键映射”里列出、可改绑；默认占用了 shell 行编辑的 kill-line（Ctrl+K → `\x0b`）。
- **SSH 连接测试** — SSH 主机编辑器现在可以在保存前测试未保存的地址、密码、认证方式和私钥，带 12 秒超时，并在页脚显示连接中、成功耗时或失败状态。
- **快速终端快捷键可配置** — “设置 → 按键映射”新增快速终端全局快捷键行。捕获的新组合立即应用并持久化；若操作系统因冲突拒绝注册，Nebula 会保留原先可用的快捷键，并在该行显示注册失败。
- **工作区导出/导入** — 命令面板新增"导出工作区…"和"打开工作区…",标签页右键菜单新增"导出为工作区…"。工作区文件(`.nebula-workspace.json`)记录标签页列表、每个标签页的完整分屏布局(方向、比例、每个 pane 的工作目录)、标签名与颜色,以及每个标签页的启动身份:WSL/自定义 shell 按原 shell 重开,SSH 标签页自动重连保存的目标。文件跨机器、跨平台可移植——导入机器上不存在的目录回落默认目录;缺失的 shell 程序(如 Linux 上的 `wsl.exe`)回落为默认 shell 并保留目录,不会丢掉整个标签页。
- **崩溃恢复现在还原分屏布局** — 持续写入的会话快照(每秒)与工作区文件共用同一格式,崩溃或强杀后重开时,每个标签页的分屏树、比例、各 pane 目录以及 SSH/WSL 启动身份都会还原,不再是以前的"每个标签页只剩一个 pane"。
- **识别运行中的 Grok** — 跑 `grok` 或 `grok-cli` 的标签页会在图标位显示 xAI 官方标记,和现有的 `claude`、`codex` 一样。浅色/深色两版按 chrome 墨色自动选,并且是按图标槽的**物理像素尺寸**重采样、再按墨迹重心居中的,因此在任何 DPI 下都不发虚。(感谢 @Sakyvo。)
- **标签展开动效可配置** — 设置 → 交互新增"标签展开"选项。`滑动`(默认,与原行为一致)让标签页在变可见时缓动到位,`立即`则直接落位。两种模式下拖拽换序的弹簧手感都不变。(感谢 @Sakyvo。)

#### 修复

- Nebula 现在始终以标准尺寸启动——配置的 `window.dimensions`,或默认的 116×30 网格,且**按配置基准字号计算**。此前有两处回归会把首窗口撑到接近全屏:启动会重放会话文件里保存的窗口尺寸(包括过期或单位错误的值);新增的字号持久化又把 Ctrl+滚轮缩放喂进了启动尺寸公式,116 列放大后的 cell 本身就是一个全屏宽度。启动现在对两者都免疫;持久化的缩放字号启动后照常渲染,只是在标准尺寸的窗口里显示更少的列数。
- 修复界面文字随 Ctrl+滚轮一起缩放、以及终端字号离开基准后出现的发丝级脏线。根因是架构性的:chrome 与终端共用同一套字体系统,而侧栏标题、tab/主机标签、设置页大标题、SSH 编辑浮层、消息队列条目等多处 chrome 文字走的是**设计上就跟随终端缩放**的文档文字路径。现在全部 chrome 排版统一走专用的 UI 锚定文字路径(按 UI 基准字号栅格化、独立的基线数学),布局按基准字号真实栅格化的 cell 步进,点击目标与绘制读取同一布局,恢复持久化缩放的第一帧锚定即正确。
- 终端字号缩放现在有硬边界(逻辑 4–64 px)。此前卡住的修饰键或触控板会把字号滚过 180px,那里每档滚轮变化不足 1%,缩回去的手感如同失灵。
- 修复调整窗口大小的 HUD("80 × 24")在侧栏打开时文字画到居中框外:框按窗口像素居中,文字却按终端网格居中,而网格原点带着侧栏的非对称边距。两者现在共用同一坐标系,HUD 也不再随它所汇报的终端缩放一起变大。
- 修复目录树不跟随标签页切换:打开过的 SFTP 面板不再永久霸占抽屉——视图按聚焦 pane 路由(切回本地标签页恢复目录树,SFTP 连接保持热连接),切到远程 pane 不再把树清空;`wsl -d <发行版>` 启动的 WSL 标签页现在通过 `\\wsl$` 跟随 shell 目录。
- 修复 WSL 与远程 SSH 会话中的公式渲染:隔着 `wsl.exe` / `ssh.exe` 运行的 claude、codex、pi 等工具输出的 `$$ … $$`、`\[ … \]` 块级公式现在原生渲染——块级检测不再依赖本机进程树里能否找到 AI CLI(进程探测无法穿透 WSL 和 SSH)。
- 块级公式接受 `E = mc^2` 这类隐式乘积;行内 `$…$` 保持更严格的形状检查,`$foo^bar$` 之类的 shell 文本仍按字面显示。
- pane 内首个渲染成功的块级公式会解锁该 pane 的行内 `$…$` 渲染,远程 AI 会话无需本机进程探测即可获得行内公式。

#### 改进

- SSH 主机编辑器重排为紧凑的内容自适应弹窗：控件统一为 32px 高，地址提示移到输入框下方，认证方式改为分段选择，标签/控件与分组间距遵循统一节奏，私钥区降级为轻量小节，文本光标真实闪烁，并清晰分隔“测试连接 / 取消 / 保存”。
- Shell / Profile 选择器改为卡片式：按条目数量决定高度，最多显示十行；分为“推荐”区（一张更高的大卡片，名称在上、完整程序路径在下、右侧回车键帽）和“所有选项”区；初次打开保持中性，选中态使用柔和 accent，保留默认 Shell 发丝描边徽标和键盘提示页脚。路径保留盘符/根路径上下文、尾部省略。悬停与点击改为按布局的逐行矩形命中，分区标题和行之间的缝隙不再被当成行高亮。
- 选择器的行不再画成带框卡片。默认完全透明，只有悬停/选中才上底色——面板与行之间的明度差已经足够把它们分开，而五个带圈的白矩形排成一列读起来是表单，不是列表。选中态改用中性底色而不是 accent：强调色预算一屏只花一次，就花在“当前选中”上。
- 所有选项的路径改为从同一列起画，左缘对齐成一条竖线。此前是右对齐，每行路径的**起点**随它自身长度浮动（`cmd.exe` 的起点比 Nushell 靠右近 300px），眼睛沿列表往下扫时要不停地左右找落点。
- 键帽样式全局统一：所有展示键位的地方（选择器的 Ctrl+K 徽标、命令列表的快捷键提示、设置 → 按键映射、确认弹窗）画同一种 chip——发丝圈边、面板填充、统一圆角。一个组合键是**一颗**承载整串的 chip，符合 Windows 用户熟悉的输入习惯；被它替换掉的逐键拆分是 macOS 的形式，那里 `⌘ ⇧ ⌥` 是单字符图形、排起来宽度整齐，而 `Ctrl` `Shift` `Alt` 不是。
- chip 底色现在只表示“这里可以点”。选择器页脚的 ↑↓ / Enter / Esc 是键位说明而非按钮，因此去掉 chip 底，改用墨色深浅区分键名与释义。
- 文本光标有了活动感。闪烁相位改为从**最后一次编辑**起算，而不是跟着系统挂钟走：输入框一获得焦点光标立刻是亮的（此前有一半概率要等最多半秒才出现，读起来像“这个框没聚焦”）。打字期间光标保持常亮，停手后才开始呼吸，节律取自系统的 `GetCaretBlinkTime`，并尊重“关闭闪烁”这项无障碍设置。命令面板此前还漏在快速重绘名单外，入场动画一结束它的光标就不再翻转。
- 命令面板的光标改为 1.5px 细梁而不是占满一格的字形，于是提示文字与真实输入从同一个 x 起画——打下第一个字符时整行不再横向跳动。
- 确认弹窗与 SSH 主机编辑器现在有真正的外阴影，圆角也并入统一阶梯。此前两者只画发丝描边加填充，于是一个要求用户作答的弹窗，浮起高度还不如按 Esc 就能关掉的命令面板。SSH 编辑器的遮罩此前是写死的 67% 纯黑、不读主题，浅色主题下比深色主题罩得还重。
- 下拉列表改为完全不透明，并用阴影而不是辉光。辉光只是向外扩散亮度、不建立高度关系，在浅色主题下只会让列表四周发灰。
- 浅色主题的浮层统一白色打底：命令面板、SSH 主机编辑器与设置页表面不再从主题族继承银/岩灰底色。
- 命令面板右缘现在对齐成一条线。此前 Ctrl+K 徽标、大卡片的回车键帽、命令列表的快捷键 chip 与页脚的 Esc 提示各用一套内边距——页脚甚至是按面板而非卡片列计算的——右边缘参差，且会随字号漂移。
- 设置页统一到新的导航与等高行体系：hairline 分组、右对齐控件、克制的分节标题、主题预览和可编辑键帽共用一致的布局与交互模型。

## 0.7.0 - 2026-07-24

### English

#### Added

- **Background image** — Appearance settings can now set a wallpaper for the terminal, with stretch mode, position, and image opacity. The wallpaper stays inside the terminal area by default, and can optionally extend across the whole window.
- **Startup directory** — new terminal tabs can open in a directory of your choice, picked with the system folder dialog. The choice is remembered and can be cleared at any time.
- **Window size memory** — Nebula reopens with the same window size and maximized state as last time, and the size stays consistent across screens with different scaling.
- **Live appearance preview** — the Appearance page now starts with a preview card that immediately shows color, font, font size, and cursor changes as you make them.
- **Dropdown option lists** — every multi-choice setting (default shell, terminal font, wallpaper stretch and position, interface language, completion accept key, cursor shape) now opens a dropdown showing all options, instead of cycling through values on click.
- **Font size controls** — the terminal font size can be changed with a numeric control in settings or Ctrl + mouse wheel in the terminal, and is remembered. Interface text keeps its own size and is not affected by zooming.
- **Background color picker** — the background color row now opens a picker with preset colors and a hex color input, instead of cycling colors on click.
- **Cursor shape and blinking** — the default cursor shape (bar, underscore, filled box, hollow box) is selectable and cursor blinking is on by default. Programs like vim can still change the cursor themselves.
- **Copy on select** — a new Interaction section adds copy-on-select, on by default. With it off, right-click still copies the selection, or pastes when nothing is selected.
- **SSH keepalive** — remote sessions that sit idle for a long time no longer get disconnected by routers or firewalls.
- **Update check** — Nebula now checks GitHub Releases shortly after startup and shows a dismissible in-app banner when a newer version is available. It is a single anonymous version query; nothing else is sent.

#### Fixed

- Fixed wallpaper fading: lowering the image opacity now fades the picture into the theme background — lighter in light mode, darker in dark mode — instead of letting the desktop behind the window shine through as a harsh white.
- Fixed wallpaper formats: PNG, JPG, WebP, and BMP files chosen in the picker all display correctly now.
- Fixed the wallpaper covering the terminal's rounded corners, and made the appearance preview card show the actual wallpaper with its real stretch, position, and opacity.
- Fixed the opacity controls: both sliders now drag smoothly with live preview, and save when released.
- Fixed uneven window transparency: at any opacity the frame around the terminal now looks like one even surface, without patches that appear more solid than their neighbors.
- Fixed the maximize button not switching to the restore icon when the window is maximized, and hover highlights on the window buttons not reaching the top edge of the screen.
- Fixed background text showing through the command palette.
- Fixed math rendering while typing: formulas typed into Claude Code, Codex, and similar tools stay as editable text, and only finished output is displayed as math.
- Fixed rendered formulas reverting to raw source while scrolling, and long formulas losing their beginning after it scrolled out of view.
- Fixed matrices and multi-line formulas showing leftover text such as `&nbsp;`.
- Fixed the terminal and input boxes keeping colors from the previous theme after switching between light and dark.
- Fixed tab titles not following directory changes; tabs you renamed yourself keep their names.
- Fixed custom fonts causing hollow boxes and runs of broken symbols; icons and missing characters now fall back to the built-in font automatically.
- Fixed SSH warnings that could not be closed and spilled over the sidebar.
- Fixed the title-bar buttons: minimize, maximize, and close now form a seamless Windows-style group flush with the window edge, the close button shows the familiar red hover, and it stays clickable in the very corner when maximized.
- Fixed the sidebar "+" button and the three-dot menu changing size depending on window state.
- Fixed the working spinner pausing and jumping between rotations; it now spins smoothly.
- Fixed the opacity sliders showing a resize cursor and a gray hover box; they now look and feel like standard Windows sliders.
- Fixed icons sitting visibly off-center inside their hover highlights.
- Fixed command palette rows not highlighting under the mouse.
- Fixed the Ctrl+click link tooltip jumping around with the pointer and growing too long; it now stays in place and shortens long paths.
- Fixed cursor shape and blinking changes not taking effect until a restart.
- Fixed confirmation dialogs stretching into wide banners; long messages now wrap and the buttons are a simple Yes / No pair.
- Fixed harmless clipboard warnings that appeared when another program briefly held the clipboard.
- Fixed the cursor style chosen in Settings never applying when the shell had already touched cursor blinking (ConPTY does this on startup, so on Windows the setting effectively never worked): the choice now takes effect immediately, in every open tab and in new tabs.
- Fixed rendered formulas sitting on an opaque black (in light mode: white) slab that blocked the wallpaper and window transparency; formulas now draw directly over the real background.
- Fixed formula sizes depending on how many terminal rows the source text happened to occupy — a one-line `$$...$$` was squeezed tiny while multi-line sources rendered large. Formulas now borrow breathing room from surrounding blank lines and render at one consistent size, and tall inline fractions (`\dfrac`) become readable the same way.
- Fixed formulas flashing back to raw TeX while typing — including during Chinese IME composition — and sometimes never re-rendering after a TUI repaint; a formula whose closing `$$` is still visible now restores itself from scrollback.
- Fixed short inline formulas like `$\xi$` being centered inside the width of their source text, leaving them stranded between large gaps; narrow results now sit next to the preceding words.
- Fixed the background-color value in Settings overlapping the dropdown arrow.

#### Improved

- Improved long terminal sessions with many formulas: scrolling stays fast and memory use stays stable.
- Improved scrolling smoothness when a custom font is selected.
- Improved battery and CPU usage: animations only redraw while something is actually moving, and idle windows do almost no work.
- Improved consistency of settings controls: sliders, switches, dropdowns, and steppers now share one implementation, so they look and behave the same everywhere.

### 简体中文

#### 新增

- **背景图** — 外观设置现在可以为终端设置壁纸，支持拉伸方式、位置和图片不透明度调节。壁纸默认只显示在终端区域内，也可以选择铺满整个窗口。
- **启动目录** — 新建终端标签页可以在你指定的目录中打开，目录通过系统文件夹对话框选择，选择会被记住，也可以随时清除。
- **窗口大小记忆** — Nebula 会以上次关闭时的窗口大小和最大化状态重新打开，在不同缩放比例的屏幕之间大小也保持一致。
- **外观实时预览** — 外观页顶部新增预览卡片，颜色、字体、字号和光标的改动立刻能在预览中看到。
- **下拉选项列表** — 所有多选项设置（默认 Shell、终端字体、壁纸拉伸方式与位置、界面语言、补全接受键、光标形状）都改为打开下拉列表直接选择，不再是点一下换一个。
- **字号调节** — 终端字号可以在设置中用数字控件调整，也可以在终端里按住 Ctrl 滚动鼠标滚轮缩放，并会被记住。界面文字保持自己的大小，不受缩放影响。
- **背景色选择器** — 背景色一行改为打开选择器，提供预设颜色和十六进制色值输入，不再点击循环切换颜色。
- **光标形状与闪烁** — 默认光标形状可选（竖线、下划线、实心方块、空心方块），光标闪烁默认开启；vim 等程序仍然可以自己改变光标。
- **选中即复制** — 新增"交互"设置，提供选中即复制开关，默认开启；关闭后右键仍可复制选中内容，没有选中时则执行粘贴。
- **SSH 保活** — 长时间没有操作的远程会话不会再被路由器或防火墙断开。
- **检查更新** — Nebula 启动后会静默检查 GitHub Releases，有新版本时在应用内显示一条可关闭的横幅提示。整个过程只有一次匿名的版本查询，不上传任何数据。

#### 修复

- 修复壁纸变淡的方向：降低图片不透明度时，画面会淡入主题背景色——浅色模式越来越浅、深色模式越来越深，不再透出窗口后面的桌面形成刺眼的白色。
- 修复壁纸格式：选择器允许的 PNG、JPG、WebP 和 BMP 文件现在都能正常显示。
- 修复壁纸盖住终端圆角的问题，并让外观预览卡按真实的拉伸、位置和不透明度显示当前壁纸。
- 修复不透明度控件：两个滑块都可以流畅拖动、实时预览，松手后保存。
- 修复窗口透明度不均：任何透明度下，终端四周的边框看起来都是均匀的一整块，不再出现某一块比旁边更实的拼接感。
- 修复窗口最大化后按钮没有换成还原图标，以及窗口按钮的悬停高亮没有延伸到屏幕顶边的问题。
- 修复命令面板背后的文字透进面板形成重影的问题。
- 修复输入时的公式渲染：在 Claude Code、Codex 等工具里输入的公式保持为可编辑文字，只有已经输出完成的内容才会显示为数学公式。
- 修复滚动时已渲染的公式变回原始文字，以及长公式开头滚出屏幕后无法完整显示的问题。
- 修复矩阵和多行公式中出现 `&nbsp;` 之类残留文字的问题。
- 修复切换明暗主题后，终端和输入框残留上一个主题颜色的问题。
- 修复标签页标题不跟随目录变化的问题；你手动重命名过的标签页仍保持自定义名称。
- 修复自定义字体导致空心方框和成片乱码的问题；图标和缺失的字符会自动使用内置字体显示。
- 修复 SSH 警告无法关闭、背景延伸到侧边栏的问题。
- 修复标题栏按钮：最小化、最大化和关闭现在是连续无缝的 Windows 风格按钮组并完全贴齐窗口边缘，关闭按钮悬停显示熟悉的红色，最大化时屏幕最角落也能点到。
- 修复侧栏"+"按钮和三点菜单随窗口状态忽大忽小的问题。
- 修复工作状态转圈动画停顿、跳动的问题，现在旋转是连续平滑的。
- 修复不透明度滑块显示双向箭头光标和灰色悬停底块的问题，现在的外观和手感与标准 Windows 滑块一致。
- 修复图标在悬停高亮中明显偏离中心的问题。
- 修复命令面板候选行不跟随鼠标高亮的问题。
- 修复 Ctrl+点击 链接提示跟着指针乱跳、内容过长的问题，提示现在固定显示并会缩短过长的路径。
- 修复光标形状和闪烁设置需要重启才生效的问题。
- 修复确认对话框被拉成横幅的问题：长文字自动换行，按钮就是简单的"是 / 否"。
- 修复其他程序短暂占用剪贴板时弹出无意义警告的问题。
- 修复设置里选择的光标样式始终不生效的问题（Windows 上 shell 启动时会设置光标闪烁，旧逻辑会连形状一起"钉死"）：现在选择立即生效，对所有已打开和新建的标签页都有效。
- 修复公式渲染带一块不透明底色（深色模式黑块、浅色模式白块）、挡住壁纸和窗口透明效果的问题；公式现在直接绘制在真实背景上。
- 修复公式大小取决于源码占了几行终端的问题——单行 `$$...$$` 被压得极小、多行的又显得很大。公式现在会向上下空行借用空间，以统一大小渲染；行内的高分数（`\dfrac`）也因此变得可读。
- 修复输入时（包括中文输入法组词过程中）公式闪回原始文字、TUI 重绘后偶尔再也不渲染的问题；闭合 `$$` 仍在屏幕上的公式现在能从回滚缓冲区自动恢复。
- 修复 `$\xi$` 这类短公式在源码宽度内居中、两侧留出大片空隙的问题；渲染结果较窄时现在紧贴前面的文字。
- 修复设置中背景色的当前值文字与下拉箭头重叠的问题。

#### 改进

- 改进含大量公式的长时间终端会话：滚动保持流畅，内存占用保持稳定。
- 改进选择自定义字体后的滚动流畅度。
- 改进耗电和 CPU 占用：动画只在真正有东西变化时刷新，空闲窗口几乎不做工作。
- 改进设置控件的一致性：滑块、开关、下拉框和步进器共用同一套实现，各处外观和行为完全一致。
## 0.6.0 - 2026-07-19

### English

#### Added

- **Native math formula support** — Nebula can now display inline $...$ and display $$...$$ formulas directly in Markdown. Fractions, roots, scripts, limits, matrices, scalable brackets, Greek letters, common operators, and Unicode text are supported. Formulas are rendered locally in Rust with the bundled math font, without a web component, formula images, or an external TeX program.
- **Math formula test document** — the README now includes a verified screenshot, and `docs/math-rendering-test.md` provides a reusable test page covering common symbols, complex formulas, long formulas, blank rows, Unicode text, and dollar-fence boundaries.
- **Windows installer support** — Nebula now provides a guided per-user installer with English and Simplified Chinese interfaces, optional font installation, desktop and startup shortcuts, and structured cleanup during uninstall.
- **File-drawer directory actions** — the Files drawer can move to its parent directory, open a new terminal at the displayed directory, and drag a file or folder into the terminal to insert its safely quoted full path without executing it.
- **Frequent-directory workflows** — Nebula remembers directories that the shell actually entered. Frequently used locations are promoted in path completion and inline suggestions, and the command palette can open a new terminal directly in a visited directory.

#### Fixed

- **Markdown wrapping and formula containment fix** — paragraphs, Chinese text, long unbroken content, failed formula source, and oversized formulas now remain inside the reading column instead of overflowing or being cut off.
- **Multiline math and recognition boundary fix** — display formulas can contain blank rows and Unicode explanations, while only paired $...$ and $$...$$ ranges are treated as math. Bare TeX and quoted code remain ordinary Markdown.
- **Formula geometry and clarity fix** — arrows are converted only inside formulas, radical bars connect cleanly to the root symbol, Markdown headings use consistent weight, and small mathematical symbols have clearer edges.
- **Split-pane paste routing fix** — right-click paste and Ctrl+V always send text to the pane where the paste started, even after the pointer or focus moves across a split.
- **Enter penetration fix** — while multiline-paste confirmation is visible, Enter handles that confirmation only and never reaches the terminal behind it or a neighboring split. Approved text remains bound to the pane that opened the confirmation.
- **Split Markdown input penetration fix** — keyboard, pointer, and scrolling input used by a Markdown document no longer reaches a neighboring or background terminal in a split window.
- **Numpad Enter routing fix** — the numeric keypad Enter key now submits commands in the same way as the main Enter key instead of triggering paste behavior.
- **SFTP split-session routing fix** — opening the file panel from a split SSH terminal now uses that pane's authenticated destination, so the panel does not connect to a different host after titles, commands, or focus change.
- **Shell prompt lifecycle fix** — Nebula now preserves existing PowerShell and Bash prompt hooks, command exit status, pipeline status, and prompt behavior while still reporting directory changes and command completion.
- **Default-shell picker fix** — confirming a shell in the default-shell picker now saves it as the default instead of opening it as a new terminal.
- **System appearance following fix** — enabling automatic appearance now reads the operating system theme directly instead of reusing a stale manual window theme, so switching from a light theme follows an already-dark system immediately and continues tracking later changes.
- **AI integration removal fix** — removing integrations now continues through every supported tool even when one user configuration is damaged, avoiding stale hooks that point to an uninstalled Nebula executable.

#### Improved

- **Large Markdown math document improvements** — Markdown files containing many formulas load quickly and remain responsive while scrolling. Nebula processes the visible area, reuses repeated formulas, and limits unusually complex input so memory use stays stable during long reading sessions.
- **SFTP workflow improvements** — the SFTP panel now supports parent-directory navigation, drag-and-drop upload, and a background context menu for refresh, uploading files or folders, and creating a directory. Multi-file drops are grouped into one transfer instead of cancelling one another.

### 简体中文

#### 新增

- **Markdown 数学公式支持** — Nebula 现在能够直接显示行内 $...$ 和块级 $$...$$ 公式，支持分数、根式、上下标、极限、矩阵、伸缩括号、希腊字母、常用运算符和 Unicode 文字。公式由 Rust 和内置数学字体在本地完成显示，不需要网页组件、公式图片或外部 TeX 程序。
- **数学公式测试文档** — README 已加入经过验证的效果截图，`docs/math-rendering-test.md` 提供可重复使用的测试页面，覆盖常用符号、复杂公式、长公式、空行、Unicode 文字和美元围栏边界。
- **Windows 安装程序支持** — Nebula 现在提供中英文安装向导，支持按当前用户安装、可选字体安装、桌面与开机启动快捷方式，并在卸载时完成应用配置清理。
- **文件目录快捷操作支持** — 文件抽屉可以返回上级目录、在当前显示目录中新建终端，也可以把文件或目录拖入终端，插入经过安全引用的完整路径而不会自动执行。
- **常用目录支持** — Nebula 会记录 Shell 实际进入过的目录，让常用位置优先出现在路径补全和行内建议中；也可以从命令面板直接在访问过的目录中新建终端。

#### 修复

- **Markdown 换行与公式越界修复** — 普通段落、中文、连续长文本、解析失败的公式源码和过宽公式都会留在阅读列内，不再越界或从右侧被裁掉。
- **多行公式与识别边界修复** — 块级公式可以包含空行和 Unicode 说明文字，同时只有成对的 $...$ 和 $$...$$ 才会识别为数学公式；裸露 TeX 和引用代码仍按普通 Markdown 显示。
- **公式几何与清晰度修复** — 箭头只在公式内部转换，根号横线能够与根号主体完整连接，Markdown 标题字重保持统一，小字号数学符号的边缘也更加清楚。
- **分屏粘贴路由修复** — 右键粘贴和 Ctrl+V 始终把内容发送到发起粘贴的分屏，即使鼠标或焦点随后移动到其他分屏也不会改错目标。
- **Enter 穿透修复** — 多行粘贴确认框显示时，Enter 只处理当前确认，不会再发送到后方终端或相邻分屏；确认后的内容仍然只进入发起粘贴的分屏。
- **分屏 Markdown 输入穿透修复** — 在分屏窗口中查看 Markdown 文档时，文档使用的键盘、鼠标和滚动操作不再发送到相邻或后方终端。
- **数字小键盘 Enter 修复** — 小键盘最右侧 Enter 现在与主 Enter 一样提交命令，不再触发粘贴行为。
- **SFTP 分屏连接修复** — 从 SSH 分屏打开文件面板时，会使用该分屏已经认证的连接目标，不再因标题、命令或焦点变化连接到其他主机。
- **Shell 提示符生命周期修复** — Nebula 在报告目录变化和命令完成状态时，会保留已有的 PowerShell、Bash 提示符 Hook、命令退出状态和管道状态，不再破坏用户原有提示符工具。
- **默认 Shell 选择修复** — 在默认 Shell 选择器中确认后会正确保存设置，不再把所选 Shell 当成新终端直接打开。
- **系统明暗模式跟随修复** — 开启自动跟随后会直接读取操作系统主题，不再沿用窗口中残留的手动浅色状态；即使系统已经处于深色，也能立即切换并继续响应之后的明暗变化。
- **AI 集成移除修复** — 移除集成时，即使某一项用户配置损坏，Nebula 也会继续清理其他工具，避免残留指向已卸载程序的 Hook。

#### 改进

- **大型 Markdown 数学文档加载改进** — 包含大量公式的 Markdown 文档能够快速打开并保持流畅滚动。Nebula 只处理当前可见区域、复用重复公式，并限制异常复杂的输入，让长时间阅读时的内存占用保持稳定。
- **SFTP 操作改进** — SFTP 面板新增返回上级目录、拖放上传，以及包含刷新、上传文件、上传目录和新建目录的空白区域右键菜单；一次拖入多个文件时会合并为同一批传输，不会互相取消。

## 0.4.0 - 2026-07-14

### Terminal Rendering And Interaction / 终端渲染与交互

- **No more missing rows at the bottom** — the terminal now makes proper room for both the top bar and the bottom edge. The last prompt, cursor, selection, and full-screen terminal content stay inside the visible card, including in split views.
  **中文：** 终端底部不再凭空少一截啦。顶部栏和底部边距现在各占各的空间，即使使用分屏，最后一行命令、光标、选区和全屏程序内容也都能完整显示在卡片内。
- **Selection stays clean in transparent windows** — selected text no longer shows ghosted content from apps behind Nebula or leaves visual residue behind.
  **中文：** 透明窗口里的选区不再透出后方应用，也不会留下残影，选中文字时看起来更干净。
- **Softer cursor and selection colors** — the cursor and selection now follow the current theme with lower-saturation colors, so they feel more balanced and are no longer harsh on the eyes. A color chosen by the user still takes priority.
  **中文：** 光标和选区现在会跟随主题色啦，并使用低饱和度的颜色，看起来更协调、不再刺眼；如果用户自己设置了光标颜色，仍会优先使用用户的选择。
- **Links are easier to recognize** — clickable file paths and terminal links now keep a dashed underline. The underline follows the original text color, so folders, executables, and multicolored filenames remain easy to tell apart.
  **中文：** 可点击的文件路径和终端链接现在会一直带有虚线下划线，而且下划线会跟随文字原本的颜色，目录、可执行文件和彩色文件名依然一眼就能分清。
- **Mouse selection feels like other desktop apps** — double- and triple-click selection now follows the system's timing and movement rules, while `Shift`+click extends the current selection. A normal click will no longer unexpectedly select a whole word or line.
  **中文：** 鼠标选中文字现在更符合系统习惯：双击、三击会遵循系统的速度和移动范围，`Shift`+点击可以继续扩展选区，普通单击也不会再莫名选中整词或整行。
- **Multiline shortcuts no longer look like pasted text** — key bindings that send an `Esc`-prefixed sequence, such as `Shift`+`Enter` for multiline input in Claude Code, now go straight to the terminal instead of opening the multiline paste confirmation.
  **中文：** `Shift`+`Enter` 这类发送 `Esc` 组合序列的多行输入快捷键，现在会直接交给终端，不会再被误认为粘贴内容并弹出多行粘贴确认。
- **Text sizing looks normal again** — headings and copy in the sidebar, SSH view, and document view no longer appear stretched, crowded, or stuck together when display scaling is enabled.
  **中文：** 开启系统缩放后，侧栏、SSH 页面和文档里的标题与说明文字不再被异常放大，也不会显得拉长、拥挤或粘在一起。

### SSH Safety And Feedback / SSH 安全与反馈

- **Right-click menus for SSH hosts and tabs** — SSH hosts can be connected, copied, edited, or removed from a right-click menu. Tabs can be duplicated, split, renamed, closed, or given a custom color. The menu closes naturally when clicking elsewhere, pressing `Esc`, typing, or switching away from the window.
  **中文：** SSH 主机和标签页都补上了顺手的右键菜单。SSH 主机可以连接、复制地址、编辑或删除；标签页可以复制、左右/上下分屏、重命名、关闭或设置颜色。点击其他地方、按 `Esc`、继续输入或切走窗口时，菜单都会自然收起。
- **Deleted hosts can be recovered** — removing a host now asks for confirmation and provides an eight-second Undo button plus `Ctrl+Z`. Hosts read from `~/.ssh/config` are only hidden in Nebula, never deleted from that file, and hidden hosts can be brought back from Settings. Saved order is restored on Undo, and credentials are not erased until the Undo period ends.
  **中文：** 删除 SSH 主机前现在会先确认，删除后还有 8 秒撤销时间，也可以直接按 `Ctrl+Z`。从 `~/.ssh/config` 读取的主机只会在 Nebula 里隐藏，不会改动原文件；之后也能从设置页的隐藏主机入口找回来。撤销时会恢复原来的顺序，保存的密码也会等撤销时间结束后再清理。
- **SSH errors are shown where you can see them** — an invalid address keeps the text you entered, returns focus to the address box, and explains what needs fixing. If a terminal pane cannot be created, Nebula now shows the host, the reason, and what to try next instead of leaving the details only in the log.
  **中文：** SSH 地址填错时不会再悄悄失败：已经输入的内容会保留，光标会回到地址框，并直接告诉你哪里需要修改。终端面板创建失败时，界面也会显示目标主机、失败原因和下一步建议，不用再去日志里猜。
- **SSH fields now use familiar editing shortcuts** — address and password boxes support `Ctrl+A`, `Ctrl+C`, `Ctrl+V`, replacing selected text, Chinese IME input, and visible selection. Hidden passwords can be selected and pasted, but can only be copied after being revealed.
  **中文：** SSH 地址和密码框现在可以正常使用 `Ctrl+A`、`Ctrl+C`、`Ctrl+V`，也支持中文输入法、全选后直接替换和清晰的选中效果。隐藏状态下的密码可以选择和粘贴，但只有点开显示后才能复制。

### UI Hierarchy And Control Consistency / UI 层级与控件一致性

- **A more consistent interface** — spacing now follows a 4px rhythm, while type sizes, row heights, icon buttons, corners, borders, shadows, animations, and control states share the same visual rules across the app.
  **中文：** 界面的间距现在统一按 4px 节奏排布，字号、行高、图标按钮、圆角、描边、阴影、动画和各种操作状态也都使用同一套视觉规则，页面之间看起来更整齐、更一致。
- **Themes can follow the system** — Appearance now includes “Follow system light/dark mode”. Nebula switches between the matching light and dark themes while preserving the selected theme family. Choosing a theme card manually turns automatic switching off, so an explicit choice is never overwritten.
  **中文：** 新增跟随系统明暗模式。在“外观”里开启后，Nebula 会切换到同系列的浅色或深色主题，同时保留用户选择的主题系列；手动点选主题卡会退出自动跟随，不会覆盖用户明确选择的主题。
- **Text boxes behave the same everywhere** — renaming tabs, filtering files, entering Git commit messages, editing SSH hosts, and searching commands now all support the same copy, paste, select-all, replacement, IME, and selection behavior.
  **中文：** 各处输入框终于用起来一致了：无论是重命名标签页、筛选文件、填写 Git 提交信息、编辑 SSH 主机还是搜索命令，都能用同样的复制、粘贴、全选、替换和中文输入法操作。
- **A calmer sidebar** — `TABS` and `SSH HOSTS` now have clearer heading sizes, weights, and shades. The two `+` buttons only appear when the pointer is over their section title, the tab menu uses a vertical three-dot icon, and the empty SSH message is easier to read.
  **中文：** 侧栏现在更清爽了：`TABS` 和 `SSH HOSTS` 的字号、字重与灰度层级更清楚；两个 `+` 只会在鼠标移到对应标题时出现，标签页菜单改成竖向三点，SSH 为空时的提示也更容易看清。
- **Tab colors are now optional** — tabs no longer show a color strip by default. The strip appears only after you choose a color, and custom tab names and colors are restored with the session.
  **中文：** 标签页默认不再显示色条，只有用户主动设置颜色后才会出现；自定义名称和颜色也会跟随会话保存，下次打开仍然保留。
- **The `+` buttons are properly centered** — the icon, hover background, and clickable area now share the same center, so the button looks and feels aligned. Menu icons are also limited to shapes that the bundled Maple Mono Nerd Font can display reliably.
  **中文：** `+` 图标、悬停背景和实际可点击区域现在共用同一个中心，看起来不会再歪，点起来也更准确；菜单图标也只使用内置 Maple Mono Nerd Font 能稳定显示的字形，避免出现方框或错位。
- **Shell and profile search is back** — the picker can once again search and filter shells or profiles, with Chinese IME and familiar editing shortcuts. Search boxes and results use a compact 38px height, while SSH hints are brighter and easier to read.
  **中文：** Shell 和 Profile 选择器的搜索回来了，支持中文输入法、常用编辑快捷键和模糊筛选。搜索框与结果行统一收紧到 38px，SSH 提示文字也调亮了一些，不再灰得看不清。
- **Right-click menus feel lighter** — menus now use a soft theme-aware shadow, a subtle border, and a short open/close animation. Tab color labels and swatches also have more natural spacing.
  **中文：** 右键菜单加上了跟随主题的柔和阴影、细边框和短促的开合动画，层次更自然；标签页颜色名称和色块之间也留出了更舒服的间距。

### Architecture, Reliability, And Verification / 架构、可靠性与验证

- **Cleaner internal structure** — context menus, text editing, SSH UI state, and shared visual values now live in separate modules, making later changes easier to understand and less likely to affect unrelated parts of the app.
  **中文：** 右键菜单、文本输入、SSH 界面状态和通用视觉配置已经拆到各自的模块里，后续修改更容易看懂，也更不容易误伤其他功能。
- **Product experience verification** — normal, empty, and error states were reviewed together with destructive-action recovery, focus behavior, and font and icon reliability, making common workflows easier to understand and recover.
  **中文：** 完成正常、空白和错误状态的产品体验检查，并覆盖误操作恢复、焦点行为、字体和图标可靠性，让常用流程更容易理解，也更容易从错误中恢复。
- **More regression tests** — new tests cover the terminal bottom edge, split views, link underlines, transparent cursor and selection colors, overlapping links, menu placement, SSH deletion recovery, text editing, theme-family switching, and control-state priority. Current result: **188 passed; 0 failed**.
  **中文：** 新增回归测试，覆盖终端底部显示、分屏、链接下划线、透明窗口中的光标与选区、重叠链接、菜单位置、SSH 删除恢复、文本输入、主题系列切换和操作状态优先级。当前结果：**188 项通过，0 项失败**。

### Still In Progress / 还在继续做

- **Not marked as complete yet** — the full SSH connecting/connected/failed experience, further cleanup of `display/mod.rs`, one shared animation timeline, tab close/reflow animations, and the OpenGL/wgpu direction are still being worked on or evaluated.
  **中文：** 完整的 SSH 连接中/已连接/失败状态、`display/mod.rs` 的进一步拆分、统一动画时间线、标签页关闭与回流动画，以及 OpenGL/wgpu 方案选择都还在继续开发或评估，本次没有把它们算作已经交付。

## 0.3.0 - 2026-07-12

### Highlights / 亮点

- **Complete UI redesign** — the top bar and left sidebar now form a continuous L-shaped chrome shell, with a unified visual language across settings, the command palette, confirmation dialogs, and drawers.
  **中文：** 全面重设计窗口 UI：顶部栏与左侧栏组成连续 L 形 chrome，并统一设置、命令面板、确认框和抽屉的视觉语言。
- **Flexible tab interaction** — tabs support animated reordering, dragging into the active terminal to create a split, edge docking previews, and matching pointer feedback.
  **中文：** 标签页支持动画排序、拖入当前终端形成分屏、边缘停靠预览和对应的鼠标反馈。
- **Files and Git drawer** — adds a right-side directory tree and Git workspace with filtering, expansion, path dragging, file status, commit/push actions, and new full-color file-type icons.
  **中文：** 新增右侧目录树与 Git 工作区，支持筛选、展开、路径拖拽、文件状态、提交/推送操作以及新的彩色文件类型图标。
- **Markdown/GFM viewer** — adds read-only rendering for headings, lists, tables, task lists, code blocks, block quotes, links, and scrollable documents.
  **中文：** 新增 Markdown/GFM 只读查看器，支持标题、列表、表格、任务列表、代码块、引用、链接和滚动浏览。
- **Detected shells with brand icons** — discovers PowerShell, CMD, Git Bash, Nushell, WSL, and common Linux distributions and renders their full-color icons.
  **中文：** 新增 Shell 探测和品牌彩色图标，覆盖 PowerShell、CMD、Git Bash、Nushell、WSL 及常见 Linux 发行版。

### Terminal And Profiles / 终端与配置

- **New-tab shell menu** — the chevron beside `+` launches a detected shell or configured profile directly.
  **中文：** 标签栏 `+` 旁新增 Shell 菜单，可直接使用检测到的执行器或配置 Profile 创建标签页。
- **Inline default-shell picker** — the settings row expands in place, displays every detected shell with its color icon, persists the selected item, and collapses after selection.
  **中文：** 设置页“默认 Shell”改为原地展开列表，显示全部检测到的 Shell 及彩色图标；选择后立即持久化并收起。
- **Rich shell identifiers** — default-shell persistence supports `cmd`, `pwsh`, `nu`, and `wsl:<distribution>` while retaining Nebula prompt bootstrap support for PowerShell and Git Bash.
  **中文：** 默认 Shell 持久化支持 `cmd`、`pwsh`、`nu` 和 `wsl:<distribution>`，同时继续兼容 PowerShell/Git Bash 的 Nebula prompt bootstrap。
- **Appearance controls** — adds runtime window opacity, background image, background-image opacity, and independently scrollable settings sections.
  **中文：** 新增窗口透明度、背景图片、背景图片透明度控制，以及可独立滚动的设置分区。

### SSH

- **Native Rust SSH transport** — saved hosts now connect directly to a remote PTY channel without a wrapper shell, injected command, or external `ssh.exe` console window.
  **中文：** 保存的 SSH host 现在通过 Rust SSH 传输直接连接远端 PTY，不再依赖包装 Shell、命令注入或外部 `ssh.exe` 黑窗口。
- **Complete authentication chain** — resolves aliases, users, ports and identity files from `~/.ssh/config`, then supports private keys, OpenSSH certificates, encrypted-key passphrases, Windows OpenSSH Agent, Pageant, saved or prompted passwords, and keyboard-interactive/MFA.
  **中文：** 从 `~/.ssh/config` 解析别名、用户、端口和 IdentityFile，并支持私钥、OpenSSH 证书、加密密钥口令、Windows OpenSSH Agent、Pageant、已保存/现场输入密码以及 keyboard-interactive/MFA。
- **Connection reuse** — authenticated sessions are pooled by `user@host:port`, so additional SSH tabs open a new shell channel without repeating transport setup and authentication.
  **中文：** 已认证连接按 `user@host:port` 复用；后续 SSH 标签页直接创建新 Shell channel，无需重复传输握手和认证。
- **Standard host-key verification** — verifies and learns host keys through the standard `known_hosts` store, prompts on first connection, and rejects changed keys with a security warning.
  **中文：** 使用标准 `known_hosts` 校验和保存主机密钥；首次连接会确认，密钥变化时会拒绝连接并显示安全警告。
- **Authenticated remote Hook bridge** — remote AI lifecycle envelopes can travel through a private OSC protected by a random per-channel token; pane identity is always assigned locally before notifications are dispatched.
  **中文：** 远端 AI 生命周期信封可通过每通道随机令牌保护的私有 OSC 返回；通知分发前始终由本地分配 Pane 身份。
- **Built-in host editor** — the `SSH HOSTS` header has an add button and an internal form for `user@host`, optional non-default ports, and passwords.
  **中文：** `SSH HOSTS` 标题新增添加按钮和内部编辑面板，可输入 `user@host`、非默认端口和密码。
- **Secure credential persistence** — passwords are saved only with explicit consent and are stored in Windows Credential Manager, never in Nebula settings, command arguments, shell history, or logs.
  **中文：** 密码仅在用户明确选择保存时写入 Windows Credential Manager，绝不会进入 Nebula 设置、命令参数、Shell 历史或日志。
- **Host deletion and cleaner right-click behavior** — SSH rows keep their tab-style delete button and credential cleanup, while right-click no longer silently pins or reorders a host.
  **中文：** SSH host 行保留标签页式删除按钮和凭据清理；右键不再静默置顶或改变主机顺序。

### Session And Rendering / 会话与渲染

- **Smoother workspace interaction** — improves split layout, navigation animations, independent sidebar scrolling, tab rename input, hover hit-testing, and the resize HUD.
  **中文：** 改进分屏布局、导航动画、侧栏独立滚动、标签重命名输入、hover 命中和 resize HUD。
- **Safe image staging** — full-color shell icons, AI brand marks, and OSC 1337 images are staged into a final texture pass so inline images cannot corrupt later glyph batches.
  **中文：** 彩色 Shell 图标、AI 品牌标识和 OSC 1337 图片统一进入帧末贴图阶段，避免内联图片破坏后续 glyph batch。
- **Richer pane state** — expands OSC, cwd, process-state, and pane event routing for the directory tree, SSH activity, and AI CLI status indicators.
  **中文：** 扩展 OSC、cwd、进程状态和 pane 事件链路，为目录树、SSH 活动和 AI CLI 状态提供实时数据。

### Notes / 说明

- **Major update** — this release spans UI chrome, tabs and splits, the file drawer, Markdown, shell profiles, SSH, and the rendering pipeline.
  **中文：** 这是自 0.2.1 以来的大版本更新，覆盖 UI chrome、标签与分屏、文件抽屉、Markdown、Shell Profile、SSH 和渲染管线。

## 0.2.1 - 2026-07-11

### Fixes / 修复

- **Per-pane event routing** — window event batches previously resolved to one target pane, allowing output from a background tab to misroute keyboard input or terminal query replies. Events now route to their source pane, user input always targets the focused pane, and events for closed panes are dropped.
  **中文：** 修复逐 pane 事件路由：过去窗口事件批次只解析到单一 pane，后台标签输出可能导致键盘输入或终端查询回复发往错误 PTY；现在事件按来源 pane 路由，用户输入始终进入焦点 pane，已关闭 pane 的事件直接丢弃。
- **CJK text in chrome rendering** — removed the phantom spacer consumed after every wide glyph, which previously swallowed alternating CJK characters in ghost hints, HUD text, and link previews.
  **中文：** 修复 chrome 中的 CJK 文本渲染：移除宽字符后的虚假 spacer，避免幽灵提示、HUD 和链接预览隔字丢失。
- **History capture for wrapped prompts** — prompt text is reconstructed across soft-wrapped rows and snapshotted from the grid on Enter, preventing desynchronized keystroke buffers from polluting history.
  **中文：** 修复换行 prompt 的历史捕获：命令会跨软换行重建，并在按下 Enter 时直接从网格快照，避免失同步的按键缓冲污染历史。
- **`git.exe` close-confirmation noise** — Nebula's short-lived prompt helper is treated as stateless plumbing and no longer blocks tab closure with a busy-process dialog.
  **中文：** 修复 `git.exe` 触发关闭确认的问题：Nebula prompt 的短生命周期 git 辅助进程现在视为无状态工具，不再阻止标签页关闭。
- **Process lingering after window close** — teardown now terminates the shell tree first and drains ConPTY output on a detached thread, preventing `ClosePseudoConsole` deadlocks.
  **中文：** 修复窗口关闭后进程残留：销毁流程先终止 Shell 进程树，再由独立线程排空 ConPTY 输出，避免 `ClosePseudoConsole` 死锁。
- **ConPTY sideload hygiene** — `conpty.dll` is loaded only by absolute path when its matching `OpenConsole.exe` is present; failed resize calls now log warnings instead of aborting.
  **中文：** 改进 ConPTY side-load：仅在配套 `OpenConsole.exe` 存在时通过绝对路径加载 `conpty.dll`；resize 失败改为记录警告而非终止进程。

### Housekeeping / 工程维护

- **License and fixtures** — consolidated third-party attribution into `THIRD-PARTY-NOTICES` and renamed reference fixtures after the behavior they cover.
  **中文：** 将第三方许可归集到 `THIRD-PARTY-NOTICES`，并按实际行为重新命名参考测试 fixture。

## 0.2.0 - 2026-07-10

### Shell Experience / Shell 体验

- **Ctrl+V paste** — Windows and Linux users can paste with the expected shortcut while preserving bracketed paste and multi-line confirmation.
  **中文：** Windows 和 Linux 支持使用预期的 `Ctrl+V` 粘贴，同时保留 bracketed paste 和多行粘贴确认。
- **Safer pane spawning** — new tabs and splits validate inherited cwd before spawning, avoiding `os error 267` for deleted or virtual directories.
  **中文：** 新建标签和分屏前验证继承的 cwd，避免目录已删除或为虚拟目录时出现 `os error 267`。
- **SSH passthrough** — `nebula ssh user@host` bootstraps Nebula integration on Linux bash/zsh remotes while preserving forwarding, query, and explicit-command forms.
  **中文：** `nebula ssh user@host` 可在 Linux bash/zsh 远端引导 Nebula 集成，同时保持转发、查询和显式远程命令模式原样透传。

### AI Workflow / AI 工作流

- **opencode integration** — adds an opencode plugin that routes turn state through the same sidebar and toast bridge as Claude Code and Codex.
  **中文：** 新增 opencode 插件，通过与 Claude Code、Codex 相同的侧栏和通知桥接传递回合状态。
- **Remote AI awareness** — OSC cwd and command-state signals from bootstrapped SSH sessions update the local sidebar.
  **中文：** 已引导的 SSH 会话可把 OSC cwd 和命令状态信号传回本地侧栏。

### UI And UX / UI 与交互

- **Right-side Files/Git drawer** — adds filtering, persistent selection, drag-to-paste, Git staging/commit/push actions, and geometry aligned with the left tabs panel.
  **中文：** 新增右侧 Files/Git 抽屉，支持筛选、持久选择、拖拽粘贴和 Git 暂存/提交/推送，并与左侧标签栏对齐。
- **Chrome refactor** — moves chrome and side-panel rendering into dedicated modules while keeping rendering and hit-testing geometry synchronized.
  **中文：** 将 chrome 和侧面板渲染拆分到独立模块，同时保持渲染与 hit-test 几何同步。
- **Default font** — changes the packaged Nerd Font to `MapleMonoNormal-NF-CN-Regular.ttf`.
  **中文：** 发布包默认 Nerd Font 更换为 `MapleMonoNormal-NF-CN-Regular.ttf`。
- **Release documentation** — updates README and INSTALL for the 0.2 package and GPL-3.0-only licensing.
  **中文：** 更新 README 与 INSTALL 中的 0.2 发布包和 GPL-3.0-only 许可说明。

## 0.1.0 - 2026-07-07

Nebula Terminal's first public release.

Nebula Terminal 的第一个公开版本。

### AI Integration / AI 集成

- **Real brand marks in the sidebar** — renders the Anthropic starburst for `claude`, the OpenAI blossom for `codex`, and Nerd Font icons for other common developer tools.
  **中文：** 侧栏为 `claude` 显示 Anthropic 星芒、为 `codex` 显示 OpenAI 花结，并为其他常见开发工具显示 Nerd Font 图标。
- **Live turn state** — Claude Code hooks and Codex notify call the dependency-free `nebula-hook.exe`, forwarding prompt, completion, and input-needed events over a named pipe.
  **中文：** Claude Code hooks 和 Codex notify 调用无依赖的 `nebula-hook.exe`，通过命名管道转发提交、完成和等待输入事件。
- **Click-to-focus notifications** — activating a toast raises the window, selects the originating tab, and focuses the originating split.
  **中文：** 点击通知会前置窗口、选择来源标签页并聚焦来源分屏。
- **Zero setup and self-healing** — hook entries install automatically, recover after external configuration rewrites, remain scoped to Nebula, and can be removed with `nebula setup-ai --remove`.
  **中文：** hook 条目自动安装，可在外部配置重写后自愈，仅作用于 Nebula，并可通过 `nebula setup-ai --remove` 移除。
- **Codex chain mode** — wraps an existing Codex notifier instead of replacing it.
  **中文：** Codex chain 模式会包装已有 notifier，而不是覆盖它。
- **Fallback signals** — OSC 133 and BEL cover other CLIs and report long-command completion with duration.
  **中文：** OSC 133 和 BEL 为其他 CLI 提供兜底，并在长命令结束时报告耗时。

### Persistent Sessions / 会话保活

- **Session residency** — closing a window detaches its tabs while PTYs continue running; relaunching reattaches to the same processes and scrollback.
  **中文：** 关闭窗口仅分离标签页，PTY 继续运行；再次启动可接回相同进程和滚屏内容。
- **Cold restore** — autosaved tab layout and working directories restore after reboot or crash, with crash-loop protection.
  **中文：** 重启或崩溃后可从自动快照恢复标签布局和工作目录，并带崩溃循环保护。
- **Single instance** — subsequent launches hand off to the resident process.
  **中文：** 后续启动会交给常驻进程处理，保持单实例。

### Interface / 界面

- **Seven-theme skin system** — one token system drives seven light/dark themes across chrome, prompts, and dialogs, with persistence and hot reload.
  **中文：** 一套设计 token 驱动七种明暗主题，覆盖 chrome、prompt 和对话框，并支持持久化与热重载。
- **Sidebar tabs and splits** — supports tab reordering, drag-to-dock splits, dimmed unfocused panes, zoom, and CJK-aware chrome text.
  **中文：** 支持标签排序、拖拽停靠分屏、非焦点 pane 变暗、pane 缩放和 CJK-aware chrome 文本。
- **Quick terminal** — provides a global-hotkey Quake-style terminal with slide animation.
  **中文：** 提供全局快捷键唤起的 Quake 风格终端和滑入动画。
- **In-app settings** — configures themes, backgrounds, opacity, shells, and completion behavior in grouped panels with true clipping.
  **中文：** 应用内设置支持主题、背景、透明度、Shell 和补全行为，并使用真正裁剪的分组面板。
- **Chrome utilities** — adds the command palette, resize HUD, auto-hiding scrollbar, and visual bell.
  **中文：** 新增命令面板、resize HUD、自动隐藏滚动条和 visual bell。
- **Inline images** — supports OSC 1337 images with lazy upload and scrollback anchoring.
  **中文：** 支持 OSC 1337 内联图片、延迟上传和滚屏锚定。
- **Welcome page** — adds a fastfetch-style system introduction for new tabs.
  **中文：** 新标签页提供 fastfetch 风格的系统欢迎信息。

### Performance And Correctness / 性能与正确性

- **Modern ConPTY host** — bundles `conpty.dll` and `OpenConsole.exe`, pre-primes the DA1 handshake, improves resize behavior, and retains an in-box fallback.
  **中文：** 随包提供 `conpty.dll` 和 `OpenConsole.exe`，预热 DA1 握手、改善 resize，并保留系统内置 ConPTY 回退。
- **Coalesced resizing** — interactive resizing updates the PTY once after the drag settles, while rendering remains damage-tracked.
  **中文：** 交互式 resize 在拖动结束后一次性通知 PTY，同时继续使用 damage tracking 渲染。
- **Boot instrumentation** — `NEBULA_BOOT_TRACE=1` reports per-stage startup timing.
  **中文：** `NEBULA_BOOT_TRACE=1` 可输出逐阶段启动耗时。
- **Native notifications** — WinRT toasts use a registered Nebula identity, taskbar flashing, throttling, and a worker thread that cannot block rendering.
  **中文：** WinRT 通知使用注册的 Nebula 身份、任务栏闪烁和全局限流，并在独立线程运行以避免阻塞渲染。

### Shell Experience / Shell 体验

- **Fish-style ghost completions** — suggests commands from persistent JSONL history and filesystem paths, accepted with Right Arrow or Tab.
  **中文：** 从持久化 JSONL 历史和文件路径提供 fish 风格幽灵补全，可使用右方向键或 Tab 接受。
- **Built-in powerline prompt** — provides a themed Git branch and clock prompt for PowerShell and Git Bash without plugins.
  **中文：** 为 PowerShell 和 Git Bash 提供无需插件、包含 Git 分支和时钟的主题化 powerline prompt。
- **Input quality-of-life fixes** — supports unquoted paths with spaces, safely rewrites bare PowerShell environment assignments, and adds colored, clickable `ls` output.
  **中文：** 支持未加引号的空格路径、安全改写裸 PowerShell 环境变量赋值，并为 `ls` 增加彩色可点击输出。
- **OSC coverage** — supports OSC 7, 8, 9, 9;9, 133, and 1337 for cwd, hyperlinks, notifications, semantic prompts, and images.
  **中文：** 支持 OSC 7、8、9、9;9、133 和 1337，覆盖 cwd、超链接、通知、语义 prompt 和图片。
