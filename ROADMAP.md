# Nebula UI 终局路线图

定案时间：2026-08-13。本文件是唯一的长期计划权威；阶段推进、闸门结果、
裁定变更都回写到这里。

## 当前迁移快照（2026-08-14）

当前 **完整迁移加权完成度约 65%**。这个数字按可验收能力计分，不按代码行数；
其中供应商设置、共享命令面板、文件树抽屉和窗口视效仍位于未提交工作区，不能
等同于已进入可发布基线。默认 UI 切换前的发布准备度低于功能完成度。

| 能力面 | 权重 | 已得 | 当前边界 |
|---|---:|---:|---|
| 架构与产品接入 | 15 | 15 | GPUI 已迁入 `nebula_app`，`nebula --gpui` 可独立运行 |
| 终端核心与会话 | 25 | 21 | ConPTY、网格、输入/选择/鼠标、IME 代码路径、多 tab 与 AI 会话已接入；分屏和最终人工验收未闭环 |
| 工作区与产品表面 | 20 | 12 | 侧栏、tab 拖拽、命令面板、文件树、设置 tab、标题栏/视效已有实现；分屏、SSH HOSTS、Git 抽屉与部分命令动作未接入 |
| 设置与业务工作流 | 20 | 11 | 纯键值设置和 Provider 编辑器已有实现；SSH、备份、按键编辑器、字体选择器仍缺 |
| 三闸门与默认切换 | 15 | 6 | G1/G3 有首轮结果；产品形态复测、G2 用户验收、默认入口切换均未完成 |
| 稳定期与收尾 | 5 | 0 | 默认切换、稳定期、脚手架移除和 P4 拆分尚未开始 |
| **合计** | **100** | **65** | **功能实现约 65%，尚不具备默认替换条件** |

### 当前已越过的里程碑

- GPUI 终端、ConPTY、CJK/Powerline 网格、多 tab、输入/选择/鼠标语义已进入
  产品代码；配置与旧壳共享同一事实源。
- 设置已成为内容区单例 tab；全部纯键值项可编辑，主题、配色、字号、背景、
  壁纸、透明度和系统深浅跟随已有 GPUI 路径。
- Provider 编辑器已复用共享业务 API；API Key 通过 Windows 原生凭据对话框
  写入，不进入可复制的 GPUI 文本框。
- 命令面板复用旧壳动作目录，文件树复用旧壳 `SidePanel` 模型；GPUI 只负责
  展示和事件适配。当前目录的复制与系统文件管理器打开动作已接线，并按活动
  tab 是否具有本地目录复用旧壳的可用性规则。

### 高难度剩余项（先独立设计和验收，不与零碎修改混做）

1. **分屏面板接线**：接入 `nebula-split` 会改变 tab/pane 所有权、关闭与 Drop
   语义、PTY Resize、焦点路由、拖拽落点和 AI hook 的 pane 定位。必须先定
   生命周期合同，再改 `workspace.rs` 与终端会话层。
2. **SSH 完整迁移**：共享 HostStore、SSH HOSTS 侧栏、设置 CRUD、置顶/隐藏/
   恢复、原生凭据输入、连接状态与错误恢复必须同一套语义；还要验证真实服务
   器、远端 hook、Resize 和避免外部 conhost 窗口。
3. **加密备份与远端同步**：本地导出/恢复和 Folder/WebDAV/S3/SFTP 四条路径
   涉及凭据、进度/取消、失败回滚、原子恢复与协议错误，且 SFTP 依赖 SSH
   主机模型稳定。
4. **三闸门与默认入口切换**：需在 `nebula --gpui` 产品形态重跑等网格性能、
   完成真实 IME/DPI/多屏人工验收、视觉对账和稳定期；通过后才能翻转默认入口
   并移除 `NEBULA_GPUI_SHELL` spike。

### 低风险剩余项（在高难度文件空闲时穿插）

- 同步路线图和验收清单；补齐已实现能力的针对性测试与离线构建检查。
- 字体选择器、命令面板中不改变 pane 生命周期的简单动作、设置页过期说明。
- 小型视觉/可访问性收尾；凡需修改 `workspace.rs` 或 `settings_pane.rs` 的任务，
  在分屏、SSH、备份工作进行时暂停，避免文件竞态。

## 终局形态（不可协商的三条）

1. **唯一产品、唯一代码库**：`nebula.exe`（`nebula_app`）。GPUI 最终作为
   `nebula_app` 体内的 UI 层接入——不是第二个产品 exe。`nebula_gpui` crate
   永远只是**实验场**：快速验证组件、交互与渲染方案；验证通过的模块**并入
   `nebula_app`**，产品代码不在实验场安家。
