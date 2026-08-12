use gpui::App;

pub fn init(cx: &mut App) {
    // 组件库必须只初始化一次，否则全局 action、菜单和主题状态会重复注册。
    gpui_component::init(cx);
    super::theme::apply_nebula_theme(cx);
    crate::views::workspace::init(cx);

    // 用户 nebula.toml（字体/终端配色），启动读一次；失败静默回退默认值。
    let settings = crate::config::Settings::load();
    match &settings.source_path {
        Some(path) => eprintln!("[nebula-gpui] config loaded: {}", path.display()),
        None => eprintln!("[nebula-gpui] no user config found, using defaults"),
    }
    cx.set_global(settings);
}
