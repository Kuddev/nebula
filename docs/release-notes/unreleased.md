# Unreleased

This file contains changes that are not part of a numbered release yet.

## English

### Added
- Added an application icon picker with 25 color palettes and light/dark previews.
- Added an opt-in terminal network proxy setting. On Windows, new local sessions inherit the system HTTP/HTTPS proxy when enabled; existing sessions keep their environment.
- Added per-host SSH proxy and jump-host options, with separate proxy credentials and a connection-route preview.
- Added an answer reader for captured Claude Code and Codex responses, with Markdown, formulas, source-text mode, and local image previews.
- Added UI language choices for Traditional Chinese, French, German, Spanish, Brazilian Portuguese, Italian, Russian, Japanese, and Korean alongside English and Simplified Chinese. Initial coverage includes navigation, common actions, appearance, and network controls; untranslated text falls back to English.
- Added a settings search that opens the matching section and a confirmed Restore defaults action that backs up preferences while retaining saved hosts and other user data.
- Added the Antigravity product icon to terminal tabs and the sidebar, including its common launcher aliases, while retaining its original colors in light and dark themes.
- Recognized binary terminal confirmations can show Allow and Deny actions in a notification. The action is rejected if the prompt, session, or input state has changed, and is available only once for the captured request.

### Fixed
- `Ctrl+C` copies and clears a terminal selection, and still sends an interrupt when no selection is present. Text inputs keep their own copy behavior.
- `Ctrl+Backspace` deletes a word in the supported legacy and kitty keyboard paths, including the managed PowerShell prompt.
- Running tab indicators keep animating when another window has focus; hidden or minimized windows stop requesting animation frames.
- File-name search discards results from an old root, updates after creates, renames and deletes, and handles canonical file-watcher paths without losing the visible directory root.
- Multiline confirmation text is measured with wrapping so longer messages fit the shared confirmation dialog.
- Improved Antigravity activity and permission detection so old spinner output and ordinary prose mentioning approval do not keep an idle session marked as running or waiting for permission.
- Closing the last regular window and then quitting no longer replaces saved tabs and AI session identities with an empty snapshot. Closing all tabs explicitly still starts an empty workspace next time, and failed session writes are retried.
- SSH startup now waits for PTY and shell confirmation before showing Ready, supports cancellation during connection setup, and stops stalled startup stages within a bounded timeout. Unexpected disconnects retain the pane and its failure state; explicit shell exits still close normally.
- Duplicated SSH tabs retain the known remote working directory and quote it when starting the new remote shell.
- Modified Enter and ASCII shortcuts retain their negotiated keyboard-protocol modifiers, including Shift+Enter, Ctrl+Enter, and Alt+V. Keyboard-mode queries now report the active flags after a protocol reset.
- Runtime CLI responses and event subscriptions stop cleanly when the program reading their output closes its pipe, instead of opening a panic dialog. Other output errors are still reported.

### Improved
- Made Titanium the default application icon and adjusted small-size proportions, prompt strokes, and transparent edges.
- Adopted Pebrel as the display name in the application, notifications, and installer. Local Pebrel-named packages retain `nebula.exe`, existing configuration paths, installer identity, and the Nebula update feed; this is not a new published release.
- File-name indexing applies incremental updates and recovers from watcher queue overflow with a rescan; editing a file's contents alone no longer rebuilds its name entry.
- Improved terminal formula parsing and rendering for streamed agent output, while preserving the original text when a formula cannot be rendered.
- Added native platform adapters and preview packaging instructions for Linux and macOS. Native build, test and package validation remains a separate requirement before publishing those previews.
- Updated the public project and installation guides to use Pebrel consistently, while retaining the existing repository, command, configuration, and download identifiers.
- Added contribution, architecture, and translation guides with explicit compatibility and review requirements. GitHub branch protection still requires separate server-side setup and verification.
- Made the settings navigation more compact, kept Application first, labeled terminal options more clearly, and added matching navigation icons. Appearance settings now show font, cursor, interface, and background controls in groups with expandable help.
- Reduced the intensity of hover and selected backgrounds in light themes.
- Settings now appears as a selectable, closable item in the top tab layout. Switching tab layouts while Settings is open preserves the sidebar state, and closing the last terminal tab keeps Settings available until it is closed.
- SSH editing now offers common usernames alongside recent entries, keeps manual usernames available, and makes host icons and pin controls clearer. Connection screens show completed stages and failures more accurately.
- Terminal completion lists now scroll to reveal keyboard selections, support wheel scrolling and hover feedback, and clear stale list positions when the query changes.
- Completion, bell, and Agent attention notifications now follow their source pane, including after a tab moves to another window. Clicking a notification focuses that pane, and activity in one pane no longer suppresses another pane's notification.
- Contributors can run the shared source-size, dependency-direction, translation, and branding checks locally and in the architecture workflow. Cross-platform metadata checks are documented separately from native build and UI validation.

## 中文

