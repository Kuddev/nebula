# Unreleased

This file contains changes that are not part of a numbered release yet.

## English

### Fixed

- Fixed Codex treating Shift+Enter as ordinary Enter in Nebula on Windows. The
  terminal now enables ConPTY Win32 input mode, tracks DECSET 9001, and encodes
  the corresponding Win32 key records.
- Fixed the installer's fallback and numeric version metadata still identifying
  old 0.6.0 builds instead of the current 0.9.0 application version.
- Fixed PowerShell sessions created from the three-dot shell menu, and sessions
  with Powerline disabled, losing Nebula's OSC prompt/command state until the
  first interactive conversation. The OSC bridge is now installed before the
  first prompt so Codex/CLI detection and the right-side activity state are
  available immediately.
- Fixed PowerShell profiles created through the alternate profile path missing
  the Nebula command-completion integration.
- Disabled the `fmt` command from Nebula's shell-injected command surface.
- Fixed the GPUI Windows background-blur switch doing nothing at runtime. The
  window effects were applied inside `handle.update` while that same window was
  already borrowed by the enclosing update, so the call always failed with
  `window not found` and the error was discarded — blur only ever took effect at
  startup, and once on it could not be turned off. Effects are now applied on a
  deferred pass and update failures are logged.
- Fixed dragging the opacity sliders stuttering badly. Dragging no longer
  reloads settings from disk, rebuilds every terminal's fonts and palette,
  rebuilds the whole theme, re-bakes the wallpaper texture, or round-trips to
  DWM on every event; it updates the shell alpha directly and persists once
  after the drag settles. The blocking `DwmFlush` was removed and window-level
  blur is only touched when the blur state actually changes.
- Fixed the WSL file tree showing "此目录为空" for guest directories. A fresh
  `wsl.exe -d <distro> -- find` call takes about 7.5s on its cold path even when
  the same distribution already has an interactive terminal running, so the 6s
  budget always expired on the first snapshot, the worker was killed, and the
  empty result was rendered as an empty directory. The budget is now 20s.
- The Files panel no longer re-scans the tree on a timer. It previously rebuilt
  the whole snapshot (a WSL subprocess plus three git subprocesses) every 4
  seconds, which on WSL left the panel flipping between "正在读取目录…" and an
  empty list. Refreshes are now driven by directory changes, the refresh button,
  and finished git operations only.
- Fixed windows staying hidden when the tray was disabled while they were
  minimized to it. The reveal path applied its effect inside `handle.update` on
  an already-borrowed window, so it always failed silently while
  `window_hidden` had already been cleared — leaving an app with no window and
  no tray icon that could not restore itself.
- Fixed the shared Files panel losing the active pane after tab/pane switches
  or manual tree navigation. WSL `/mnt/<drive>` paths map to the host drive,
  while `/`, `/home`, `/etc`, and other guest paths are enumerated inside the
  selected distribution. Directory rows are published before the slower WSL
  VCS probe, refresh resumes following the active pane, and timed-out workers
  can no longer disable later refreshes.

### Improved

- Added a default-selected installer task that adds Nebula to the current
  user's PATH. The installer also registers `nebula.exe` with Windows App Paths
  for Win+R, preserves the Explorer directory context menus, and removes only
  its own PATH entry during uninstall.
- Shell injection is now selected by shell family. PowerShell uses its own
  startup/status path; `cmd.exe` uses only the compatible minimal status path;
  Nushell keeps its native completion behavior and is not replaced by Nebula's
  completion script; other shells use the fallback OSC path where supported.
- Improved Windows mixed-DPI dragging between monitors by separating the
  native move transaction from the final DPI/size commit. The Windows 11
  backend must use the native `WM_DPICHANGED` target rectangle, while the
  Windows 10 compatibility reposition path remains gated to older builds.
- Improved cross-monitor drag performance by coalescing intermediate DPI,
  physical-size, font, and UI updates until the native drag settles. Ordinary
  native messages remain on the fast path.