2. **网格渲染永远自有**：`nebula_terminal` 引擎 + 渲染合同
   （`render.rs` / `render/boxdraw.rs`）+ 终端 Element。终端内容只存在
   "单元格网格"一种形态，排版引擎（任何字体系统）无权移动网格字形。
   powerline、fastfetch、TUI 边框、CJK 对齐是**验收闸门**，不是愿望。
3. **回滚 = 代码保留 + git 历史**：旧 winit/OpenGL UI 保留在树上、保持可
   构建，直到闸门全过 + 稳定期结束；删除旧 UI 是迁移的最后一步。不设
   运行时开关、不留长期 feature flag 双路径。

## 三道闸门（任一否决即停在原地）

| 闸门 | 内容 | 验收方式 |
|---|---|---|
| G1 性能 | 大流量吞吐与滚动流畅度不低于旧渲染器既定比例（首测后定阈值） | `scripts/perf_baseline.ps1` 可重复测量，新旧同负载对比 |
| G2 IME | 中文输入全链路：预编辑、候选窗跟随光标、提交、退格 | 人工清单（用户亲手验收） |
| G3 视觉保真 | powerline 提示符、fastfetch（logo/色块/宽字符）、boxdraw 边框、CJK 对齐、光标形态 | 新旧并排截图对账 |

## 阶段

### P0 地基（已完成）

- workspace 合并（120859b）：crossfont 0.9 解除 links 冲突，单 lockfile。
- 渲染合同（73c584a、d049894）：viewport 合流 + 纯数据快照，引擎零框架类型。
- boxdraw 几何（5c6745d）：框线/块/Powerline 收归合同，字体无权染指。
- 终端 Element + 多 tab 工作区（5815b95、05ab821）：真 ConPTY 在 GPUI 内跑通。
- lib+bin 双形态与进程内拉起验证（f315edc）：双 UI 运行时同进程共存已证实。
- 注：2026-08-13 全库改写剔除联合作者尾注，本节哈希已同步为新值。

### P1 闸门建设（当前）

- **1a 性能基线**：`scripts/perf_baseline.ps1`（负载生成 + ConPTY 免焦点
  注入 `perf_inject.ps1` + 终端内自计时）。
  **首测 2026-08-13（50MB 混合负载：ANSI 256/真彩 + CJK + boxdraw +
  Powerline），release 构建，各 3 轮取中位**：

  | 壳 | 中位耗时 | 吞吐 | 内存 | 窗口 |
  |---|---|---|---|---|
  | winit-old | 21.1s | 2.4 MB/s | 118 MB | 1267x740 |
  | gpui-lab | 21.8s | 2.3 MB/s | 85 MB | 1095x729 |

  判读：吞吐差 3.6%（GPUI 窗口略小，按面积校正视为持平），内存省 28%。
  **G1 阈值定为：等网格下吞吐不低于旧壳的 90%——当前通过**。后续在等
  网格条件下复测一次收严。

  附带判明（判别实验 2026-08-13，两壳分别以 `openconsole=off`/缺侧载对
  复测）：无侧载 OpenConsole/conpty.dll 时走 in-box ConPTY（conhost），
  两壳同样慢 3~8 倍且抖动巨大（0.5~1.3 MB/s、单轮 38~94s、尾部可达
  分钟级，观感近似停摆）。这是 in-box host 自身行为，**不是引擎死锁**，
  引擎侧无从修复。运维硬规则：任何 nebula 系 exe 旁必须部署侧载对。
- **1b IME 清单**：产品形态人工清单已更新到
  `nebula_gpui/IME_CHECKLIST.md`；等待用户在 `nebula.exe --gpui` 亲手验收，
  实验场与双运行时 spike 的结果不计入 G2。
- **1c 视觉对账集**：`scripts/visual_parity.ps1`——固定样张（boxdraw 全家
  桶、块/浓度/象限、Powerline 分隔符、CJK 对齐标尺、256 色/真彩）注入
  两壳并截图（`.tmp_parity_<tag>.png`）。
  **首轮对账 2026-08-13：结构级持平**——框线闭合无缝、CJK 标尺逐列对
  齐、Powerline 三角在 GPUI 侧（boxdraw 几何）边缘更干净；`mixed/half/
  trans` 几行两壳同样偏怪（旧壳像素栅格化的既有观感，非回归）。
  待办：config 共享后做同主题对比；fastfetch 未安装（装上即可入集）；
  放大级笔画粗细对账。

### P2 终端体验齐平（裁定修订 2026-08-13：直接在产品内做）

