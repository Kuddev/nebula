# Installing Pebrel

Pebrel retains the `Kuddev/nebula` repository, `nebula` command, and existing
package identifiers for compatibility. Historical download filenames below
are unchanged; do not rename installed files or configuration directories.

## Linux Preview

Cross-platform Preview runs provide three Linux x86_64 assets. They are test
builds rather than stable releases.

- Debian/Ubuntu: install
  `NebulaTerminal-v<version>-preview.<id>-linux-x86_64.deb` with
  `sudo apt install ./NebulaTerminal-v<version>-preview.<id>-linux-x86_64.deb`.
  Remove it with `sudo apt remove nebula-terminal-preview`.
- AppImage: make
  `NebulaTerminal-v<version>-preview.<id>-linux-x86_64.AppImage` executable and
  run it directly. It does not register itself with the system package manager.
- Portable archive: extract the `tar.gz` and run its `AppRun` launcher. Keep
  the AppDir layout intact so bundled libraries are found correctly.

The initial Preview target is Linux x86_64 with glibc 2.35 or newer. The
Preview workflow is configured to build on Ubuntu 22.04 and run the same
Runtime API conformance suite against the final AppImage under X11 and
Wayland, and against the installed Debian package under X11.

SSH password login works without a keyring. Saving SSH passwords or encrypted-key
passphrases requires `libsecret-tools` and an unlocked Secret Service keyring
(for example GNOME Keyring or a compatible KWallet setup). The Debian package
recommends these dependencies. If storage is unavailable, enter the secret for
the current connection instead; Nebula does not silently claim to save it.

## macOS Preview

Download the DMG matching the Mac architecture:

- `macos-aarch64.dmg` for Apple Silicon Macs.
- `macos-x86_64.dmg` for Intel Macs.

Open the DMG and drag **Nebula Terminal Preview** into Applications. These
Preview builds default to ad-hoc signing, without Apple notarization. For a
download you have verified and trust, macOS may require **System Settings →
Privacy & Security → Open Anyway** after an initial launch is blocked. Do not
disable Gatekeeper globally. Developer ID/notarized builds are explicitly
identified in their Preview notes; that mode requires the maintainer's Apple
credentials and fails rather than falling back to ad-hoc signing.

The workflow builds both architectures on native macOS 15 runners. It checks
the final mounted DMG and launches a private installed copy through
LaunchServices with a minimal PATH and unset locale. The package deployment
target is macOS 14, but macOS 14 itself still needs separate runtime validation;
a deployment target is not a tested-OS guarantee.

SSH secrets are stored in macOS Keychain only when requested. A GUI launch uses
a UTF-8 locale and starts from the home directory when launched from `/`, while
an explicit working directory is preserved. Both platforms use the embedded
Maple Mono terminal font without requiring a system font installation.

Preview is not full Windows feature parity: tray/close-to-background residency,
global quick-terminal hotkeys, automatic update installation, and automatic
local AI-hook configuration are not enabled on Linux/macOS. Native IME, display
scaling, notification permissions, and interactive SSH/SFTP still need native
user testing. The release procedure and acceptance checklist are in
[`docs/preview-release-checklist.md`](docs/preview-release-checklist.md).

## Windows installer (recommended)

1. Download `NebulaTerminal-<version>-windows-x64-setup.exe` from the
   [Releases](https://github.com/Kuddev/nebula/releases) page.
2. Follow the wizard to choose the installation directory and optional desktop
   or Windows sign-in shortcuts. The default per-user installation does not
   require administrator rights.
3. The installer installs the bundled Maple Mono font and can launch Nebula on
   the final page.

Uninstalling closes Nebula, runs `nebula setup-ai --remove` before deleting the
program files, and removes Nebula's Claude, Codex, opencode, and Pi integration.
Other user-owned hook and notifier entries are preserved.

## Portable archive

1. Download `NebulaTerminal-<version>-windows-x64.zip` from the Releases page.
2. Unzip it anywhere.
3. **Install the font**: open `fonts`, double-click
   `MapleMonoNormal-NF-CN-Regular.ttf`, and press
   *Install*. Nebula embeds the same font as a runtime fallback, but the normal
   system installation remains recommended. Nebula checks at every launch and
   shows a dismissible reminder that can reopen the bundled `fonts` folder;
   restart Nebula after installing to load the complete icon set.
4. Run `nebula.exe`.

Keep the extracted directory structure intact:

| Path | Purpose |
| --- | --- |
| `nebula.exe` | the terminal |
| `README.md` | overview and usage |
| `runtime/nebula-hook.exe` | AI turn-notification bridge (Claude Code / Codex) |
| `runtime/conpty.dll` + `runtime/OpenConsole.exe` | modern ConPTY host (correct resize, fast tab spawn) |
| `fonts/MapleMonoNormal-NF-CN-Regular.ttf` | Nerd Font for powerline/icons — install once (SIL OFL 1.1) |
| `docs/CHANGELOG.md` + `docs/INSTALL.md` + `docs/lua-configuration.md` | release changes, installation, and Lua configuration |
| `licenses/` | Nebula and third-party license notices |

## Build from source

Requirements: Windows 10 1809+ / 11 and [rustup](https://rustup.rs). The
repository pins Rust 1.97.1 in `rust-toolchain.toml`.

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

Build the wizard-based installer with Inno Setup 6.7.3:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-installer.ps1 -Force
```

The installer build validates the same runtime inputs, pins and verifies the
UTF-8 Simplified Chinese wizard translation, and prints the setup executable's
size and SHA-256. Pass `-InnoCompiler` when `ISCC.exe` is installed in a custom
directory.

## First run

- Toast notifications register under the `Nebula` app identity automatically.
- Claude Code / Codex turn notifications are wired on first boot
  (`nebula setup-ai --remove` to undo; `nebula notify-test` to verify the
  toast pipeline).
- New configuration uses `%APPDATA%\nebula\nebula.lua`. Run
  `nebula config init --language system` to create an annotated template and
  `nebula config check` to validate it. Existing `nebula.toml` remains
  supported when no Lua configuration is present.
- Linux uses `$XDG_CONFIG_HOME/nebula/nebula.lua` (normally
  `~/.config/nebula/nebula.lua`) and supports both Wayland and X11. The initial
  binary baseline is x86_64 glibc on Ubuntu 24.04, Debian 12, and Fedora 42.
- Visual settings remain available in the in-app settings panel.
