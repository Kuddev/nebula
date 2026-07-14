# 当前任务（2026-07-14）

- [ ] 使用真实 SSH 服务器验证直连认证顺序、首次连接确认、输入输出、Resize 和实际连接耗时。
- [ ] 验证远端 `nebula-hook` 的部署/发现方式，确认 Claude、Codex、OpenCode 在 SSH Pane 内能触发本地通知。
- [x] 为 SSH 条目与 Tab 实现统一的原生右键菜单组件和短促开合动画；外部点击、Esc、普通按键和窗口失焦可自然释放轻量菜单。
- [x] SSH 菜单：连接、复制地址、编辑、删除；不再提供右键置顶。删除已补来源说明、二次确认、8 秒 Undo / Ctrl+Z、凭据延迟清除及设置页长期恢复入口。
- [x] Tab 菜单：复制标签页、左右分屏、上下分屏、重命名、关闭和颜色选择。
- [x] Tab 左侧窄光条只在用户明确设置自定义颜色后显示；默认不显示侧边条（按用户 2026-07-14 最新要求覆盖旧设计）。
- [x] 保存并恢复 Tab 自定义颜色及必要的会话元数据。
- [x] 自绘输入框统一支持 Ctrl+A / Ctrl+C / Ctrl+V；全选后输入/粘贴替换，选择有可见背景；隐藏密码仅允许粘贴，显示后才允许复制。
- [x] 侧栏按 `D:/下载/gemini-code-1784017517204.html` 收敛：TABS / SSH HOSTS 使用真实小字号层级，两个加号仅标题 hover 时显现，Tabs 下拉改为竖向三点。
- [x] `cargo check -p nebula` 与 `cargo test -p nebula` 通过（174 passed，0 failed；2026-07-14）。
- [x] 建立 `display/design_tokens.rs`（4px 空间阶梯、字号、控件尺寸、动画和统一状态枚举），菜单动画与侧栏层级已开始实际引用；SSH UI 状态类型已从 `display/mod.rs` 拆至 `display/ssh_ui.rs`。
- [ ] 继续把 `display/mod.rs` 中 SSH 方法/绘制、确认弹窗和其他大块职责拆到子模块，并逐屏迁移散落的局部数值到 design tokens。
- [ ] 完成 Debug 运行检查和真实窗口交互验收（hover-only 控件、菜单脱焦、删除/撤销/恢复、输入框快捷键和 DPI 字体观感）。
- [ ] 验收完成后重打 0.3.0 Release，并替换现有发布压缩包。

---

# Nebula 任务交接（2026-07-12 17:10 写于会话结束前）

> 交接给新对话。持久记忆里有同步副本（`nebula-task-handoff` 记忆），本文件更详尽。
> 已交付包：`target/release/nebula.exe`（16:59+，含下述两个已完成修复 + 诊断日志）。
> 不要为了检查代码反复关闭用户正在查看的 Nebula。只有在用户同意进行 Release 重打、且运行实例确实占用目标 exe 时，才单独说明后关闭对应实例。

## ✅ 本轮已完成（已在包里）

### 1. 标题/缩放文字"拥挤粘连+虚胖"— 已修
- 根因：crossfont::Size 单位是 **pt**，`Size::new(base_size.as_px() * scale)` 把 px 当 pt，字号多乘 96/72=1.333 → 字形比步进宽 33% → 标题粘成墨块。
- 修复：`glyph_cache.font_size = base_size.scale(scale)`（renderer/mod.rs `draw_doc_text` 内，~467 行）。
- 用户验证方法：md 标题/设置面板标题应字距正常且整体小一号（之前虚胖）；侧栏 SSH 提示真正变小（0.85× 之前实际渲染 1.13×）。

