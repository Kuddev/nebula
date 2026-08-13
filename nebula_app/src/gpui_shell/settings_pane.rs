//! 设置页（GPUI 组件版）。
//!
//! 薄壳纪律：本文件不定义任何设置语义——键名、值域、主题色表、持久化
//! 格式全部在共享 crate `nebula-settings`（从旧壳逐字迁移而来）。这里只
//! 负责用组件库把字段摆出来；改动经 `persist_keys` 原地写回
//! `nebula_settings.txt`，与旧壳读写同一份文件、同一套语义。
//!
//! 生效时机（对齐旧壳）：主题/配色、copy_on_select、字号即时生效（宿主
//! 收 `Changed` 事件后热应用）；字体族与默认光标形状对新标签页生效。

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Window, div, px,
};
use gpui_component::select::SelectEvent;
use nebula_settings::{CursorShapeName, RuntimeSettings, ThemeName, persist_keys};

use crate::gpui_shell::prelude::*;

/// 主题下拉的选项顺序（展示名 = 持久化名，与旧壳一致）。
const THEMES: [ThemeName; 7] = [
    ThemeName::Nebula,
    ThemeName::SilverLight,
    ThemeName::SteelDark,
    ThemeName::LimestoneLight,
    ThemeName::CoalDark,
    ThemeName::LinenLight,
    ThemeName::MossDark,
];

const CURSOR_SHAPES: [(&str, &str); 3] =
    [("block", "块状"), ("beam", "竖线"), ("underline", "下划线")];

/// 宿主（workspace）监听：设置已写盘，全局 `Settings` 已重载。
pub enum SettingsPaneEvent {
    Changed,
}

pub struct SettingsPane {
    focus_handle: FocusHandle,
    theme_select: Entity<SelectState<Vec<SharedString>>>,
    cursor_select: Entity<SelectState<Vec<SharedString>>>,
    cursor_blink: bool,
    copy_on_select: bool,
    powerline: bool,
    font_family: SharedString,
    font_size_pt: f32,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = RuntimeSettings::load();

        let theme_ix = THEMES.iter().position(|t| *t == runtime.theme).unwrap_or(0);
        let theme_select = cx.new(|cx| {
            SelectState::new(
                THEMES.iter().map(|t| SharedString::from(t.prompt_name())).collect::<Vec<_>>(),
                Some(IndexPath::default().row(theme_ix)),
                window,
                cx,
            )
        });

        let cursor_ix = match runtime.cursor_shape {
            Some(CursorShapeName::Block) => 0,
            // 旧壳出厂默认 beam；未设置时展示 beam。
            Some(CursorShapeName::Beam) | None => 1,
            Some(CursorShapeName::Underline) => 2,
        };
        let cursor_select = cx.new(|cx| {
            SelectState::new(
                CURSOR_SHAPES
                    .iter()
                    .map(|(_, label)| SharedString::from(*label))
                    .collect::<Vec<_>>(),
                Some(IndexPath::default().row(cursor_ix)),
                window,
                cx,
            )
        });