### 新增
- 新增应用图标选择器，提供 25 款配色及浅色、深色背景预览。
- 新增默认关闭的终端网络代理设置。Windows 下启用后，新建本地会话继承系统 HTTP/HTTPS 代理；现有会话保留原有环境。
- SSH 主机新增独立的代理与跳板配置，支持单独保存代理凭据并预览连接路线。
- 为已捕获的 Claude Code 和 Codex 回答新增阅读视图，支持 Markdown、公式、原文模式和本地图片预览。
- 在英语和简体中文之外，界面语言新增繁体中文、法语、德语、西班牙语、巴西葡萄牙语、意大利语、俄语、日语及韩语。初版覆盖导航、常用操作、外观与网络控件，未翻译文案回退英文。
- 新增可跳转到匹配分区的设置搜索，以及需要确认的“恢复默认”操作；重置前备份偏好设置，并保留已保存主机等用户数据。
- 终端标签与侧栏新增 Antigravity 产品图标，支持常见启动别名，并在浅色和深色主题中保留图标原色。
- 可识别的终端二选一确认提示可在通知中显示允许与拒绝操作；提示、会话或输入状态变化后会拒绝过期操作，每个捕获的请求只能处理一次。

### 修复
- 终端中 `Ctrl+C` 会复制并清除选区，没有选区时仍发送中断；文本输入框保留自身的复制行为。
- `Ctrl+Backspace` 在支持的传统和 kitty 键盘路径中按词删除，并适用于受管理的 PowerShell 提示符。
- 其他窗口获得焦点后，运行中的标签指示器继续播放动画；窗口隐藏或最小化时停止请求动画帧。
- 文件名搜索丢弃旧目录的结果，在创建、重命名和删除后更新，并正确处理文件监听器返回的规范路径，保留用户看到的目录根。
- 多行确认文案按换行后的尺寸测量，使较长消息能完整放入共享确认框。
- 改进 Antigravity 的活动与权限提示识别，避免历史转圈输出或普通正文中提到审批时，把空闲会话误标为运行中或等待授权。
- 关闭最后一个普通窗口后再退出，不再用空快照覆盖已保存标签与 AI 会话身份；明确关闭全部标签后，下次仍从空工作区开始，保存失败会重试。
- SSH 启动现在等待 PTY 与 shell 确认后才显示就绪，连接准备期间可取消，停滞的启动阶段会在限定时间内结束；意外断连会保留 pane 与失败状态，明确退出 shell 时仍正常关闭。
- 复制 SSH 标签时保留已知远端工作目录，并在启动新远端 shell 时正确引用该路径。
- 带修饰键的回车与 ASCII 快捷键会保留协商键盘协议中的修饰信息，包括 Shift+Enter、Ctrl+Enter 和 Alt+V；协议重置后的键盘模式查询现在返回实际生效的标志。
- 读取输出的程序关闭管道后，Runtime CLI 响应与事件订阅会正常结束，不再弹出 panic 对话框；其他输出错误仍会报告。

### 改进
- 将钛银设为默认应用图标，并调整小尺寸比例、提示符笔画与透明边缘。
- 应用、通知和安装器采用 Pebrel 展示名称。本地 Pebrel 命名包保留 `nebula.exe`、现有配置路径、安装标识及 Nebula 更新源；这不代表新的已发布版本。
- 文件名索引采用增量更新，监听队列溢出时通过重新扫描恢复；仅编辑文件内容不再重建对应名称条目。
- 改进流式 Agent 输出中的终端公式解析与绘制，无法渲染时保留原始文本。
- 新增 Linux 和 macOS 原生平台适配及预览打包说明；发布这些预览前仍需单独完成原生构建、测试和打包验证。
- 公开项目说明与安装指南统一采用 Pebrel 名称，同时保留现有仓库、命令、配置与下载标识。
- 新增贡献、架构和翻译指南，明确兼容性与审查要求；GitHub 分支保护仍需单独在服务端配置并核验。
- 设置导航更紧凑，“应用”保持首项，终端选项名称更清晰，并配有含义匹配的图标；外观设置按字体、光标、界面和背景分组，提供可展开的详细说明。
- 降低浅色主题中悬停与选中背景的色彩强度。
- 顶部标签布局中，设置现在作为可选择、可关闭的标签显示；打开设置时切换标签布局会保留侧栏状态，关闭最后一个终端标签后仍可继续使用设置，直到关闭设置页。
- SSH 编辑器在最近使用项之外提供常用用户名，保留手动输入，并改进主机图标与置顶控件；连接界面更准确地显示已完成阶段和失败状态。
- 终端补全列表现在会滚动到键盘选中项，支持滚轮浏览与悬停反馈，并在查询变化后清除过期列表位置。
- 命令完成、响铃和 Agent 待处理通知现在跟随来源 pane，即使标签已移动到另一个窗口；点击通知会聚焦来源 pane，一个 pane 的活动不再抑制另一个 pane 的通知。
- 贡献者可在本地及架构工作流中运行统一的源码规模、依赖方向、翻译和品牌检查；跨平台元数据检查与原生构建、界面验证的边界分别说明。

## Contributors
- [@Sakyvo](https://github.com/Sakyvo): terminal network proxy, conditional `Ctrl+C` copy, and word deletion with `Ctrl+Backspace` / 终端网络代理、`Ctrl+C` 按选区复制及 `Ctrl+Backspace` 按词删除。
