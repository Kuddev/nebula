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

use gpui::{
    InteractiveElement as _, ParentElement as _, SharedString, Styled as _, Window, div, px,
    relative,
};
use gpui_component::dialog::{CancelDialog, ConfirmDialog};

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
/// Dialog 不会仅凭 button_props 生成按钮。动作必须绑在真实 Button 上；该版本
/// Button 在按下时会 prevent_default，依赖父容器 click 会让鼠标确认和取消都失效。
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
                .w_full()
                .child(
                    Button::new("confirm-cancel")
                        .debug_selector(|| "confirm-dialog-cancel".to_owned())
                        .flex_1()
                        .label(cancel_text.clone())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(CancelDialog), cx);
                        }),
                )
                .child(
                    Button::new("confirm-ok")
                        .debug_selector(|| "confirm-dialog-ok".to_owned())
                        .flex_1()
                        .label(ok_text.clone())
                        .with_variant(ok_variant)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ConfirmDialog), cx);
                        }),
                ),
        )
        .button_props(
            DialogButtonProps::default()
                .ok_text(ok_text)
                .ok_variant(ok_variant)
                .cancel_text(cancel_text)
                .show_cancel(true),
        )
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AppContext as _, Context, IntoElement, Modifiers, Render, TestAppContext, Window, div,
        point,
    };
    use gpui_component::{Root, WindowExt as _};

    use super::*;

    #[derive(Default)]
    struct ConfirmDialogProbe;

    impl Render for ConfirmDialogProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            div().size_full().children(Root::render_dialog_layer(window, cx))
        }
    }

    #[gpui::test]
    fn confirm_dialog_mouse_buttons_dispatch_cancel_and_confirm(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|_| ConfirmDialogProbe);
            Root::new(view, window, cx)
        });

        let confirmed = Rc::new(Cell::new(false));
        let cancelled = Rc::new(Cell::new(false));
        let open_dialog = |cx: &mut gpui::VisualTestContext| {
            let confirmed = confirmed.clone();
            let cancelled = cancelled.clone();
            cx.update(|window, cx| {
                window.open_dialog(cx, move |dialog, window, _| {
                    confirm_dialog(
                        dialog,
                        window,
                        "Confirm action?",
                        "Choose an action.",
                        "Confirm",
                        "Cancel",
                        ButtonVariant::Primary,
                    )
                    .on_ok({
                        let confirmed = confirmed.clone();
                        move |_, _, _| {
                            confirmed.set(true);
                            true
                        }
                    })
                    .on_cancel({
                        let cancelled = cancelled.clone();
                        move |_, _, _| {
                            cancelled.set(true);
                            true
                        }
                    })
                });
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };

        open_dialog(cx);
        let cancel = cx.debug_bounds("confirm-dialog-cancel").expect("cancel button bounds");
        let ok = cx.debug_bounds("confirm-dialog-ok").expect("confirm button bounds");
        let width_delta = (f32::from(cancel.size.width) - f32::from(ok.size.width)).abs();
        assert!(width_delta <= 2.0, "确认框按钮只能有像素吸附造成的微小宽度差异");
        cx.simulate_click(
            point(
                cancel.origin.x + cancel.size.width * 0.5,
                cancel.origin.y + cancel.size.height * 0.5,
            ),
            Modifiers::default(),
        );
        assert!(cancelled.get(), "鼠标点击取消必须派发 Dialog 的取消动作");
        assert!(!confirmed.get());

        open_dialog(cx);
        let ok = cx.debug_bounds("confirm-dialog-ok").expect("confirm button bounds");
        cx.simulate_click(
            point(ok.origin.x + ok.size.width * 0.5, ok.origin.y + ok.size.height * 0.5),
            Modifiers::default(),
        );
        assert!(confirmed.get(), "鼠标点击确认必须派发 Dialog 的确认动作");
    }
}
