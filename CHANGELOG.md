# Changelog

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
