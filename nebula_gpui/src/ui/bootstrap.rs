use gpui::App;

pub fn init(cx: &mut App) {
    // 组件库必须只初始化一次，否则全局 action、菜单和主题状态会重复注册。
    gpui_component::init(cx);
    super::theme::apply_nebula_theme(cx);
}