### 2. 鼠标选择"只能选整行/拖拽粒度错"— 已修（代码完成，待用户手感验证）
- 根因：`on_mouse_press` 中 click-state 被**双重推进**——chrome 块（原 ~1238）推一次更新时间戳，`ChromeHit::None` 落下后终端块（原 ~1401）再推一次，elapsed≈0 → 每次终端单击必然升级 DoubleClick/TripleClick → 词/行粒度选择与拖拽。
- 修复（input/mod.rs）：
  - 新增 `advance_click_state()`：press 入口调用**恰好一次**（WT `_numberOfClicks` 模型）。
  - 补 WT 位置约束：两次点击须落在系统双击矩形内（`GetSystemMetrics(SM_CXDOUBLECLK)/2`），时间用系统 `GetDoubleClickTime()`（Cargo.toml 已加 windows-sys feature `Win32_UI_Input_KeyboardAndMouse`）。
  - `Mouse` 新增 `last_click_pos` 字段（event.rs）。
  - Shift+点击 = 扩展现有选区（WT 行为），不再清空。
  - 测试 `input_delay` 引用改为 `multi_click_time()`；7 个 click 测试失败为**遗留**（mock display() unimplemented，input/mod.rs:2286 panic），非回归。

## 🧊 已沉淀暂缓：#6 抽屉/目录树 → TUI 历史刷新风暴（用户 07-12 指示"别死磕，先做别的"）

已埋诊断日志（`nebula_link_log` → `%APPDATA%\Nebula\nebula_debug.log`）：
- display/mod.rs handle_update HUD 处：`grid_resize <cols>x<lines> px=.. pad_x/pad_r/pad_y cell=.. drawer=.. sidebar=.. reserved=..`
- window_context/split.rs `apply_view_pty`：`pty_resize cols=.. lines=.. view_px=..`
复现脚本：`scripts/drawer_repro.ps1`（启动+自动点抽屉按钮各一次，间隔 2s；FindWindowW 按 title "nebula" 找不到窗口那次是 race，重跑即可）。

**⚠️ 上轮误判已推翻（07-12 二次侦察）：**
> 上轮写"异常 B：抽屉状态自发振荡、无人点击也振荡"是**错的**。真相：日志里每对 84↔120 的 grid_resize 前后都紧跟一条 `link_release`——那是 `drawer_repro.ps1` 脚本的模拟点击，不是自发的。**根本没有自发振荡**，别再往"每帧派发 toggle / session 恢复反复 toggle"方向查了。

**真正的实锤（重新读日志得出）：单击一次 = 开+关两次 toggle。**
```
[..419.485] grid_resize 84x20  ... drawer=462   ← 开
[..419.485] pty_resize cols=84
[..419.616] link_release                         ← 脚本第 1 次点击的 release
[..419.812] grid_resize 120x20 ... drawer=0     ← 立刻又关（约 300ms 内）
[..419.813] pty_resize cols=120
```
每次点击都产生"开(drawer=462)→约300ms→关(drawer=0)"成对 resize，即**一次物理点击触发了两次 toggle_side_panel**。press 和 release 各调一次是头号嫌疑：
- `on_mouse_press` 顶栏按钮路径 input/mod.rs:1368-1381（`ChromeHit::PanelFiles/PanelGit → toggle_side_panel`）
- `on_mouse_release` 里是否有对称的第二次分发（tab-drag `end_tab_drag`→Click、或 chrome 命中在 release 再跑一遍）→ **下一步只需读 on_mouse_release 全程 + arm/end_tab_drag，确认 press 已 return 后 release 不该再命中同一按钮**。
- 另一嫌疑：`ChromeHit::PanelFiles` 命中矩形与抽屉打开后某元素重叠，开抽屉后同一坐标又命中"关"。