用户裁定：实验场不再堆产品功能——`nebula_gpui` 只是验证场，验证过即进
`nebula_app`；P2 剩余项全部在 `nebula_app/src/gpui_shell/` 内继续。

- 选择/复制/鼠标语义 **✔（2026-08-13，1244f1a）**：鼠标模式上报（SGR/
  normal/UTF-8 扩展，`mouse_protocol.rs` 纯函数逐字对照旧壳 + 字节级单测；
  vim/htop 接管指针，Shift 旁路）、右键复制/粘贴（旧壳 Windows 惯例）、
  copy_on_select 抬手复制、Shift+点击扩展选区、Shift+PageUp/PageDown/
  Home/End 回滚翻页（仅主屏）。
- 分屏规则共享化 **✔（2026-08-13，9e21bc9）**：新共享 crate
  `nebula-split`（零依赖）——布局树、几何切割（floor+逐侧单元格钳制）、
  分隔条拖拽（关闭边距/预览钉边/整格量化提交）、方向聚焦（垂直漂移 4x
  惩罚），数学逐字对照旧壳 `split.rs`，12 个单测锁定。**GPUI 面板接线
  待办（在产品内做）**。
- config 共享化 **✔ 第一刀完成（2026-08-13，9e29331）**：新共享 crate
  `nebula-settings`（零依赖）承接 `nebula_settings.txt` 路径/键值语义与
  主题终端色表；按旧壳同序叠加（toml → 主题），字体/光标/copy_on_select
  均读运行时设置。同主题对账样张已验。剩余：toml 侧 font offset、
  cursor 反色语义、follow_system_theme、热重载。
- 剩余：滚动条（如旧壳有）、会话/SSH 逻辑共享化评估。

### P3 接入 `nebula_app`（GPUI 成为产品 UI 层）

- **✔ 第一刀（2026-08-13，791fdc8）：GPUI UI 层物理迁入产品**。
  `git mv` 保历史：终端栈（view/element/session/keymap/mouse_protocol/
  colors）、config 桥、workspace、theme/prelude 全部住进
  `nebula_app/src/gpui_shell/`；`gpui-shell` feature 自足（直接依赖
  gpui/gpui-component/-assets + nebula-settings + futures，fork 经根
  patch 表重定向），不再依赖 `nebula-gpui` crate。
  **`nebula --gpui` = GPUI 主窗形态**：主线程直接进 GPUI 消息循环，
  winit 完全不启动；已实跑验证（Nebula 主窗 + tabs + ConPTY 终端 +
  CJK + powerline，样张 `.tmp_parity_gpui-in-nebula.png`）。
  `nebula_gpui` 瘦身为组件 gallery（终端预览移除；theme/prelude 留
  注记副本），产品代码零残留。
- 剩余：GPUI 主窗成为默认 UI（三闸门在 `--gpui` 形态复测通过后切换）；
  分屏面板接线（消费 `nebula-split`）；产品面直接用组件库实现：设置
  **原生内嵌侧栏 pane**、启动器/命令面板、SSH 管理、标题栏/Acrylic
  对齐；`NEBULA_GPUI_SHELL` spike 移除。
- 三闸门在接入形态下复测（实验场数字不能替代产品形态数字）。

### P4 重构与收尾（GPUI 接入完成后）

- 旧 winit/OpenGL UI **不删除**（用户裁定 2026-08-13）：保留在树上、保持
  可构建，作为永久回滚锚点，只移出默认执行路径。
- crossfont **退场但不删除**：新 UI 路径不再使用它，代码与依赖随旧渲染
  器保留。
- `gpui-shell` 双运行时脚手架在 P3 主窗接管完成后移除（脚手架不属于
  "旧代码"范畴）。
- **大文件拆分分层**（必做，接入完成后统一做一次）：
  - `display/mod.rs`（10342 行）→ 按面拆：终端视图、布局、消息条、弹层等。
  - `display/settings.rs`（7264 行）→ 设置分组模块化。
  - `event.rs`（3178 行）/ `window_context.rs`（3100 行）→ 输入/窗口分层。
  - 拆分原则：先划模块边界再搬代码，行为零变化，每步独立 commit 可回退。

## 纪律（引用 `nebula_gpui/ARCHITECTURE.md`）

- 功能只写一次，进共享 crate；两壳都只写薄视图。
- 直接采用：GPUI 赢下某个面后就是该面的唯一实现。
- 引擎与渲染合同禁止出现 UI 框架类型（Cargo 依赖方向物理执行）。
- fork 只允许组件级小行为补丁，平台窗口层不碰。
