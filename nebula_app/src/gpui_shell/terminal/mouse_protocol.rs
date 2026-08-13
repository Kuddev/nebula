//! 鼠标上报协议编码：X10/normal、SGR、UTF-8 扩展坐标。
//!
//! 从旧壳 `nebula_app/src/input/mouse.rs` 的 `mouse_report` /
//! `normal_mouse_report` / `sgr_mouse_report` 逐字对照移植成纯函数——
//! 字节序列以旧壳行为为权威，这里只做"无 UI 状态"化，方便单测锁定。
//!
//! 坐标约定：`point` 是 buffer 坐标（`viewport_to_point` 之后），负行
//! 意味着鼠标落在回滚区，任何协议都不上报。

use nebula_terminal::index::Point as TermPoint;
use nebula_terminal::term::TermMode;

/// 按钮码（xterm）：左 0、中 1、右 2；滚轮上/下 64/65。
/// 拖动在按钮码上 +32；无按键的纯移动固定 35。
pub const BUTTON_LEFT: u8 = 0;
pub const BUTTON_MIDDLE: u8 = 1;
pub const BUTTON_RIGHT: u8 = 2;
pub const WHEEL_UP: u8 = 64;
pub const WHEEL_DOWN: u8 = 65;
pub const DRAG_OFFSET: u8 = 32;
pub const MOTION_ONLY: u8 = 35;

#[derive(Clone, Copy, Default)]
pub struct ReportMods {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

impl ReportMods {
    /// xterm 修饰位：shift +4、alt +8、ctrl +16。
    fn value(self) -> u8 {
        let mut mods = 0;
        if self.shift {
            mods += 4;
        }
        if self.alt {
            mods += 8;
        }
        if self.control {
            mods += 16;
        }
        mods
    }
}

/// 编码一次鼠标上报。`None` 表示这次事件不应上报（回滚区、normal 模式
/// 坐标超界）。`pressed=false` 只对按键有意义；滚轮永远按 pressed 上报。
pub fn report(
    mode: &TermMode,
    point: TermPoint,
    button: u8,
    pressed: bool,
    mods: ReportMods,
) -> Option<Vec<u8>> {
    // 回滚区不属于应用坐标空间。
    if point.line.0 < 0 {
        return None;
    }

    let mods = mods.value();
    if mode.contains(TermMode::SGR_MOUSE) {
        let c = if pressed { 'M' } else { 'm' };
        Some(
            format!("\x1b[<{};{};{}{}", button + mods, point.column.0 + 1, point.line.0 + 1, c)
                .into_bytes(),
        )
    } else if pressed {
        normal_report(mode, point, button + mods)
    } else {
        // normal 协议无法区分哪个键抬起：释放一律报 3。
        normal_report(mode, point, 3 + mods)
    }
}

fn normal_report(mode: &TermMode, point: TermPoint, button: u8) -> Option<Vec<u8>> {
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let max_point: i32 = if utf8 { 2015 } else { 223 };

    if point.line.0 >= max_point || point.column.0 >= max_point as usize {
        return None;
    }

    let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

    let mouse_pos_encode = |pos: usize| -> [u8; 2] {
        let pos = 32 + 1 + pos;
        [(0xC0 + pos / 64) as u8, (0x80 + (pos & 63)) as u8]
    };

    if utf8 && point.column.0 >= 95 {
        msg.extend_from_slice(&mouse_pos_encode(point.column.0));
    } else {
        msg.push(32 + 1 + point.column.0 as u8);
    }

    if utf8 && point.line.0 >= 95 {
        msg.extend_from_slice(&mouse_pos_encode(point.line.0 as usize));
    } else {
        msg.push(32 + 1 + point.line.0 as u8);
    }

    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_terminal::index::{Column, Line};

    fn at(line: i32, col: usize) -> TermPoint {
        TermPoint::new(Line(line), Column(col))
    }

    const NO_MODS: ReportMods = ReportMods { shift: false, alt: false, control: false };

    #[test]
    fn sgr_press_and_release() {
        let mode = TermMode::SGR_MOUSE;
        assert_eq!(
            report(&mode, at(0, 0), BUTTON_LEFT, true, NO_MODS),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
        assert_eq!(
            report(&mode, at(9, 4), BUTTON_LEFT, false, NO_MODS),
            Some(b"\x1b[<0;5;10m".to_vec())
        );
    }

    #[test]
    fn sgr_modifiers_add_to_button() {
        let mode = TermMode::SGR_MOUSE;
        let mods = ReportMods { shift: false, alt: false, control: true };
        assert_eq!(
            report(&mode, at(0, 0), BUTTON_RIGHT, true, mods),
            Some(b"\x1b[<18;1;1M".to_vec())
        );
    }

    #[test]
    fn normal_press_release_and_bounds() {
        let mode = TermMode::empty();
        // 按下左键 (0,0)：CSI M, 32+0, 33, 33。
        assert_eq!(
            report(&mode, at(0, 0), BUTTON_LEFT, true, NO_MODS),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        // 释放一律报按钮 3。
        assert_eq!(
            report(&mode, at(0, 0), BUTTON_LEFT, false, NO_MODS),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
        // normal 协议坐标上限 223。
        assert_eq!(report(&mode, at(0, 223), BUTTON_LEFT, true, NO_MODS), None);
        assert_eq!(report(&mode, at(223, 0), BUTTON_LEFT, true, NO_MODS), None);
    }

    #[test]
    fn utf8_mouse_encodes_wide_coordinates() {
        let mode = TermMode::UTF8_MOUSE;
        // col=100 → pos=133 → 0xC2 0x85（两字节），行仍单字节。
        assert_eq!(
            report(&mode, at(0, 100), BUTTON_LEFT, true, NO_MODS),
            Some(vec![0x1b, b'[', b'M', 32, 0xC2, 0x85, 33])
        );
        // UTF-8 扩展上限 2015。
        assert_eq!(report(&mode, at(0, 2015), BUTTON_LEFT, true, NO_MODS), None);
        assert!(report(&mode, at(0, 2014), BUTTON_LEFT, true, NO_MODS).is_some());
    }

    #[test]
    fn scrollback_is_never_reported() {
        for mode in [TermMode::SGR_MOUSE, TermMode::empty()] {
            assert_eq!(report(&mode, at(-1, 0), BUTTON_LEFT, true, NO_MODS), None);
        }
    }

    #[test]
    fn wheel_uses_press_encoding() {
        let mode = TermMode::SGR_MOUSE;
        assert_eq!(
            report(&mode, at(2, 2), WHEEL_UP, true, NO_MODS),
            Some(b"\x1b[<64;3;3M".to_vec())
        );
        assert_eq!(
            report(&mode, at(2, 2), WHEEL_DOWN, true, NO_MODS),
            Some(b"\x1b[<65;3;3M".to_vec())
        );
    }
}