**异常 A（独立问题，仍未解）：`cell=13x40` — 行高 40px 偏大**（宽 13 正常约配 26 高）。
- 计算链已定位：display/mod.rs:4936 `compute_cell_size` → `(metrics.line_height + offset_y).floor()`；metrics 来自 glyph_cache.rs:112 `rasterizer.metrics(key, font.size())`，font.size() 已 `.scale(scale_factor)`（display/mod.rs:1444）。
- 存疑：cell_width=13 说明 average_advance 未被 DPI 双乘，但 line_height=40 偏大 → line_height 单独异常。**下一步**：加日志打印 `metrics.average_advance / metrics.line_height / offset` 原始值，看是 crossfont 返回的 line_height 本身比例失调，还是 config.font.offset.y 被设过大。注意用户设置 `powerline=1`，排查是否 powerline/字体 fallback 拉高行盒。
- 用户运行时设置（`%APPDATA%\Nebula\nebula_settings.txt`）：theme=SilverLight, shell=powershell, powerline=1, keep_session=0, background=#ffffff。

**修复原则（留给复工时）：** press 已处理的点击，release 必须 early-return，不得二次 toggle；toggle 目标状态幂等（相同 open 值不 set_dimensions）。PTY resize settle 机制已就绪（window_context.rs:1931 leading-edge 300ms + 150ms trailing）——源头去重后风暴应自愈。

## 📋 待办（按用户优先级：先 bug 后新功能）

### #7 SSH 弹外部 conhost 黑窗（bug，未开工）
- 现象：点 SSH host，`C:\Windows\System32\OpenSSH\ssh.exe` 独立黑窗弹出，密码在外部输入。
- 根因判断：`spawn_tab_ssh`（window_context.rs:816）经 `nebula.exe ssh <host>`（main.rs:108 → `ssh::run(options.args)`，ssh.rs）。nebula.exe 是 GUI 子系统进程、无控制台可继承 → 子进程 ssh.exe 被分配新可见 conhost。
- 下一步：读 `ssh::run`；候选修法：包装进程入口 AttachConsole/AllocConsole（隐藏窗口）；或 PTY 直接跑 ssh（需另路注入远端集成）；或 CreateProcess 时正确传递 pseudoconsole。

### #2-#5 执行器下拉 + WT 同款默认 Shell 设置（✅ 07-12 完成，含 #5 默认 shell picker）
- 执行器下拉已交付：`shell_detect.rs`（Tabby 探测逻辑移植，缓存于 `Display::nebula_detected_shells`）、chevron 按钮（`ChromeHit::NewTabMenu`）、palette profiles-only 菜单、`TabRequest::NewShell`。
- #5 默认 Shell（WT parity）已交付，全链路：
  - 存储：`shell=<id>` 支持富 id（cmd/pwsh/nu/wsl:X）。`NebulaRuntimeSettings.shell_id: Option<String>` 原样读写（settings.rs），2 值枚举 `NebulaShell` 继续跟踪 PTY 集成执行器族（powershell/bash 的提示符注入基座）。
  - 生效：`spawn_tab` → `default_shell_override()`（window_context.rs:800）经 `shell_detect::resolve_id` 解析；powershell/bash 族返回 None 走 PTY 自有 bootstrap。
  - UI：设置页"默认 Shell"行点击 → `open_default_shell_picker()`（palette 置 `picking_default`，confirm 返回 `SetDefaultShell` 而非 launch）→ `set_default_shell()` 持久化。行值显示 icon+名称（`display_name_for_id`，无 IO，每帧安全）。旧 `cycle_shell` 2 值轮换已删除。
  - palette 行现携带 `(icon, label, hint)` 三元组，shell 行画 Nerd Font 图标（icon_for_id）。
  - 顺手修复：keyboard.rs 重复 `LaunchShell` arm、input/mod.rs 重复 `NewTabMenu` arm（均为前次会话残留的 unreachable pattern）。
- 验证：cargo build 0 error；测试 137 passed / 8 failed（8 个失败与干净基线完全一致 = 遗留 mock 问题，palette 测试 9/9 过）。

