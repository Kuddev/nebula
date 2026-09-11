//! GPUI key facts adapted to ConPTY Win32 input records.

use gpui::Keystroke;

/// 无修饰的字母/数字/空格必须交给 IME / `TranslateMessage`，不能编进 PTY。
///
/// GPUI 的 Windows 后端：`on_key_down` 一旦 `stop_propagation`，就不会再
/// `TranslateMessage`。IME 组字（微软拼音）是 TranslateMessage 喂进去的；
/// 把 `n`/`i` 编成 KEY_EVENT_RECORD 等于把拼音当英文写进 shell，中文永远
/// 起不来。旧壳对应合同是 `keyboard.rs`：`ime.preedit()` 期间直接 return。
pub(super) fn win32_encodes_keystroke(ks: &Keystroke) -> bool {
    if ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform {
        return true;
    }
    let key = ks.key.as_str();
    if key == "space" {
        return false;
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => false,
        _ => true,
    }
}

/// GPUI 键名 → Win32 虚拟键码。
///
/// 旧壳从 winit fork 的 `RawKeyEventInfo` 直接拿到系统报的 VK；GPUI 的
/// `Keystroke` 只有键名，所以这里按名字反查。表只覆盖**编码器会处理的键**
/// （控制键、方向、功能键）——可打印字符在 GPUI 走 IME 管道，不经这里。
pub(super) fn virtual_key_of(key: &str) -> Option<u16> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT,
        VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
    };

    let vk = match key {
        "escape" => VK_ESCAPE,
        "enter" => VK_RETURN,
        "tab" => VK_TAB,
        "backspace" => VK_BACK,
        "space" => VK_SPACE,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "home" => VK_HOME,
        "end" => VK_END,
        "insert" => VK_INSERT,
        "delete" => VK_DELETE,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        // F1..F24 在 VK 表里连号。
        key if key.starts_with('f') => {
            let index: u16 = key[1..].parse().ok()?;
            if !(1..=24).contains(&index) {
                return None;
            }
            VK_F1 + index - 1
        },
        // 单个字符（Ctrl+C 一族）：ASCII 字母数字的 VK 就是它的大写码点。
        key => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            if !c.is_ascii_alphanumeric() {
                return None;
            }
            c.to_ascii_uppercase() as u16
        },
    };
    Some(vk)
}

/// 控制键必须携带真实 `KEY_EVENT_RECORD` 的字符值（Esc=0x1B、Enter=0x0D、
/// Tab=0x09、Backspace=0x08）：OpenConsole 1.22 的 VT 翻译层会丢弃 uChar=0
/// 的 VK_ESCAPE，于是读字节流的那类应用（Claude Code）收不到 Esc。修饰键与
/// 功能键保持 0，与真实键盘一致。逐条同旧壳 `control_char_fallback`。
fn unicode_char_of(ks: &Keystroke, scan_code: u32) -> u16 {
    // 平台已经判出文本的（含 Ctrl 变体）以它为准，与 WM_CHAR 语义一致。
    // `key_char` 若是 NUL，当作没文本：真实键盘的 Esc 不会写出 U+0000。
    if let Some(text) = ks.key_char.as_deref() {
        let mut units = text.encode_utf16();
        if let (Some(first), None) = (units.next(), units.next()) {
            if first != 0 {
                return first;
            }
        }
    }
    match ks.key.as_str() {
        "escape" => 0x1b,
        "enter" => crate::platform::keyboard::enter_character(
            scan_code,
            ks.modifiers.shift,
            ks.modifiers.control,
            ks.modifiers.alt,
        ),
        "tab" => b'\t' as u16,
        // 真实控制台的 Ctrl+Backspace 记录携带 Uc=DEL（0x7f，WM_CHAR 语义），
        // PSReadLine 的原生 Ctrl+Backspace=BackwardKillWord 绑定按此匹配。
        // 给 0x08 会跌进 KeyChar 分发被当成 Ctrl+H——只删一个字符。
        "backspace" => {
            if ks.modifiers.control {
                0x7f
            } else {
                0x08
            }
        },
        "space" => b' ' as u16,
        _ => 0,
    }
}

/// 一条 ConPTY Win32 input 记录：`CSI Vk;Sc;Uc;Kd;Cs;Rc_`。
///
/// 认不出 VK 的键返回 `None`，调用方回落到传统 VT 编码——宁可少一条记录，
/// 也不要编一个 Vk=0 的假记录，那会让子进程读到一个不存在的键。
pub(super) fn win32_input_record(ks: &Keystroke, key_down: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC, MapVirtualKeyW};

    const SHIFT_PRESSED: u32 = 0x0010;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;

    let vk = virtual_key_of(ks.key.as_str())?;
    // 扫描码问系统要，不硬编码：非 US 布局与笔记本键盘上这张表并不通用。
    let scan_code = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) };
    let mut control_key_state = 0u32;
    if ks.modifiers.shift {
        control_key_state |= SHIFT_PRESSED;
    }
    if ks.modifiers.alt {
        control_key_state |= LEFT_ALT_PRESSED;
    }
    if ks.modifiers.control {
        control_key_state |= LEFT_CTRL_PRESSED;
    }
    let key_down = u8::from(key_down);
    Some(
        format!(
            "\x1b[{};{};{};{key_down};{};1_",
            vk,
            scan_code,
            unicode_char_of(ks, scan_code),
            control_key_state
        )
        .into_bytes(),
    )
}

/// GPUI 的终端视图只接到 key-down。旧壳对 9001 会再写一条 Kd=0 的抬起
/// （`keyboard.rs` 的 `key_release` + `escape_carries_its_control_character_both_directions`）。
/// 真实键盘也是 down+up：Codex 按 VK 看按下就够了，Claude Code / Ink 吃的是
/// OpenConsole 翻译出的字节流，缺抬起时 Esc 常常一个字节都到不了。
pub(super) fn win32_press_and_release(ks: &Keystroke) -> Option<Vec<u8>> {
    let mut sequence = win32_input_record(ks, true)?;
    sequence.extend(win32_input_record(ks, false)?);
    Some(sequence)
}
