# Changelog / 更新日志

Every release entry is provided in English and Simplified Chinese.

每个版本条目均同时提供英文和简体中文说明。

## Unreleased / 未发布

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