### #2-#5 原侦察记录（已实现，留档）
- 需求：tabs 加号旁下箭头 → 菜单列执行器（PowerShell/pwsh/CMD/Git Bash/WSL 发行版）→ 点击用该 shell 开新 tab。参考 Tabby（源码 D:\temp_build\tabby）。**不造轮子**。
- 现成管线：`Profile{name,command,args,cwd}`(config/ui_config.rs:684)、`TabRequest::NewProfile(usize)`→`spawn_tab_profile`(window_context.rs:787)→`spawn_pane_detached_with(shell override)`(1078)；右键加号已弹菜单=命令面板 profiles-only（`open_profile_menu` display/mod.rs:2162；palette `LaunchProfile(i)`→keyboard.rs:539）。
- 检测逻辑（照搬 Tabby tabby-electron/src/shells/*.ts，已读）：
  - PowerShell：知名路径顺序探测 `%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe` → `%ProgramFiles%\PowerShell\7\pwsh.exe` → `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`，args `-nologo`
  - pwsh：注册表 HKLM `SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\pwsh.exe` 默认值
  - CMD：`cmd.exe`
  - Git Bash：HKLM→HKCU `Software\GitForWindows` `InstallPath` → `{path}\bin\bash.exe --login -i`
  - WSL：HKCU `Software\Microsoft\Windows\CurrentVersion\Lxss` 枚举子键读 `DistributionName` → `%windir%\system32\wsl.exe -d <name>`；无 Lxss 但 system32\bash.exe 存在 → "WSL/Bash on Windows"
  - 注册表：windows-sys feature `Win32_System_Registry` 已在依赖（或加 winreg，Cargo.lock 已有）
- UI：chrome.rs `ChromeTabLayout` 加 chevron rect（plus 是 TABS header 右端 s(20) 方块，chrome.rs:299；chevron 放其左），`ChromeHit::NewTabMenu`，hit chrome.rs:429 旁，hover chrome.rs:860 旁，图标 chrome.rs:1010（`draw_centered_icon` + codicon chevron-down）。
- 路由：菜单名单 = 检测项+config.profiles 合并（一个函数三处共用）；新 `TabRequest::NewShell{name,shell}`，keyboard.rs:539 做 index→请求映射；Ctrl+Shift+数字（keyboard.rs:273）保持 config.profiles 语义。

### #9 设置改为内容区 tab（新功能，用户已细化）
- 设置作为 tabs 列表一项，点设置按钮 → 设置页在中间内容区打开（VS Code 风格，废除悬浮模态）。
- 设置 tab 激活时：右上角文件/git 抽屉按钮**隐藏**、抽屉功能禁用。
- 参考先例：OpenDoc 查看器 tab（TabEntry.doc 字段、markdown_view）是现成的"非终端 tab"。

### #10 快捷键扩充 + 一键跳转（新功能）
- 对齐 WT；核对 Alt+1..9 / Ctrl+1..9 切 tab 是否真实生效（设置页写着有）；[[keyboard.bindings]] 自定义已有配置系统，扩默认表+文档化；"点击一下自动连接/启动"与执行器下拉、SSH hosts 一键行合并交付。

### #11 大文件拆分（重构，用户点名"顺手做"）
- display/mod.rs ~4900 行、input/mod.rs ~2500、event.rs ~2900、window_context.rs ~2000。沿 chrome.rs/settings.rs/split.rs 子模块先例按职责拆。
- 顺带修 8 个遗留测试失败（input 点击测试的 mock display() unimplemented → 解耦 chrome 命中）。

## 环境与流程
- 效果验证：用户装包截图反馈；渲染问题用"量截图"法（列投影+自相关量步进/墨宽，见 nebula-render-crispness 记忆第 6 条）。
- 诊断日志常开：`%APPDATA%\Nebula\nebula_debug.log`（grid_resize/pty_resize/link_press 等）。修完 #6 后考虑移除或门控这两条 resize 日志。
- dist 打包需带 conpty.dll + OpenConsole.exe（target/release 已有，7/2 版本）。
- 上轮遗留：设置面板标题层级（真粗体+字号梯度已上车，看效果再调）。
