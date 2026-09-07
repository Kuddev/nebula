<p align="center">
  <img src="extra/logo/nebula.png" alt="Pebrel Titanium icon" width="148" height="148" />
</p>

<h1 align="center">Pebrel</h1>

<p align="center">
  <strong>A GPU-accelerated terminal, SSH workspace, and home for your AI CLI sessions.</strong>
</p>

<p align="center">
  Built with Rust and GPUI for Windows, macOS, and Linux.<br />
  Split terminals · SSH &amp; SFTP · Claude Code &amp; Codex workflows · Native document reader
</p>

<p align="center">
  <a href="https://github.com/Kuddev/pebrel/releases/latest"><img src="https://img.shields.io/github/v/release/Kuddev/pebrel?style=for-the-badge&label=release&color=007ec6" alt="Latest release" /></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-007ec6?style=for-the-badge" alt="Windows, macOS, and Linux" />
  <img src="https://img.shields.io/github/license/Kuddev/pebrel?style=for-the-badge&color=79a900" alt="GPL-3.0 license" />
</p>

<p align="center">
  <a href="https://github.com/Kuddev/pebrel/releases/latest"><img src="https://img.shields.io/badge/Download-latest%20release-35a854?style=for-the-badge&logo=github&logoColor=white" alt="Download the latest release" /></a>
</p>

<p align="center">
  <strong>English</strong> · <a href="README.zh-CN.md">简体中文</a>
</p>

---

<p align="center">
  <img src="docs/screenshots/nebula-top-tabs.png" alt="Terminal tabs in Pebrel" width="1040" />
</p>

## One Workspace

Pebrel (formerly Nebula) brings local shells, remote hosts, files, and AI command-line
tools into a native desktop workspace. Arrange terminals in tabs and splits, follow
each agent's activity, and read its output without leaving the application.

### Terminals and Sessions

- Sidebar or top tabs, draggable split panes, and per-pane working directories.
- Saved workspace layouts and optional AI conversation restoration. On WSL and
  Linux, Codex session metadata can supply an ID when its hook has not reported one.
- On Windows, optional background residency keeps running sessions alive when you
  close the window. Restoring a conversation after the process exits is a separate
  feature and requires a supported CLI and a usable session identity.
- History and path completions, configurable keybindings, and integrated shell prompts.

### SSH and Files

- Saved hosts, SSH config aliases, proxy and jump-host options, private-key and
  keyboard-interactive authentication, and host-key verification.
- SFTP browsing, uploads and downloads, folder transfers, progress, and cancellation.
- Local file browsing and Git actions alongside your terminals.
- Duplicated WSL and SSH tabs retain their known working directory.

### AI CLI Workflows

- Claude Code, Codex, and other recognized CLIs have their own icons and activity
  states. Supported hook events provide more precise progress and attention signals.
- Notifications follow their source pane; clicking one returns you to that terminal.
- Captured Claude Code and Codex answers open in a reader with Markdown, formulas,
  source text, and local image previews.
- Clipboard images are saved as PNG files and their paths inserted into local,
  WSL, or SSH sessions. Inline terminal images require the CLI to emit the supported
  OSC 1337 protocol; image attachment previews depend on the CLI itself.

### A Native, Configurable Interface

- GPU-accelerated GPUI interface, light and dark themes, backgrounds, and opacity controls.
- Application icon palettes and eleven UI language choices with English fallback
  for untranslated text.
- Searchable settings, a command palette, and Lua configuration with validation and
  live reload. Existing TOML configuration remains supported.
- Markdown document tabs and native mathematical typesetting without a WebView.

<details>
<summary>More screenshots</summary>

<p align="center"><img src="docs/screenshots/nebula-claude-session.png" alt="Claude Code session" width="1040" /></p>
<p align="center"><img src="docs/screenshots/split-ai-workflows.png" alt="AI CLI split panes" width="1040" /></p>
<p align="center"><img src="docs/screenshots/ai-sidebar.png" alt="AI activity sidebar" width="300" /></p>
<p align="center"><img src="docs/screenshots/ssh.gif" alt="Native SSH session" width="1040" /></p>
<p align="center"><img src="docs/screenshots/native-math-rendering.png" alt="Native formula rendering" width="1040" /></p>
<p align="center"><img src="docs/screenshots/themes.png" alt="Application themes" width="1040" /></p>

</details>

## Download

Choose a package from the **[latest release](https://github.com/Kuddev/pebrel/releases/latest)**.

| System | Architecture | Packages |
| --- | --- | --- |
| Windows 10 1809+ / 11 | x64 | Installer `.exe`, portable `.zip` |
| Linux Preview, glibc 2.35+ | x64 | `.AppImage`, `.deb`, portable `.tar.gz` |
| macOS Preview, deployment target 14+ | Apple Silicon / arm64 | `.dmg` |
| macOS Preview, deployment target 14+ | Intel / x64 | `.dmg` |

Package names follow `Pebrel-v<version>-<system>-<architecture>`. Windows offers
stable installers ending in `-setup.exe` and portable ZIPs. Linux and macOS are
Preview builds, marked `-preview` before the extension. A legacy-named Windows
installer is also supplied for older automatic-update clients.

On Windows, run the installer or extract the ZIP and launch `pebrel.exe`, keeping
the bundled directories together. On Linux, install the DEB or make the AppImage
executable. On macOS, open the matching DMG and drag Pebrel into Applications.
Ad-hoc-signed macOS builds may require **Open Anyway** in **System Settings >
Privacy & Security** on first launch. Native macOS CI runs on macOS 15; the
deployment target alone does not establish validation on every older OS version.

Windows currently also provides tray residency, the global quick-terminal hotkey,
automatic local AI-hook setup, and automatic update installation. These integrations
are not yet available on Linux or macOS. See [installation details](INSTALL.md) for
platform requirements and upgrading an existing Nebula installation.

## Configure

```sh
pebrel config init --language en-US
pebrel config check
```

The generated Lua configuration uses `require 'pebrel'` and
`pebrel.config_builder()`. Invalid reloads retain the last valid configuration.
See the [Lua configuration guide](docs/lua-configuration.md) for settings and examples.

## Build

With the pinned Rust toolchain and the platform dependencies installed:

```sh
cargo build --release --locked -p nebula --bin pebrel --features gpui-shell
```

The internal Cargo package is still named `nebula`; the application and command
are `pebrel`. GPUI is the product interface. The older renderer is available only
through the explicit `legacy-shell` feature.

## Acknowledgements

Pebrel builds on [Alacritty](https://github.com/alacritty/alacritty),
[GPUI](https://github.com/zed-industries/zed), and
[gpui-component](https://github.com/longbridge/gpui-component). Terminal text uses
Maple Mono, and native formulas use Latin Modern Math. Upstream copyright and
license notices are preserved in `THIRD-PARTY-NOTICES` and `licenses/`.