- Matched the expanded sidebar toggle's selected background to the softer
  surface color used by the legacy shell without changing other selected
  buttons globally.

## 中文

### 修复

- 修复 Windows 下 Codex 把 Shift+Enter 当作普通 Enter 的问题。终端现在启用
  ConPTY Win32 输入模式、跟踪 DECSET 9001，并编码对应的 Win32 按键记录。
- 修复安装器兜底版本与数字文件版本仍显示旧 0.6.0、没有跟随当前 0.9.0
  应用版本的问题。
- 修复通过三个点创建的 PowerShell，或关闭 Powerline 后的 PowerShell，在
  第一次实际对话前丢失 Nebula OSC 提示符/命令状态的问题。现在启动阶段就
  安装 OSC 桥接，因此 Codex/CLI 识别和右侧活动状态会在第一条提示符出现
  时可用，不需要先发送一轮对话。
- 修复通过备用 profile 路径创建的 PowerShell 没有命令补齐的问题。
- 禁用 shell 注入命令面板中的 `fmt` 命令。
- 修复 GPUI Windows 壳的背景模糊开关在运行中点了没反应的问题。窗口视效原先在
  `handle.update` 里应用，而那一刻这个窗口已经被外层 update 借出，调用必然返回
  `window not found`，错误又被丢弃——于是模糊只在启动时生效，一旦开着就再也
  关不掉。现在改为延迟一帧统一应用，且 update 失败会留日志。
- 修复拖动不透明度滑块严重卡顿的问题。拖拽过程不再每个事件都重新读设置文件、
  重建每个终端的字体与调色板、重建整套主题、重烘焙壁纸纹理、以及和 DWM 往返；
  改为直接更新壳色 alpha，停手之后落盘一次。阻塞式 `DwmFlush` 已移除，窗口级
  模糊只在模糊态真的改变时才动。
- 修复共享 Files 面板在切换 tab/pane 或手动浏览目录后不再跟随当前 pane 的
  问题。WSL `/mnt/<盘>` 映射到宿主盘，`/`、`/home`、`/etc` 等来宾路径则在
  对应发行版内枚举；目录行会先于较慢的 WSL VCS 探测发布，刷新按钮会恢复
  跟随当前 pane，WSL 工人超时也不会再让后续刷新永久失效。

### 改进

- 安装器新增默认勾选的当前用户 PATH 任务，并通过 Windows App Paths 注册
  `nebula.exe` 供 Win+R 直接启动；资源管理器目录右键菜单继续随包提供，卸载
  时只移除安装器自己加入的 PATH 条目。
- 按 shell 家族选择注入逻辑：PowerShell 使用专用启动/状态路径；`cmd.exe`
  只使用兼容的最小状态路径；Nushell 保留自身原生命令补齐，不被 Nebula
  的补齐脚本替换；其他 shell 在支持时使用通用 OSC 回退路径。
- 改进 Windows 混合 DPI 多显示器拖动：把原生拖动阶段与最终 DPI/尺寸提交
  分离；Windows 11 使用原生 `WM_DPICHANGED` 目标矩形，Windows 10 的兼容
  重定位逻辑只对旧版本生效。
- 改进跨显示器拖动性能：中间产生的 DPI、物理尺寸、字体和 UI 更新合并到
  原生拖动结束后执行，普通系统消息仍走快速路径。
- 左上角侧栏折叠按钮展开时改用与旧壳一致的柔和 surface 选中色，不全局
  改动其他 selected 按钮的主题状态。

## Verification

The PowerShell/OSC changes need first-prompt checks for all profile creation
routes. The mixed-DPI change still requires a real Windows two-monitor matrix,
including Windows 11 150% -> 125% and the reverse direction. A compile check
alone is not sufficient to claim that the physical monitor seam is fixed. The
WSL changes still require guest-path checks for both `/mnt/<drive>` and native
Linux repositories, and the blur change requires a live Windows DWM toggle
check in both directions.
