# Installing Pebrel

Pebrel 1.6 uses the `pebrel` command, `pebrel.exe`, and Pebrel package names.
Older releases keep their original filenames. Use the new installer to migrate
an existing Nebula installation; configuration migration is handled by the
application at startup.

## Linux

Stable releases provide three Linux x64 packages.

- Debian/Ubuntu: install
  `Pebrel-v<version>-linux-x64.deb` with
  `sudo apt install ./Pebrel-v<version>-linux-x64.deb`.
  Remove it with `sudo apt remove pebrel`.
- AppImage: make
  `Pebrel-v<version>-linux-x64.AppImage` executable and
  run it directly. It does not register itself with the system package manager.
- Portable archive: extract the `tar.gz` and run its `AppRun` launcher. Keep
  the AppDir layout intact so bundled libraries are found correctly.

The Linux target is x86_64 with glibc 2.35 or newer. The
release workflow builds on Ubuntu 22.04 and runs the same
Runtime API conformance suite against the final AppImage under X11 and
Wayland, and against the installed Debian package under X11.

SSH password login works without a keyring. Saving SSH passwords or encrypted-key
passphrases requires `libsecret-tools` and an unlocked Secret Service keyring
(for example GNOME Keyring or a compatible KWallet setup). The Debian package
recommends these dependencies. If storage is unavailable, enter the secret for
the current connection instead; Pebrel does not silently claim to save it.

## macOS

Download the DMG matching the Mac architecture:

- `Pebrel-v<version>-macos-arm64.dmg` for Apple Silicon Macs.
- `Pebrel-v<version>-macos-x64.dmg` for Intel Macs.

Open the DMG and drag **Pebrel** into Applications. The 1.6.0
packages use ad-hoc signing, without Apple notarization. For a
download you have verified and trust, macOS may require **System Settings →
Privacy & Security → Open Anyway** after an initial launch is blocked. Do not
disable Gatekeeper globally. Developer ID/notarized builds are explicitly
identified in their release notes; that mode requires the maintainer's Apple
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

Linux/macOS do not yet provide full Windows feature parity: tray/close-to-background residency,
global quick-terminal hotkeys, automatic update installation, and automatic
local AI-hook configuration are not enabled on Linux/macOS. Native IME, display
scaling, notification permissions, and interactive SSH/SFTP still need native
user testing. The release procedure and acceptance checklist are in
[`docs/preview-release-checklist.md`](docs/preview-release-checklist.md).

Preview builds remain separate from stable releases. Their filenames contain
`-preview.<id>` and use `x86_64` / `aarch64`; the Debian package is
`pebrel-preview` and the macOS bundle is **Pebrel Preview**.

## Windows installer (recommended)

1. Download `Pebrel-v<version>-windows-x64-setup.exe` from the
   [Releases](https://github.com/Kuddev/pebrel/releases/latest) page.
2. Follow the wizard to choose the installation directory and optional desktop
   or Windows sign-in shortcuts. The default per-user installation does not
   require administrator rights.
3. The installer offers system installation of the bundled Maple Mono font and
   can launch Pebrel on the final page. The application also embeds the font.

### Upgrade an existing Nebula installation

The installer keeps the existing application identity so Windows treats Pebrel
as an upgrade. A registered installation ending in `Nebula Terminal` moves to
the sibling `Pebrel` directory. For the old default installation, this changes
`%LOCALAPPDATA%\Programs\Nebula Terminal` to
`%LOCALAPPDATA%\Programs\Pebrel`. Other custom directory names are preserved,
and an explicitly chosen installer directory takes precedence.

Close the old application normally before installing. The installer updates its
managed shortcuts and PATH entry, installs `pebrel.exe` and
`runtime/pebrel-hook.exe`, and removes recognized old program files. Unknown
files and user configuration remain in place. If cleanup fails after the new
program is installed, the installer reports the failure and retains retry state.

On startup, Pebrel copies old application data into `%APPDATA%\Pebrel` without
overwriting newer files. The old data directory remains available for recovery
and existing absolute configuration imports. See
[Lua configuration and migration](docs/lua-configuration.md#existing-nebula-data)
for discovery order and the `PEBREL_*` environment variables.

Uninstalling closes Pebrel, runs `pebrel setup-ai --remove` before deleting the
program files, and removes Pebrel's Claude, Codex, opencode, and Pi integration.
Other user-owned hook and notifier entries are preserved.

## Portable archive

1. Download `Pebrel-v<version>-windows-x64.zip` from the Releases page.
2. Unzip it anywhere.
3. Run `pebrel.exe`. The portable application includes its terminal font, so
   no separate font installation is required. The installer offers optional
   system font installation for use in other applications.

Keep the extracted directory structure intact:

| Path | Purpose |
| --- | --- |
| `pebrel.exe` | the terminal |
| `README.md` | overview and usage |
| `runtime/pebrel-hook.exe` | AI turn-notification bridge (Claude Code / Codex) |
| `runtime/conpty.dll` + `runtime/OpenConsole.exe` | modern ConPTY host (correct resize, fast tab spawn) |
| `docs/CHANGELOG.md` + `docs/INSTALL.md` + `docs/lua-configuration.md` | release changes, installation, and Lua configuration |
| `skills/pebrel-runtime/` | instructions for controlling Pebrel through its Runtime API |
| `licenses/` | Pebrel and third-party license notices |

## Build from source

Install [rustup](https://rustup.rs) and the build dependencies for your platform.
The repository pins Rust 1.97.1 in `rust-toolchain.toml`. Linux dependency packages
are listed in `.github/workflows/release.yml`; macOS requires Xcode command-line
tools. Windows requires Windows 10 1809+ / 11 and a supported Rust linker toolchain.

```powershell
git clone https://github.com/Kuddev/pebrel
cd pebrel
cargo build --release --locked -p nebula --bin pebrel --features gpui-shell
```

Build and assemble the portable archive with:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/package-release.ps1 `
  -Version unreleased -Force
```

The script builds the release workspace, verifies every required input, stages
the directory layout above, creates the ZIP, and prints its file count, packed
and unpacked sizes, and SHA-256. Final releases must use a fresh build;
`-SkipBuild` and `-AllowStale` are reserved for packaging-script tests.

Build the wizard-based installer with Inno Setup 6.7.3:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/build-installer.ps1 -Force
```

The installer build validates the same runtime inputs, pins and verifies the
UTF-8 Simplified Chinese wizard translation, and prints the setup executable's
size and SHA-256. Pass `-InnoCompiler` when `ISCC.exe` is installed in a custom
directory.

## First run

- Toast notifications register under the `Pebrel` app identity automatically.
- Claude Code / Codex turn notifications are wired on first boot
  (`pebrel setup-ai --remove` to undo; `pebrel notify-test` to verify the
  toast pipeline).
- New configuration uses `%APPDATA%\Pebrel\pebrel.lua`. Run
  `pebrel config init --language system` to create an annotated template and
  `pebrel config check` to validate it. Legacy Nebula configuration is supported
  when no Pebrel configuration is present; see the discovery order in the
  [Lua guide](docs/lua-configuration.md#discovery-order).
- Linux uses `$XDG_CONFIG_HOME/pebrel/pebrel.lua` (normally
  `~/.config/pebrel/pebrel.lua`); macOS uses
  `~/Library/Application Support/Pebrel/pebrel.lua`. Native package and runtime
  acceptance requirements are described in the Preview sections above.
- Visual settings remain available in the in-app settings panel.
