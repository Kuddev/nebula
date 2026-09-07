<p align="center">
  <img src="extra/logo/nebula.png" alt="Pebrel 钛银图标" width="148" height="148" />
</p>

<h1 align="center">Pebrel</h1>

<p align="center">
  <strong>GPU 加速终端、SSH 工作区，以及你的 AI CLI 会话空间。</strong>
</p>

<p align="center">
  使用 Rust 与 GPUI 构建，面向 Windows、macOS 和 Linux。<br />
  分屏终端 · SSH 与 SFTP · Claude Code 与 Codex 工作流 · 原生文档阅读
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024_edition-CE412B?style=for-the-badge&logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/OpenGL-ES_2.0%2B-5586A4?style=for-the-badge&logo=opengl&logoColor=white" alt="OpenGL ES 2.0+" />
  <img src="https://img.shields.io/badge/Windows-10_%2F_11-0078D6?style=for-the-badge&logo=windows11&logoColor=white" alt="Windows 10 和 11" />
  <img src="https://img.shields.io/badge/macOS-14%2B-000000?style=for-the-badge&logo=apple&logoColor=white" alt="macOS 14+" />
  <img src="https://img.shields.io/badge/Linux-glibc_2.35%2B-FCC624?style=for-the-badge&logo=linux&logoColor=black" alt="Linux glibc 2.35+" />
  <img src="https://img.shields.io/badge/PowerShell-Pebrel_prompt-5391FE?style=for-the-badge&logo=powershell&logoColor=white" alt="PowerShell" />
  <img src="https://img.shields.io/badge/License-GPL--3.0-1f6feb?style=for-the-badge" alt="GPL-3.0 许可证" />
</p>

<p align="center">
  <img src="https://img.shields.io/github/stars/Kuddev/pebrel?style=flat-square&color=ffd33d&logo=github" alt="Stars" />
  <img src="https://img.shields.io/github/forks/Kuddev/pebrel?style=flat-square&color=8957e5&logo=github" alt="Forks" />
  <img src="https://img.shields.io/github/last-commit/Kuddev/pebrel?style=flat-square&color=3fb950" alt="最近提交" />
  <a href="https://linux.do"><img src="https://img.shields.io/badge/%E5%8F%8B%E9%93%BE-linux.do-ffb003?style=flat-square&logo=discourse&logoColor=white" alt="linux.do" /></a>
</p>

<p align="center">
  <a href="README.md">English</a> · <strong>简体中文</strong>
</p>

---

<p align="center">
  <img src="docs/screenshots/nebula-top-tabs.png" alt="Pebrel 终端标签" width="1040" />
</p>

<p align="center">
  <img src="docs/screenshots/nebula-claude-session.png" alt="Pebrel 中运行的 Claude Code" width="1040" />
</p>

<p align="center">
  <img src="docs/screenshots/split-ai-workflows.png" alt="OpenCode、Claude Code 与 Codex 分屏工作流" width="1040" />
</p>

## 一个工作区

Pebrel（原名 Nebula）把本地 Shell、远程主机、文件和 AI 命令行工具放进同一个原生桌面工作区。
通过标签和分屏组织终端，关注各个 Agent 的活动，并直接在应用中阅读输出。

### 终端与会话

- 侧栏或顶部标签、可拖拽分屏，以及各窗格独立的工作目录。
- 保存工作区布局，可选恢复 AI 对话。在 WSL 与 Linux 中，Codex 的 hook 未上报 ID 时，
  可从当前会话元数据补全身份。
- Windows 下可选择后台驻留，让关窗后的会话继续运行。进程退出后重新接续已保存的对话
  是另一项功能，需要 CLI 支持恢复，并且有可用的会话身份。
- 历史记录与路径补全、自定义快捷键，以及集成的 Shell 提示符。

### SSH 与文件

- 保存主机、读取 SSH 配置别名、设置代理与跳板，支持私钥和交互式认证，并验证主机密钥。
- SFTP 文件浏览、上传下载、文件夹传输、进度显示与取消。
- 在终端旁浏览本地文件并执行 Git 操作。
- 复制 WSL 与 SSH 标签时保留已知工作目录。

<p align="center">
  <img src="docs/screenshots/ssh.gif" alt="原生 SSH 会话" width="1040" />
</p>

### AI CLI 工作流

