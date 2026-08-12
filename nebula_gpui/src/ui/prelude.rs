//! 业务视图只从这里导入获准使用的第三方组件。
//!
//! 简单组件直接复用；只有需要承载 Nebula 业务合同的组件才在本 crate 中封装。

// 这些导出是受控的业务入口，未被当前验收页使用的组件仍应保留，避免业务代码
// 重新从上游散落导入而绕过升级边界。
#[allow(unused_imports)]
pub use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{Dialog, DialogButtonProps},
    dock::{DockArea, DockAreaState, DockItem, Panel, PanelControl, PanelEvent, PanelState, TabPanel},
    h_flex,
    input::{Input, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    select::{Select, SelectState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    tab::{Tab, TabBar},
    v_flex,
};

pub use gpui_component::IndexPath;
pub use gpui_component::WindowExt as _;
