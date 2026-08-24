//! 业务视图只从这里导入获准使用的第三方组件。
//!
//! 简单组件直接复用；只有需要承载 Nebula 业务合同的组件才在本 crate 中封装。

// 这些导出是受控的业务入口，未被当前验收页使用的组件仍应保留，避免业务代码
// 重新从上游散落导入而绕过升级边界。
#[allow(unused_imports)]
pub use gpui_component::{
    ActiveTheme as _, Colorize as _, Disableable as _, Icon, IconName, InteractiveElementExt as _,
    Selectable as _, Sizable as _, StyledExt as _, TitleBar,
    alert::{Alert, AlertVariant},
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    dialog::{Dialog, DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    dock::{
        DockArea, DockAreaState, DockItem, Panel, PanelControl, PanelEvent, PanelState, TabPanel,
    },
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu},
    resizable::{ResizableState, h_resizable, resizable_panel, v_resizable},
    scroll::ScrollableElement as _,
    select::{Select, SelectState},
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    slider::{Slider, SliderEvent, SliderState},
    spinner::Spinner,
    switch::Switch,
    tab::{Tab, TabBar},
    v_flex,
};

pub use gpui_component::IndexPath;
pub use gpui_component::WindowExt as _;

// 上面那批 trait 是 `as _` 导出的：`prelude::*` 的使用者能拿到方法，但 `as _`
// 抹掉了名字，别处再 `use prelude::ActiveTheme` 就找不到。只导入具体几项、
// 不 glob 的模块（如 `terminal::view`）需要具名版本，这里补上——两种导出并存，
// 现有的 glob 用法不受影响。
pub use gpui_component::{ActiveTheme, Colorize};

use gpui::{Window, px};

/// 保留旧壳确认框的 480px 内容宽度；位置和交互采用新版 Dialog。
pub fn center_confirm_dialog(dialog: Dialog, _window: &Window) -> Dialog {
    const DIALOG_W: f32 = 480.0;
    dialog.width(px(DIALOG_W))
}