- Claude Code、Codex 等可识别的 CLI 显示各自图标与活动状态；受支持的 hook 事件提供更准确的进度和待处理信号。
- 通知跟随来源窗格，点击即可返回对应终端。
- 已捕获的 Claude Code 与 Codex 回答可在阅读器中打开，支持 Markdown、公式、原文和本地图片预览。
- 剪贴板图片会保存为 PNG，并把路径插入本地、WSL 或 SSH 会话。终端内直接显示图片需要 CLI
  输出受支持的 OSC 1337 协议；附件缩略图取决于 CLI 自身。

<p align="center">
  <img src="docs/screenshots/ai-sidebar.png" alt="AI 活动侧栏" width="300" />
</p>

### 原生、可配置的界面

- GPU 加速的 GPUI 界面、浅色与深色主题、背景及不透明度设置。
- 应用图标配色与十一种界面语言，未翻译内容回退英文。
- 可搜索的设置、命令面板，以及支持检查和热重载的 Lua 配置；已有 TOML 配置仍可使用。
- Markdown 文档标签与原生数学公式排版，不依赖 WebView。

<p align="center">
  <img src="docs/screenshots/native-math-rendering.png" alt="原生公式排版" width="1040" />
</p>

<p align="center">
  <img src="docs/screenshots/themes.png" alt="应用主题" width="1040" />
</p>

## 下载

从 **[最新发布](https://github.com/Kuddev/pebrel/releases/latest)** 选择对应安装包。

| 系统 | 架构 | 安装包 |
| --- | --- | --- |
| Windows 10 1809+ / 11 | x64 | `.exe` 安装器、`.zip` 便携包 |
| Linux Preview，glibc 2.35+ | x64 | `.AppImage`、`.deb`、`.tar.gz` 便携包 |
| macOS Preview，部署目标为 14+ | Apple Silicon / arm64 | `.dmg` |
| macOS Preview，部署目标为 14+ | Intel / x64 | `.dmg` |

安装包统一采用 `Pebrel-v<版本>-<系统>-<架构>` 命名，Windows 安装器以 `-setup.exe` 结尾。
Windows 为正式版，Linux 与 macOS 文件名在扩展名前带有 `-preview` 标记。
同时提供旧名称的兼容安装器，供旧版自动更新客户端使用。

Windows 可运行安装器，或解压 ZIP 后启动 `pebrel.exe`，保留随包目录结构。
Linux 可安装 DEB，或为 AppImage 添加执行权限。macOS 打开对应 DMG，把 Pebrel 拖入“应用程序”。
采用临时签名的 macOS 包首次启动可能需要在“系统设置 > 隐私与安全性”中选择“仍要打开”。
原生 macOS CI 运行在 macOS 15 上，部署目标并不代表每个较早系统版本都已通过运行验证。

系统托盘驻留、全局快速终端热键、自动配置本地 AI hook 和自动安装更新目前由 Windows 提供，
Linux 与 macOS 尚未提供这些集成。平台要求和旧版 Nebula 升级步骤见[安装说明](INSTALL.md)。

## 配置

```sh
pebrel config init --language zh-CN
pebrel config check
```

生成的 Lua 配置使用 `require 'pebrel'` 与 `pebrel.config_builder()`。
重载失败时继续使用上一份有效配置。设置与示例见 [Lua 配置指南](docs/lua-configuration.md)。

## 构建

安装项目固定的 Rust 工具链与对应平台依赖后运行：

```sh
cargo build --release --locked -p nebula --bin pebrel --features gpui-shell
```

内部 Cargo 包名仍为 `nebula`，应用与命令名为 `pebrel`。GPUI 是正式产品界面，
旧渲染器仅在显式启用 `legacy-shell` 时使用。

## 联系方式

邮箱：[fickleheartedkeys@163.com](mailto:fickleheartedkeys@163.com)

## 致谢

Pebrel 基于 [Alacritty](https://github.com/alacritty/alacritty)、
[GPUI](https://github.com/zed-industries/zed) 与
[gpui-component](https://github.com/longbridge/gpui-component) 构建。
终端使用 Maple Mono 字体，原生公式使用 Latin Modern Math。
上游版权与许可证声明保留在 `THIRD-PARTY-NOTICES` 和 `licenses/` 中。

## 友情链接

- **[linux.do](https://linux.do)** - 新的理想型社区。

## ⭐ Star History

<a href="https://star-history.dera.page/#Kuddev/pebrel&type=date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://star-history.dera.page/svg?repos=Kuddev/pebrel&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://star-history.dera.page/svg?repos=Kuddev/pebrel&type=Date" />
    <img alt="Star History Chart" src="https://star-history.dera.page/svg?repos=Kuddev/pebrel&type=Date" />
  </picture>
</a>
