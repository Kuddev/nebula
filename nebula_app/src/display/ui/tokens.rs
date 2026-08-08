//! Nebula 的共享 UI 契约：间距、圆角、字阶、层级、动效、控件状态。
//!
//! 值是逻辑像素/倍率。调用方在布局时**恰好**应用一次窗口 DPI；绘制、hover、
//! 焦点和命中检测必须从同一个结果矩形派生。

// 这个目录是设计系统的完整表达，允许存在暂未采用的条目——补全一个色阶或间距
// 阶梯不该被 dead_code 拦住。约束的方向是反过来的：**代码里不允许出现本目录
// 已经覆盖的魔数**。侦察数据（2026-07-29）：仅 3 个文件就有 26 个手写的
// `s(16.0)`（= space::M）、31 个 `s(8.0)`（= radius::OVERLAY）。
#![allow(dead_code)]

pub mod space {
    pub const XXS: f32 = 4.0;
    pub const XS: f32 = 8.0;
    pub const S: f32 = 12.0;
    pub const M: f32 = 16.0;
    pub const L: f32 = 24.0;
    pub const XL: f32 = 32.0;
}

/// 圆角阶梯。逐层递减：浮层 → 控件 → chip。
///
/// 参照 Windows 11 Fluent 2（`OverlayCornerRadius` = 8，`ControlCornerRadius` = 4），
/// 不是 macOS 的 10–12。2026-07-29 侦察发现八个浮层用了六种圆角
/// （命令面板 14、设置模态 12、确认框 12、AI 条 10、右键菜单 9、popup 8），
/// 这是界面"不成体系"的直接来源。
pub mod radius {
    /// 浮层：命令面板、模态、右键菜单、下拉 popup。
    pub const OVERLAY: f32 = 8.0;
    /// 统一启动器：参考稿用更宽的外轮廓，面板宽度变化时仍保持柔和的弧线。
    pub const LAUNCHER: f32 = 14.0;
    /// 控件：combobox、spinner、输入框、按钮。
    /// Fluent 本身是 4；我们的控件高 32px，6 更耐看，实机再校。
    pub const CONTROL: f32 = 6.0;
    /// 品牌/终端图标的中性 tile。
    pub const ICON_TILE: f32 = 7.0;
    /// chip / 键帽 / 徽标。
    pub const CHIP: f32 = 4.0;
}

pub mod type_scale {
    /// 11px-style sidebar/group caption from the Maple base face.
    pub const SECTION_CAPTION: f32 = 0.75;
    /// Supporting/empty-state copy: subordinate without becoming unreadable.
    pub const SUPPORTING: f32 = 0.80;
    pub const BODY: f32 = 1.0;
    pub const DIALOG_TITLE: f32 = 1.35;
}

pub mod control {
    pub const ICON_BUTTON: f32 = 20.0;
    pub const MIN_HIT_TARGET: f32 = 32.0;
    /// Dense rows used by command/search pickers where several choices must
    /// remain visible without making the input visually dominate the content.
    pub const COMPACT_ROW: f32 = 38.0;
    pub const ROW: f32 = 44.0;
    pub const HAIRLINE: f32 = 1.0;
}

/// 浮层的 Z 轴处理。阴影按浮层面积分档——同一组参数放在小菜单上刚好，
/// 放在整块命令面板上就托不住（阴影扩散必须与被托举的面积成比例）。
///
/// 调用方通常不直接用这些常量，而是走 [`super::surface::Elevation`]。
pub mod elevation {
    /// 小浮层：右键菜单、下拉 popup、tooltip。
    pub const MENU_BLUR: f32 = 12.0;
    pub const MENU_OFFSET_Y: f32 = 4.0;
    /// 大浮层：命令面板、shell 选择器。
    pub const POPOVER_BLUR: f32 = 14.0;
    pub const POPOVER_OFFSET_Y: f32 = 5.0;
    /// 模态：确认框、设置页、SSH 编辑器。
    pub const MODAL_BLUR: f32 = 20.0;
    pub const MODAL_OFFSET_Y: f32 = 7.0;
    /// 阴影浓度。2026-07-29 用户裁定：此前 54/86（21%/34%）太重，浮层像
    /// 被垫了一块黑布。成熟终端的浮层阴影普遍在 10% 上下——它只需要**暗示**
    /// 高度，一旦看得清"这是一片阴影"就已经过头了。浅色底上尤其要淡，
    /// 黑色扩散在浅灰上很容易显脏。
    pub const SHADOW_ALPHA_LIGHT: u8 = 26;
    pub const SHADOW_ALPHA_DARK: u8 = 56;
}

/// Terminal-grid feedback colors are rendered through the cell/rect alpha
/// pipeline. Thin cursors need more opacity than a full block to remain visible.
pub mod terminal_feedback {
    /// Blend the terminal hue anchor toward theme-neutral ink before applying
    /// alpha; this preserves hue continuity without an electric saturation.
    pub const ANCHOR_NEUTRAL_MIX_LIGHT: f32 = 0.28;
    pub const ANCHOR_NEUTRAL_MIX_DARK: f32 = 0.18;
    pub const BLOCK_CURSOR_ALPHA_LIGHT: f32 = 0.20;
    pub const BLOCK_CURSOR_ALPHA_DARK: f32 = 0.30;
    pub const STROKE_CURSOR_ALPHA_LIGHT: f32 = 0.72;
    pub const STROKE_CURSOR_ALPHA_DARK: f32 = 0.82;
    pub const SELECTION_ALPHA_LIGHT: f32 = 0.15;
    pub const SELECTION_ALPHA_DARK: f32 = 0.22;
    /// WCAG AA normal-text floor for application-owned terminal RGB colors.
    pub const FIXED_TEXT_MIN_CONTRAST: f64 = 4.5;
}

pub mod motion {
    use std::time::Duration;

    pub const MENU_OPEN: Duration = Duration::from_millis(120);
    pub const MENU_CLOSE: Duration = Duration::from_millis(90);
    pub const HOVER: Duration = Duration::from_millis(150);
    /// CSS-like emphasized-decelerate curve used by short reveal motion.
    pub const EMPHASIZED_DECELERATE: [f32; 4] = [0.16, 1.0, 0.3, 1.0];
}

/// Required states for every self-drawn control. Empty/loading/error describe
/// content surfaces; pointer/keyboard controls use the first five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    Default,
    Hover,
    Focus,
    Active,
    Disabled,
    Empty,
    Loading,
    Error,
}

impl ControlState {
    /// Persistent/blocked states outrank transient pointer feedback.
    pub fn interactive(disabled: bool, active: bool, focused: bool, hovered: bool) -> Self {
        if disabled {
            Self::Disabled
        } else if active {
            Self::Active
        } else if focused {
            Self::Focus
        } else if hovered {
            Self::Hover
        } else {
            Self::Default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ControlState;

    #[test]
    fn persistent_control_states_outrank_hover() {
        assert_eq!(ControlState::interactive(true, true, true, true), ControlState::Disabled);
        assert_eq!(ControlState::interactive(false, true, true, true), ControlState::Active);
        assert_eq!(ControlState::interactive(false, false, true, true), ControlState::Focus);
        assert_eq!(ControlState::interactive(false, false, false, true), ControlState::Hover);
    }
}
