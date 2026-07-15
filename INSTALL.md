# Installing Nebula

## Release package (recommended)

1. Download `NebulaTerminal-v0.4.0-windows-x64.zip` from the
   [Releases](https://github.com/Kuddev/nebula/releases) page.
2. Unzip anywhere (no installer, no admin rights).
3. **Install the font**: open `fonts`, double-click
   `MapleMonoNormal-NF-CN-Regular.ttf`, and press
   *Install*. Nebula's powerline prompt and icons need this Nerd Font —
   without it they render as `□` boxes.
4. Run `nebula.exe`.

Keep the extracted directory structure intact:

| Path | Purpose |
| --- | --- |
| `nebula.exe` | the terminal |
| `README.md` | overview and usage |
| `runtime/nebula-hook.exe` | AI turn-notification bridge (Claude Code / Codex) |
| `runtime/conpty.dll` + `runtime/OpenConsole.exe` | modern ConPTY host (correct resize, fast tab spawn) |
| `fonts/MapleMonoNormal-NF-CN-Regular.ttf` | Nerd Font for powerline/icons — install once (SIL OFL 1.1) |
| `docs/CHANGELOG.md` + `docs/INSTALL.md` | release changes and installation details |
| `licenses/` | Nebula and third-party license notices |

## Build from source

Requirements: Windows 10 1809+ / 11, [Rust](https://rustup.rs) 1.85+.

```powershell
git clone https://github.com/Kuddev/nebula
cd nebula
cargo build --release
```

Build and assemble the portable archive with:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 `
  -Version unreleased -Force
```

The script builds the release workspace, verifies every required input, stages
the directory layout above, creates the ZIP, and prints its file count, packed
and unpacked sizes, and SHA-256. Use `-SkipBuild` only when the release binaries
have already been built and verified.

## First run

- Toast notifications register under the `Nebula` app identity automatically.
- Claude Code / Codex turn notifications are wired on first boot
  (`nebula setup-ai --remove` to undo; `nebula notify-test` to verify the
  toast pipeline).
- Configuration lives at `%APPDATA%\nebula\nebula.toml` (created on demand);
  visual settings are in the in-app settings panel.
