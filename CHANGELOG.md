# Changelog

## 0.3.0 - 2026-07-12

### Highlights

- 全面重设计窗口 UI：顶部栏与左侧栏组成连续 L 形 chrome，统一设置、命令面板、确认框、抽屉和主题视觉。
- 深度还原 Windows Terminal 标签交互：拖拽排序、拖入当前终端形成分屏、边缘停靠预览、鼠标光标与位移动画反馈。
- 新增右侧目录树与 Git 抽屉，支持文件筛选、展开、拖拽路径、文件状态、提交与推送操作，并采用新的文件类型彩色图标。
- 新增 Markdown/GFM 只读查看器，支持标题、列表、表格、任务列表、代码块、引用、链接和滚动浏览。
- 新增 Shell 探测与彩色品牌图标，覆盖 PowerShell、CMD、Git Bash、Nushell、WSL 及常见 Linux 发行版。

### Terminal And Profiles

- 标签栏 `+` 旁新增 Shell 下拉选择器，可直接使用检测到的执行器或配置 Profile 创建标签页。
- 设置页“默认 Shell”改为原地展开的内联列表，显示完整彩色图标；选择后立即持久化并用于新会话。
- 默认 Shell 设置支持富 ID，包括 `cmd`、`pwsh`、`nu` 与 `wsl:<distribution>`，并继续兼容 PowerShell/Git Bash 的 Nebula prompt bootstrap。
- 新增窗口透明度、背景图片和背景图片透明度控制，设置面板支持分区滚动。

### SSH

- 保存的 SSH host 现在在用户设置的默认 Shell pane 内自动运行，不再把系统 `ssh.exe` 作为外部控制台根进程，因此不会弹出独立黑窗。
- SSH hosts 标题新增 `+`，可在内部面板输入 `用户名@地址`、端口形式和密码；密码仅在用户选择保存时写入 Windows Credential Manager。
- SSH host 行新增与标签页一致的删除按钮；删除记录时同步清理 pin 与 Windows 凭据。
- OpenSSH AskPass 支持已保存密码自动登录、密码失效后重新询问、host key 确认，以及会话结束后的临时状态清理。
- PowerShell、CMD、Git Bash、Nushell 与 WSL 均使用各自安全转义方式注入 SSH 命令。

### Session And Rendering

- 改进分屏布局、切换动画、侧栏滚动、重命名输入、hover 命中与 resize HUD。
- 彩色 Shell 图标、AI 品牌标识和 OSC 1337 图片统一进入帧末贴图阶段，避免破坏文字 glyph batch。
- 扩展 OSC、cwd、进程状态和 pane 事件链路，为目录树、SSH 活动与 AI CLI 状态提供实时数据。

### Notes

- 这是自 0.2.1 以来的大版本更新，涉及 UI、标签/分屏、文件抽屉、Markdown、Shell Profile、SSH 与渲染管线。
- SSH 密码不会写入 `nebula_settings.txt`、命令行参数、Shell 历史或普通日志。

## 0.2.1 — 2026-07-11

### 🐛 Fixes

- **Per-pane event routing** — window event batches were resolved to a single
  target pane: typing while a background tab's program (a build, `codex`, a
  `tail`) was printing could leak the keystrokes into that tab's PTY, and
  terminal query answers (cursor-position / device-attribute reports) could
  land in the wrong shell and appear as stray `[10…`-style characters in the
  input line. Terminal events now route to their source pane and keyboard /
  mouse input always goes to the focused pane; events for an already-closed
  pane are dropped instead of stalling the batch.
- **CJK text in chrome rendering** — the string renderer consumed a phantom
  "spacer" after every wide glyph, swallowing every second character of
  CJK strings in the inline ghost hint (`资料` → `资`…), HUD text and link
  previews. It now lays glyphs out by display width and clips at the row
  edge instead of bleeding past it.
- **History capture for wrapped prompts** — the prompt line is reconstructed
  across soft-wrapped rows (long CJK paths wrap fast at two columns per
  char) and snapshotted straight off the grid at Enter time; when the grid
  read fails the line is no longer taken from the desync-prone keystroke
  buffer, so spliced garbage can't enter the ghost-hint history anymore.
- **`git.exe` close-confirmation noise** — Nebula's own prompt integration
  spawns `git` on every prompt render, and the close-confirmation snapshot
  routinely caught one mid-flight, blocking tab close with a modal. `git.exe`
  is now whitelisted as stateless plumbing.
- **Process lingering after window close** — `ClosePseudoConsole` blocks
  until the console host flushes its remaining output; once teardown began
  nothing drained the pipe, so an output burst deadlocked the close and left
  `nebula.exe` running in Task Manager. Teardown now terminates the shell
  tree first and hands conout to a detached drain thread.
- **ConPTY sideload hygiene** — `conpty.dll` is loaded only by absolute path
  from the executable's directory, and only when `OpenConsole.exe` sits
  beside it, so the pre-primed DA1 handshake answer can no longer leak into
  the shell as typed input when the bundled host is missing. A failed
  `ResizePseudoConsole` now logs a warning instead of aborting the process.

### 🧹 Housekeeping

- Third-party license attribution consolidated into `THIRD-PARTY-NOTICES`;
  reference-test fixtures renamed after the behaviour they cover.

## 0.2.0 — 2026-07-10

### 🐚 Shell experience

