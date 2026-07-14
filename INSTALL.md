# Installing Nebula

## Release package (recommended)

1. Download `NebulaTerminal-v0.4.0-windows-x64.zip` from the
   [Releases](https://github.com/Kuddev/nebula/releases) page.
2. Unzip anywhere (no installer, no admin rights).
3. **Install the font**: double-click `MapleMonoNormal-NF-CN-Regular.ttf` and press
   *Install*. Nebula's powerline prompt and icons need this Nerd Font —
   without it they render as `□` boxes.
4. Run `nebula.exe`.

Keep these files together in one directory:

| File | Purpose |
| --- | --- |
| `nebula.exe` | the terminal |
| `nebula-hook.exe` | AI turn-notification bridge (Claude Code / Codex) |
| `conpty.dll` + `OpenConsole.exe` | modern ConPTY host (correct resize, fast tab spawn) |
| `MapleMonoNormal-NF-CN-Regular.ttf` | Nerd Font for powerline/icons — install once (SIL OFL 1.1) |

## Build from source

Requirements: Windows 10 1809+ / 11, [Rust](https://rustup.rs) 1.85+.

```powershell
git clone https://github.com/Kuddev/nebula
cd nebula
cargo build --release
```

Artifacts land in `target/release/`. Copy
`assets/windows/conhost/{conpty.dll,OpenConsole.exe}` next to `nebula.exe`
when distributing (the build runs fine without them, falling back to the
in-box ConPTY, but resize behavior of full-screen TUIs is worse).

## First run

- Toast notifications register under the `Nebula` app identity automatically.
- Claude Code / Codex turn notifications are wired on first boot
  (`nebula setup-ai --remove` to undo; `nebula notify-test` to verify the
  toast pipeline).
- Configuration lives at `%APPDATA%\nebula\nebula.toml` (created on demand);
  visual settings are in the in-app settings panel.
