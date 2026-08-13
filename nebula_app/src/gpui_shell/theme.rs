use gpui::{App, hsla, px};
use gpui_component::{Theme, ThemeMode};

/// 按运行时主题联动窗口 chrome 深浅：浅色终端主题切组件库 Light 模式，
/// 深色主题回 Nebula 定制 token。启动与设置变更都走这里。
pub fn apply_chrome_theme(cx: &mut App) {
    let is_light = nebula_settings::RuntimeSettings::load().theme.term_theme().is_light;
    if is_light {
        Theme::change(ThemeMode::Light, None, cx);
    } else {
        apply_nebula_theme(cx);
    }
}

pub fn apply_nebula_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);

    // 先改全局 token，而不是逐组件覆写样式，确保后续直接引入的组件自然进入
    // Nebula 视觉系统，也让上游升级时需要维护的差异集中在一个位置。
    let theme = Theme::global_mut(cx);
    let background = hsla(220.0 / 360.0, 0.16, 0.075, 1.0);
    let surface = hsla(220.0 / 360.0, 0.13, 0.105, 1.0);
    let border = hsla(216.0 / 360.0, 0.12, 0.21, 1.0);
    let text = hsla(210.0 / 360.0, 0.12, 0.91, 1.0);
    let muted_text = hsla(214.0 / 360.0, 0.09, 0.62, 1.0);
    let accent = hsla(155.0 / 360.0, 0.62, 0.47, 1.0);
    let accent_hover = hsla(155.0 / 360.0, 0.58, 0.52, 1.0);
    let accent_active = hsla(155.0 / 360.0, 0.66, 0.40, 1.0);

    theme.background = background;
    theme.foreground = text;
    theme.group_box = surface;
    theme.group_box_foreground = text;
    theme.popover = surface;
    theme.popover_foreground = text;
    theme.border = border;
    theme.input = border;
    theme.primary = accent;
    theme.primary_hover = accent_hover;
    theme.primary_active = accent_active;
    theme.primary_foreground = background;
    theme.secondary = surface;
    theme.secondary_hover = border;
    theme.secondary_active = background;
    theme.secondary_foreground = text;
    theme.muted = surface;
    theme.muted_foreground = muted_text;
    theme.accent = accent;
    theme.accent_foreground = background;
    theme.caret = accent;
    theme.ring = accent;
    theme.selection = hsla(155.0 / 360.0, 0.52, 0.34, 0.72);
    theme.font_size = px(14.0);
    theme.mono_font_size = px(13.0);
    theme.radius = px(5.0);
    theme.radius_lg = px(7.0);
}
