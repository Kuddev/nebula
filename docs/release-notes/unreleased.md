# Unreleased

This file contains changes that are not part of a numbered release yet.

## English

### Added
- Added an application icon picker with 25 color palettes and light/dark previews.
- Added an opt-in terminal network proxy setting. On Windows, new local sessions inherit the system HTTP/HTTPS proxy when enabled; existing sessions keep their environment.
- Added per-host SSH proxy and jump-host options, with separate proxy credentials and a connection-route preview.
- Added an answer reader for captured Claude Code and Codex responses, with Markdown, formulas, source-text mode, and local image previews.

### Fixed
- `Ctrl+C` copies and clears a terminal selection, and still sends an interrupt when no selection is present. Text inputs keep their own copy behavior.
- `Ctrl+Backspace` deletes a word in the supported legacy and kitty keyboard paths, including the managed PowerShell prompt.
- Running tab indicators keep animating when another window has focus; hidden or minimized windows stop requesting animation frames.
- File-name search discards results from an old root, updates after creates, renames and deletes, and handles canonical file-watcher paths without losing the visible directory root.
- Multiline confirmation text is measured with wrapping so longer messages fit the shared confirmation dialog.

### Improved
- Made Titanium the default application icon and adjusted small-size proportions, prompt strokes, and transparent edges.
- Adopted Pebrel as the display name in the application, notifications, and installer. Local Pebrel-named packages retain `nebula.exe`, existing configuration paths, installer identity, and the Nebula update feed; this is not a new published release.
- File-name indexing applies incremental updates and recovers from watcher queue overflow with a rescan; editing a file's contents alone no longer rebuilds its name entry.
- Improved terminal formula parsing and rendering for streamed agent output, while preserving the original text when a formula cannot be rendered.
- Added native platform adapters and preview packaging instructions for Linux and macOS. Native build, test and package validation remains a separate requirement before publishing those previews.
- Updated the public project and installation guides to use Pebrel consistently, while retaining the existing repository, command, configuration, and download identifiers.
- Added contribution, architecture, and translation guides with explicit compatibility and review requirements. GitHub branch protection still requires separate server-side setup and verification.

## 中文

### 新增
- 新增应用图标选择器，提供 25 款配色及浅色、深色背景预览。
- 新增默认关闭的终端网络代理设置。Windows 下启用后，新建本地会话继承系统 HTTP/HTTPS 代理；现有会话保留原有环境。
- SSH 主机新增独立的代理与跳板配置，支持单独保存代理凭据并预览连接路线。
- 为已捕获的 Claude Code 和 Codex 回答新增阅读视图，支持 Markdown、公式、原文模式和本地图片预览。

### 修复
- 终端中 `Ctrl+C` 会复制并清除选区，没有选区时仍发送中断；文本输入框保留自身的复制行为。
- `Ctrl+Backspace` 在支持的传统和 kitty 键盘路径中按词删除，并适用于受管理的 PowerShell 提示符。
- 其他窗口获得焦点后，运行中的标签指示器继续播放动画；窗口隐藏或最小化时停止请求动画帧。
- 文件名搜索丢弃旧目录的结果，在创建、重命名和删除后更新，并正确处理文件监听器返回的规范路径，保留用户看到的目录根。
- 多行确认文案按换行后的尺寸测量，使较长消息能完整放入共享确认框。

### 改进
- 将钛银设为默认应用图标，并调整小尺寸比例、提示符笔画与透明边缘。
- 应用、通知和安装器采用 Pebrel 展示名称。本地 Pebrel 命名包保留 `nebula.exe`、现有配置路径、安装标识及 Nebula 更新源；这不代表新的已发布版本。
- 文件名索引采用增量更新，监听队列溢出时通过重新扫描恢复；仅编辑文件内容不再重建对应名称条目。
- 改进流式 Agent 输出中的终端公式解析与绘制，无法渲染时保留原始文本。
- 新增 Linux 和 macOS 原生平台适配及预览打包说明；发布这些预览前仍需单独完成原生构建、测试和打包验证。
- 公开项目说明与安装指南统一采用 Pebrel 名称，同时保留现有仓库、命令、配置与下载标识。
- 新增贡献、架构和翻译指南，明确兼容性与审查要求；GitHub 分支保护仍需单独在服务端配置并核验。

## Contributors
- [@Sakyvo](https://github.com/Sakyvo): terminal network proxy, conditional `Ctrl+C` copy, and word deletion with `Ctrl+Backspace` / 终端网络代理、`Ctrl+C` 按选区复制及 `Ctrl+Backspace` 按词删除。
