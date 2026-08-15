use gpui::App;

pub fn init(cx: &mut App) {
    // 组件库必须只初始化一次，否则全局 action、菜单和主题状态会重复注册。
    //
    // 这里故意不再覆盖 Nebula 产品主题：产品 GPUI 壳唯一读取旧壳
    // `display::ui::theme::NebulaTheme`。实验场只验证组件库自身行为，避免
    // 再维护一份会漂移的私有颜色表。
    gpui_component::init(cx);
}