- **Ctrl+V paste** — Windows/Linux users can paste with the expected shortcut, while the existing bracketed-paste and multi-line paste confirmation flow stays intact.
- **Safer pane spawning** — new tabs and splits validate inherited cwd before spawning, avoiding `os error 267` when the shell reports a deleted or virtual directory.
- **SSH passthrough** — `nebula ssh user@host` can bootstrap Nebula shell integration on Linux bash/zsh remotes while preserving forwarding/query forms such as `-N -L`, `-W`, `-G`, and explicit remote commands.

### 🤖 AI workflow

- **opencode integration** — Nebula can install an opencode plugin and route its turn state through the same sidebar/toast bridge as Claude Code and Codex.
- **Remote AI awareness** — OSC cwd and command-state signals from a bootstrapped SSH session flow back into the local sidebar.

### 🎨 UI / UX

- **Right-side Files/Git drawer** — adds a floating file tree / git drawer with filtering, selection, drag-to-paste, git staging/commit/push actions, and geometry aligned with the left TABS panel.
- **Chrome refactor** — extracts chrome and side-panel drawing into dedicated modules, keeping hit-testing and rendering geometry in sync.
- **0.2 default font** — switches the release package to `MapleMonoNormal-NF-CN-Regular.ttf` / Maple Mono Normal NF CN for the default packaged Nerd Font.
- **Cleaner release docs** — README/INSTALL now point at the v0.2.0 package and GPL-3.0-only license text.

## 0.1.0 — 2026-07-07

Nebula Terminal 的第一个公开版本 / First public release.

### 🤖 AI integration

- **Real brand marks in the sidebar** — Anthropic starburst for `claude`,
  OpenAI blossom for `codex` (textured quads, theme-tinted); Nerd Font icons
  for `gemini`, `copilot`, `cursor`, `aider`, `git`, `vim`, `cargo` and more.
- **Live turn state, wired to the source** — Claude Code hooks / Codex notify
  invoke the bundled `nebula-hook.exe` (dependency-free), which forwards
  typed events over a named pipe: prompt submitted → sidebar spinner; turn
  finished → dot + toast; input needed → toast with the actual message text.
  No shell integration required.
- **Click-to-focus toasts** — activating a notification surfaces the window,
  switches to the raising tab and focuses the raising split.
- **Zero-setup & self-healing** — hook entries install on first boot, heal
  if an external config switcher wipes them (a config-directory watcher
  re-applies them), and are scoped by environment variables so claude in
  other terminals is untouched. `nebula setup-ai [--remove]`,
  `nebula notify-test` diagnostics.
- **Chain mode for codex** — a pre-existing notifier in codex's single
  `notify` slot keeps firing (`--chain` wrapping), never evicted.
- **Fallback signals** — OSC 133 command tracking + BEL cover every other
  CLI: long commands toast on completion with their duration.

### ♻️ Sessions that survive

- **Session residency** — closing a window detaches its tabs into the
  resident process; PTYs (running `claude`, builds, SSH) never stop.
  Relaunch re-attaches: same processes, same scrollback.
- **Cold restore** — tab layout and per-tab working directories restore from
  a 1 Hz autosaved snapshot after reboot/crash, with crash-loop protection.
- **Single instance** — a second launch hands over to the resident process.

### 🎨 Interface

- **Seven-theme skin system** — Nebula plus three matched light/dark pairs
  (Silver Light / Steel Dark, Limestone / Coal Dark, Linen Light / Moss
  Dark); one token system drives chrome, prompt, dialogs; persisted and
  hot-reloadable.
- **Sidebar tabs & splits** — drag to reorder, drag into the terminal area to
  dock as a split; unfocused panes dim instead of growing borders; zoomable
  panes; CJK-aware chrome text.
- **Quick terminal** — global hotkey drops a Quake-style overlay with a slide
  animation.
- **In-app settings panel** — themes, background image & opacity, shell,
  completion behavior; grouped glass panels with true scissor clipping.
- **Command palette, resize HUD, auto-hiding scrollbar, visual bell.**
- **Inline images** — OSC 1337 protocol, lazily uploaded, anchored to
  scrollback rows.
- **Welcome page** — fastfetch-style system intro on new tabs.

### ⚡ Performance & correctness

- **Modern ConPTY host side-loaded** (`conpty.dll` + `OpenConsole.exe`,
  MIT-licensed from microsoft/terminal) with the DA1 handshake pre-primed so
  a new tab doesn't stall on that round-trip, and resizing no longer smears
  full-screen TUIs into scrollback. `openconsole = false` falls back to the
  in-box host.
- **Coalesced interactive resizing** — the PTY learns its final size once per
  drag; rendering is damage-tracked.
- **Boot instrumentation** — `NEBULA_BOOT_TRACE=1` prints a per-stage timing
  trace.
- **Native notifications done right** — WinRT toasts under a registered
  `Nebula` app identity (icon included), taskbar flash, global toast
  throttle; delivery isolated on a worker thread so a slow notification stack
  can never stall rendering.

### 🐚 Shell experience

- **Fish-style ghost completions** — dim inline suggestions from persistent
  JSONL history and filesystem paths; accepted with `→` / `Tab`.
- **Built-in themed powerline prompt** — git branch + clock for PowerShell
  and Git Bash, zero plugins; prompt palette follows the app theme.
- **Quality-of-life input fixes** — unquoted `cd D:/Program Files` works,
  bare `$env:KEY=value` auto-quotes, `ls` gains colors and OSC 8 clickable
  hyperlinks.
- **OSC coverage** — 7 / 8 / 9 / 9;9 / 133 / 1337 (cwd, hyperlinks,
  notifications, semantic prompts, images).
