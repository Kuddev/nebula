use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};

use crate::terminal::view::TerminalView;
use crate::ui::prelude::*;

gpui::actions!(component_gallery, [Increment]);

pub struct ComponentGallery {
    input: Entity<InputState>,
    editor: Entity<InputState>,
    select: Entity<SelectState<Vec<&'static str>>>,
    dock_area: Entity<DockArea>,
    terminal: Entity<TerminalView>,
    enabled: bool,
    clicks: usize,
    active_tab: usize,
}

/// Dock 需要一个真正的 Panel 合同；这个面板只负责演示接入边界，不携带业务状态。
pub struct GalleryPanel {
    focus_handle: FocusHandle,
}

impl GalleryPanel {
    fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle() }
    }
}

impl EventEmitter<PanelEvent> for GalleryPanel {}

impl Focusable for GalleryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for GalleryPanel {
    fn panel_name(&self) -> &'static str {
        "NebulaGalleryPanel"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "停靠面板"
    }
}

impl Render for GalleryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().gap_2().p_3().child("这是一个直接接入 DockArea 的 GPUI Panel。").child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("后续终端 Pane、文件树和设置页可以复用同一停靠合同。"),
        )
    }
}

impl ComponentGallery {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("输入中文以验证 IME")
                .default_value("Nebula GPUI 组件基础已接通")
        });

        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("rust")
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
                .rows(8)
                .default_value("fn main() {\n    println!(\"Nebula\");\n}")
                .placeholder("代码编辑器支持 UTF-8、选区和撤销")
        });

        let select = cx.new(|cx| {
            SelectState::new(
                vec!["终端", "代码编辑器", "设置"],
                Some(IndexPath::default().row(0)),
                window,
                cx,
            )
        });

        let dock_area = cx.new(|cx| DockArea::new("nebula-gallery", Some(1), window, cx));
        let weak_dock_area = dock_area.downgrade();
        let panel = cx.new(GalleryPanel::new);
        dock_area.update(cx, |dock, cx| {
            dock.set_center(DockItem::tab(panel, &weak_dock_area, window, cx), window, cx);
        });

        let terminal = cx.new(TerminalView::new);

        Self { input, editor, select, dock_area, terminal, enabled: true, clicks: 0, active_tab: 0 }
    }
}

impl Render for ComponentGallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.clicks;
        let status: SharedString = format!("已触发 {count} 次组件事件").into();

        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &Increment, _, cx| {
                this.clicks += 1;
                cx.notify();
            }))
            .child(
                v_flex()
                    .size_full()
                    .p_8()
                    .gap_6()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Nebula"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("GPUI component acceptance surface"),
                            ),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(720.0))
                            .gap_3()
                            .p_5()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_lg()
                            .bg(cx.theme().group_box)
                            .child(div().text_sm().child("文本输入与中文 IME"))
                            .child(Input::new(&self.input))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        Button::new("primary-action")
                                            .primary()
                                            .label("执行")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.clicks += 1;
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("secondary-action").label("重置计数").on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.clicks = 0;
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        Checkbox::new("feature-enabled")
                                            .label("启用功能")
                                            .checked(self.enabled)
                                            .on_click(cx.listener(|this, checked, _, cx| {
                                                this.enabled = *checked;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(status),
                            )
                            .child(div().text_sm().child("Select / Tabs / Sidebar / Dialog"))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(Select::new(&self.select).placeholder("选择工作区"))
                                    .child(
                                        Button::new("open-dialog")
                                            .outline()
                                            .label("打开 Dialog")
                                            .on_click(cx.listener(|_, _, window, cx| {
                                                window.open_dialog(cx, |dialog, _, _| {
                                                dialog.title("Nebula 组件 Dialog").alert().child(
                                                    "Dialog、焦点和 Esc 关闭行为由组件库提供。",
                                                )
                                            });
                                            })),
                                    ),
                            )
                            .child(
                                TabBar::new("gallery-tabs")
                                    .w_full()
                                    .selected_index(self.active_tab)
                                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                                        this.active_tab = *index;
                                        cx.notify();
                                    }))
                                    .child(Tab::new().label("终端"))
                                    .child(Tab::new().label("编辑器"))
                                    .child(Tab::new().label("设置")),
                            )
                            .child(
                                h_flex()
                                    .h(px(190.0))
                                    .gap_3()
                                    .child(Sidebar::left().w(px(180.0)).child(
                                        SidebarGroup::new("Nebula").child(
                                            SidebarMenu::new().children([
                                                SidebarMenuItem::new("会话"),
                                                SidebarMenuItem::new("文件"),
                                                SidebarMenuItem::new("设置").active(true),
                                            ]),
                                        ),
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .h_full()
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .rounded_md()
                                            .overflow_hidden()
                                            .child(self.dock_area.clone()),
                                    ),
                            )
                            .child(
                                div().text_sm().child(
                                    "终端垂直切片（ConPTY + nebula_terminal + 自定义 Element）",
                                ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(360.0))
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_md()
                                    .overflow_hidden()
                                    .child(self.terminal.clone()),
                            )
                            .child(div().text_sm().child("Code Editor / Tree-sitter 基础"))
                            .child(Input::new(&self.editor).h(px(220.0)))
                            .child(
                                div()
                                    .id("context-menu-target")
                                    .w_full()
                                    .p_3()
                                    .rounded_md()
                                    .bg(cx.theme().muted)
                                    .child("在此区域点击右键验证 Context Menu")
                                    .context_menu(|menu: PopupMenu, _, _| {
                                        menu.menu("增加计数", Box::new(Increment))
                                    }),
                            ),
                    ),
            )
    }
}