        let subscriptions = vec![
            cx.subscribe_in(&theme_select, window, Self::on_theme_confirm),
            cx.subscribe_in(&cursor_select, window, Self::on_cursor_confirm),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            theme_select,
            cursor_select,
            cursor_blink: runtime.cursor_blink.unwrap_or(true),
            copy_on_select: runtime.copy_on_select,
            powerline: runtime.powerline,
            font_family: runtime.font_family.unwrap_or_default().into(),
            font_size_pt: runtime.font_size_pt.unwrap_or(11.25),
            _subscriptions: subscriptions,
        }
    }

    fn on_theme_confirm(
        &mut self,
        _: &Entity<SelectState<Vec<SharedString>>>,
        event: &SelectEvent<Vec<SharedString>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(value)) = event {
            if let Some(theme) = ThemeName::from_prompt_name(value.as_ref()) {
                self.persist(&[("theme", theme.prompt_name().to_owned())], cx);
            }
        }
    }

    fn on_cursor_confirm(
        &mut self,
        _: &Entity<SelectState<Vec<SharedString>>>,
        event: &SelectEvent<Vec<SharedString>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let SelectEvent::Confirm(Some(value)) = event {
            if let Some((key, _)) = CURSOR_SHAPES.iter().find(|(_, label)| *label == value.as_ref())
            {
                self.persist(&[("cursor_shape", (*key).to_owned())], cx);
            }
        }
    }

    /// 写盘 → 重载全局 `Settings` → 通知宿主热应用。
    fn persist(&mut self, updates: &[(&str, String)], cx: &mut Context<Self>) {
        if let Err(err) = persist_keys(updates) {
            eprintln!("[nebula:gpui] failed to persist settings: {err}");
        }
        let settings = crate::gpui_shell::config::Settings::load();
        cx.set_global(settings);
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.font_size_pt = size.clamp(4.0, 96.0);
        self.persist(&[("font_size", format!("{:.2}", self.font_size_pt))], cx);
    }

    /// 分组卡片（对齐设置页的分组式版面）。
    fn group(&self, title: &'static str, cx: &Context<Self>) -> gpui::Div {
        v_flex()
            .w_full()
            .max_w(px(680.0))
            .gap_3()
            .p_5()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .bg(cx.theme().group_box)
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(title))
    }

    fn row(label: &'static str, control: impl IntoElement) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .child(div().flex_1().text_sm().child(label))
            .child(control)
    }
}

impl EventEmitter<SettingsPaneEvent> for SettingsPane {}

impl Focusable for SettingsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_label: SharedString = if self.font_family.is_empty() {
            "（跟随 nebula.toml / 内置默认）".into()
        } else {
            self.font_family.clone()
        };
        let font_size: SharedString = format!("{:.1} pt", self.font_size_pt).into();

        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                v_flex()
                    .size_full()
                    .p_8()
                    .gap_5()
                    .overflow_y_scrollbar()
                    .child(
                        div().text_xl().font_weight(gpui::FontWeight::SEMIBOLD).child("设置"),
                    )
                    .child(
                        self.group("外观", cx)
                            .child(Self::row(
                                "主题",
                                div().w(px(220.0)).child(Select::new(&self.theme_select)),
                            ))
                            .child(Self::row(
                                "字号",
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Button::new("font-size-down")
                                            .label("−")
                                            .small()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_font_size(this.font_size_pt - 0.5, cx);
                                            })),
                                    )
                                    .child(div().text_sm().min_w(px(64.0)).child(font_size))
                                    .child(
                                        Button::new("font-size-up")
                                            .label("+")
                                            .small()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_font_size(this.font_size_pt + 0.5, cx);
                                            })),
                                    ),
                            ))
                            .child(Self::row(
                                "字体",
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(font_label),
                            ))
                            .child(Self::row(
                                "光标形状（新标签页生效）",
                                div().w(px(220.0)).child(Select::new(&self.cursor_select)),
                            ))
                            .child(Self::row(
                                "光标闪烁",
                                Checkbox::new("cursor-blink").checked(self.cursor_blink).on_click(
                                    cx.listener(|this, checked: &bool, _, cx| {
                                        this.cursor_blink = *checked;
                                        this.persist(
                                            &[("cursor_blink", (*checked as u8).to_string())],
                                            cx,
                                        );
                                    }),
                                ),
                            )),
                    )
                    .child(
                        self.group("交互", cx)
                            .child(Self::row(
                                "选中即复制（关 = 右键复制/粘贴）",
                                Checkbox::new("copy-on-select")
                                    .checked(self.copy_on_select)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.copy_on_select = *checked;
                                        this.persist(
                                            &[("copy_on_select", (*checked as u8).to_string())],
                                            cx,
                                        );
                                    })),
                            ))
                            .child(Self::row(
                                "Powerline 提示符（新会话生效）",
                                Checkbox::new("powerline").checked(self.powerline).on_click(
                                    cx.listener(|this, checked: &bool, _, cx| {
                                        this.powerline = *checked;
                                        this.persist(
                                            &[("powerline", (*checked as u8).to_string())],
                                            cx,
                                        );
                                    }),
                                ),
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("写入 nebula_settings.txt，与旧壳共享同一份设置。"),
                    ),
            )
    }
}
