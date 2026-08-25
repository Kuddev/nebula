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
    button::{Button, ButtonVariant, ButtonVariants as _},
    checkbox::Checkbox,
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    dialog::{
        Dialog, DialogAction, DialogButtonProps, DialogClose, DialogDescription, DialogFooter,
    },
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

use gpui::{ParentElement as _, SharedString, Styled as _, Window, div, px, relative};

const CONFIRM_DIALOG_WIDTH: f32 = 480.0;
const CONFIRM_DIALOG_HORIZONTAL_PADDING: f32 = 26.0;
const CONFIRM_DIALOG_TOP_PADDING: f32 = 26.0;
const CONFIRM_DIALOG_BOTTOM_PADDING: f32 = 20.0;
const CONFIRM_DIALOG_BASE_HEIGHT: f32 = 130.0;
const CONFIRM_DIALOG_BODY_LINE_HEIGHT: f32 = 24.0;

/// gpui-component 0.5.2 默认把 Dialog 放在视口高度的 1/10；旧 GPUI 壳则
/// 使用 480px 内容宽度并垂直居中。这里集中恢复旧壳几何，同时保留窄窗口限幅。
pub fn center_modal_dialog(dialog: Dialog, window: &Window, estimated_height: f32) -> Dialog {
    let viewport = window.viewport_size();
    let width = CONFIRM_DIALOG_WIDTH.min((f32::from(viewport.width) - 32.0).max(240.0));
    let margin_top = ((f32::from(viewport.height) - estimated_height) * 0.5).max(16.0);

    dialog
        .width(px(width))
        .margin_top(px(margin_top))
        .pt(px(CONFIRM_DIALOG_TOP_PADDING))
        .pb(px(CONFIRM_DIALOG_BOTTOM_PADDING))
        .px(px(CONFIRM_DIALOG_HORIZONTAL_PADDING))
}

/// 构造与旧 GPUI 壳一致的确认框。显式 footer 是必要的：固定依赖版本的普通
/// Dialog 不会仅凭 button_props 生成按钮；DialogAction/DialogClose 负责把鼠标
/// 点击重新汇入 Enter/Esc 共用的确认与取消回调。
pub fn confirm_dialog(
    dialog: Dialog,
    window: &mut Window,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    ok_text: impl Into<SharedString>,
    cancel_text: impl Into<SharedString>,
    ok_variant: ButtonVariant,
) -> Dialog {
    let title = title.into();
    let description = description.into();
    let ok_text = ok_text.into();
    let cancel_text = cancel_text.into();

    let text_style = window.text_style();
    let body_width = if description.is_empty() {
        0.0
    } else {
        f32::from(
            window
                .text_system()
                .shape_line(
                    description.clone(),
                    px(16.0),
                    &[text_style.to_run(description.len())],
                    None,
                )
                .width,
        )
    };
    let available_body_width =
        (CONFIRM_DIALOG_WIDTH - CONFIRM_DIALOG_HORIZONTAL_PADDING * 2.0).max(1.0);
    let body_lines = (body_width / available_body_width).ceil().max(1.0);
    let estimated_height =
        CONFIRM_DIALOG_BASE_HEIGHT + body_lines * CONFIRM_DIALOG_BODY_LINE_HEIGHT;

    let content_description = description.clone();
    center_modal_dialog(dialog, window, estimated_height)
        .close_button(false)
        // 新 Dialog 的遮罩点击与 Esc 共用 on_cancel 合同，不能为了旧版几何
        // 关闭这项交互，否则业务取消回调也不会执行。
        .overlay_closable(true)
        .title(div().text_lg().font_semibold().line_height(relative(1.0)).child(title))
        .content(move |content, _, _| {
            content.child(
                DialogDescription::new()
                    .text_base()
                    .line_height(relative(1.5))
                    .child(content_description.clone()),
            )
        })
        .footer(
            DialogFooter::new()
                .child(
                    DialogClose::new()
                        .child(Button::new("confirm-cancel").label(cancel_text.clone())),
                )
                .child(DialogAction::new().child(
                    Button::new("confirm-ok").label(ok_text.clone()).with_variant(ok_variant),
                )),
        )
        .button_props(
            DialogButtonProps::default()
                .ok_text(ok_text)
                .ok_variant(ok_variant)
                .cancel_text(cancel_text)
                .show_cancel(true),
        )
}
