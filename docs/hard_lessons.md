# 疑难杂症档案(Hard Lessons)

记录本项目所有「重大疑难杂症」:症状诡异、排查跨越多层、结论反直觉的问题。每案必含
**症状 → 排查路径 → 根因 → 修复原理 → 验证方式 → 教训**。目的不是记流水账,而是让同类
问题下次出现时,能在十分钟内对号入座。

收录标准:跨层(应用/协议/宿主/OS 至少两层)、或排查超过半天、或结论会被直觉否定。
普通 bug 进 CHANGELOG,不进这里。

---

## 案例 1:Claude Code 里 Esc 无法打断,Codex 却正常(2026-08-09)

### 症状

在 Nebula 里跑 Claude Code(cc),生成过程中按 Esc 毫无反应,无法打断对话;同一窗口里
Enter、Ctrl+Enter、字母、方向键全部正常。诡异点:**Codex CLI 的 Esc 完全正常**——同一台
机器、同一个终端、同一个 ConPTY。

### 排查路径(三个实验,逐层收窄)

1. **VK 探针**(`[Console]::ReadKey`,走 `ReadConsoleInput`,即 Codex/crossterm 的读法)
   在 Nebula 里收到 `key=Escape char=0`——Esc **确实到达了 PTY**,排除 Nebula chrome 层
   吞键;但 KEY_EVENT 的 `UnicodeChar=0`,而真实键盘应为 27。
2. **node 探针**(`stdin.setRawMode(true)` hex dump,即 cc/Ink 的读法)在 Nebula 里:
   夹在字母中间的 Esc 一个字节都没出现,字母全到。
3. **WriteConsoleInput A/B 注入**(绕开整个键盘层,直接向 console 写两种 KEY_EVENT):
   - Nebula 的 sideload OpenConsole 1.22:`uChar=0` 的 Esc **被丢弃**,`uChar=27` 的正常
     翻译成 `\x1b`;
   - 系统 conhost(10.0.22621):两种都能翻译出 `\x1b`(按 VK 显式映射兜底)。

### 根因

链条上三个事实叠加:

1. Nebula 的 win32-input-mode 编码器(`nebula_app/src/input/terminal_input.rs`)对所有
   非字符键硬编码 `Uc=0`,发出 `CSI 27;1;0;1;0;1 _`;winit fork 的 `win32_unicode_char`
   同样 `_ => 0`。而真实 Windows 键盘的 KEY_EVENT:Esc=27、Enter=13、Tab=9、Backspace=8
   (Windows Terminal 的编码器也发真值;wezterm 因内部把 Esc 表示为 `Char('\x1b')` 天然带 27)。
2. sideload 的 OpenConsole 1.22 在「INPUT_RECORD → VT 字节流」翻译层,对 VK_RETURN/
   VK_TAB/VK_BACK 有不依赖 uChar 的显式映射,**唯独 VK_ESCAPE 依赖 uChar**——uChar=0 的
   Esc 翻译产出为空。这精确解释了「只有 Esc 坏」。
3. 读端分两派:**Codex(crossterm)用 `ReadConsoleInput` 按 VK 识别**,uChar=0 无感;
   **cc(node/Ink)只消费翻译后的字节流**,翻译层丢了它就永远收不到。

### 修复原理

让 win32-input 记录忠实模拟真实 KEY_EVENT——协议(WT `#4999` spec)的本意就是「把原生
事实原样搬运」。`terminal_input.rs` 的非字符键分支改为:

- 优先 `text_with_all_modifiers()`(WM_CHAR 真值,自然覆盖 Ctrl+Enter→0x0A、
  Ctrl+Backspace→0x7F 的修饰变体);
- 无文本时(key-up 没有 WM_CHAR)回落显式表:Esc=0x1B、Enter=0x0D、Tab=0x09、BS=0x08;
- 修饰键/方向键/F 键保持 0,与真实键盘一致。

