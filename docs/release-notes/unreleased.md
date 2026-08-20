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
- Fixed the GPUI Windows blur switch retaining Acrylic after it was disabled.
  Nebula now applies an explicit zeroed `ACCENT_DISABLED` policy when turning
  blur off, restores the Acrylic policy when turning it on, clears stale DWM
  system backdrops, and flushes composition in both directions.
- Fixed the shared Files panel losing the active pane after tab/pane switches
  or manual tree navigation. WSL `/mnt/<drive>` paths map to the host drive,
  while `/`, `/home`, `/etc`, and other guest paths are enumerated inside the
  selected distribution. Refresh now resumes following the active pane, and
  timed-out WSL workers can no longer disable later refreshes.

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
- 修复 GPUI Windows 壳关闭背景模糊后仍残留 Acrylic 的问题。关闭时现在明确
  写入全零 `ACCENT_DISABLED` policy，开启时恢复 Acrylic policy；两向切换都
  清理遗留 DWM system backdrop 并刷新合成状态。
- 修复共享 Files 面板在切换 tab/pane 或手动浏览目录后不再跟随当前 pane 的
  问题。WSL `/mnt/<盘>` 映射到宿主盘，`/`、`/home`、`/etc` 等来宾路径则在
  对应发行版内枚举；刷新按钮会恢复跟随当前 pane，WSL 工人超时也不会再让
  后续刷新永久失效。

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