不修 OpenConsole、不关 9001(实测向已开 9001 的 ConPTY 混发 legacy 裸 `\x1b` 同样会丢,
降级不是出路),也不做任何 shell/程序特判。

### 验证

- 单元测试锁表值(`control_keys_fall_back_to_their_key_event_character`);
- VK 层:修复后 `key=Escape char=27`、`key=Enter char=13`;
- 端到端:PostMessage 注入 a/Esc/b/q,node 按序收到 `61 1b 62 71`;
- 回归工具:`scripts/win32_input_matrix.ps1`(PostMessage 无打扰驱动 + 基线比对,
  `-Record` 录制 / 默认断言),基线含 Esc/Enter/Tab/BS/方向/F 键/裸修饰键静默;
- 2026-08-09 用户在独立实例中实测 Claude Code:Esc 打断恢复正常,Enter/Ctrl+Enter/
  方向键无退化。

### 教训

- **协议实现要模拟「事实」而不是「够用」**:uChar=0 在自测(pwsh、vim)全绿,因为多数
  读者按 VK 兜底;直到遇上严格按字节流消费的 node 才爆雷。保真基准是「真实键盘发什么」,
  不是「我测的程序认不认」。
- **同一症状在不同读端表现分叉时,先问读法**:Windows 上 TUI 分「VK 派」(crossterm/
  .NET ReadKey)和「字节流派」(node/libuv、Ink);分叉本身就是定位信息。
- **宿主行为随版本漂移**:OpenConsole 1.22 与 in-box conhost 对同一 INPUT_RECORD 的翻译
  不同。sideload 换来的确定性,也意味着兼容性问题要在 sideload 版本上验证,系统 conhost
  上的「正常」不作数。
- **用户活跃时段禁用 SendKeys/SetForegroundWindow 探针**:实测把用户正在打的中文吸进
  探针窗口、探针键反向漏进用户窗口。无打扰替代:`-WindowStyle Minimized` 启动 +
  `PostMessageW(WM_KEYDOWN/UP)` 直达 winit 消息循环,不碰焦点(局限:改不了真实修饰键
  状态,带修饰组合仍需人在场用 SendInput 测)。

---

## 案例 2:Codex 的 Shift+Enter 需要 win32-input-mode(2026-08-08,案例 1 的前篇)

### 症状

Codex CLI 在 Nebula 里 Shift+Enter 无法换行(经典 VT 编码下 Shift+Enter 与 Enter 都是
`\r`,信息在编码时就丢了),Windows Terminal 里正常。

### 根因与修复原理

WT 的能力来自 ConPTY 的 Win32 input mode(DECSET 9001,`#4999` spec):终端把完整的
`KEY_EVENT_RECORD` 六元组(Vk/Sc/Uc/Kd/Cs/Rc)编码为 `CSI ..._` 发给 ConPTY,修饰状态
(Cs)随记录携带,ConPTY 重建 INPUT_RECORD 给子程序——Shift 信息得以幸存。修复 =
`CreatePseudoConsole` 传 `PSEUDOCONSOLE_WIN32_INPUT_MODE (0x4)` + 终端侧跟踪 9001 模式位
+ `build_win32_input_sequence` 编码器 + winit fork 暴露 `RawKeyEventInfo`(VK/扫描码/
repeat/extended/control_key_state 一次捕获)。Kitty 协议激活时 win32 记录让位(两套编码
互斥,kitty 是子程序显式请求的更高层契约)。

### 教训

- 案例 1 正是本案的直接后果:引入新协议时,「所有键」的编码保真度都要按真实键盘对表,
  而不是只验证目标场景(Shift+Enter)。协议是面,不是点。
- 新输入协议上线必须配读端矩阵(VK 派 + 字节流派至少各一个)——本案自测用了 VK 派的
  Codex,字节流派的 cc 三周后才暴露。

---

（新案例往下追加;修复原理写到「换个人也能按原理重新推导出补丁」的程度。）
