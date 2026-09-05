use super::appearance_picker::{AppearanceColors, AppearanceSelection, picker_columns};
use super::*;
use nebula_settings::AppIconName;

pub(super) fn icon_family(icon: AppIconName) -> usize {
    match icon {
        AppIconName::Titanium
        | AppIconName::Porcelain
        | AppIconName::Graphite
        | AppIconName::Monochrome => 1,
        AppIconName::Glacier
        | AppIconName::IceBlue
        | AppIconName::Cobalt
        | AppIconName::MidnightBlue
        | AppIconName::SteelBlue
        | AppIconName::GlassBlue
        | AppIconName::PorcelainBlue
        | AppIconName::DeepBlue => 2,
        AppIconName::MistViolet
        | AppIconName::Violet
        | AppIconName::NightViolet
        | AppIconName::SilverViolet
        | AppIconName::GraphiteViolet => 3,
        AppIconName::IceCyan
        | AppIconName::Cyan
        | AppIconName::PolarCyan
        | AppIconName::CoolMint
        | AppIconName::Emerald
        | AppIconName::NightMint
        | AppIconName::SageLight
        | AppIconName::SageDark => 4,
    }
}

pub(super) fn icon_image(icon: AppIconName, size: f32, window: &Window) -> gpui::Div {
    div().size(px(size)).flex_shrink_0().when_some(
        crate::app_icon::preview(icon, (size * window.scale_factor()).round() as u32),
        |container, image| container.child(img(image).size(px(size))),
    )
}

impl SettingsPane {
    pub(super) fn icon_picker_body(
        &self,
        width: f32,
        compact: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let picker = self.appearance_picker.as_ref().unwrap();
        let AppearanceSelection::Icon(draft) = picker.draft else { unreachable!() };
        let language = crate::gpui_shell::config::ui_language(cx);
        let colors = AppearanceColors::current(cx);
        let columns = picker_columns(false, f32::from(window.viewport_size().width));
        let grid_width = if compact { width } else { width - 174.0 - 25.0 };
        let option_width = (grid_width - (columns - 1) as f32 * 7.0) / columns as f32;
        let grid = h_flex()
            .id("appearance-icon-grid")
            .w(px(grid_width))
            .flex_shrink_0()
            .items_start()
            .flex_wrap()
            .gap_x(px(7.0))
            .gap_y(px(10.0))
            .role(gpui::accesskit::Role::RadioGroup)
            .aria_label(language.pick("应用图标配色", "App icon colors"))
            .children(picker.draft.choices(picker.filter).into_iter().map(|choice| {
                let AppearanceSelection::Icon(icon) = choice else { unreachable!() };
                let selected = draft == icon;
                let content = v_flex()
                    .relative()
                    .w_full()
                    .min_h(px(80.0))
                    .py(px(8.0))
                    .px(px(3.0))
                    .gap(px(7.0))
                    .items_center()
                    .justify_center()
                    .child(icon_image(icon, 42.0, window))
                    .child(
                        div()
                            .max_w_full()
                            .text_size(px(10.0))
                            .truncate()
                            .text_color(if selected { colors.ink } else { colors.secondary })
                            .when(selected, |label| label.font_semibold())
                            .child(choice.label(language)),
                    )
                    .when(selected, |option| {
                        option.child(
                            div()
                                .absolute()
                                .right(px(4.0))
                                .top(px(4.0))
                                .size(px(11.0))
                                .rounded_full()
                                .bg(colors.primary)
                                .text_color(colors.on_primary)
                                .child(Icon::new(IconName::Check).size(px(10.0))),
                        )
                    });
                self.appearance_option(choice, option_width, content, window, cx)
            }));
        let canvas = h_flex()
            .h(px(if compact { 104.0 } else { 171.0 }))
            .justify_center()
            .items_center()
            .border_1()
            .rounded(px(10.0))
            .border_color(gpui::rgb(if picker.dark_preview { 0x3d434d } else { 0xdfe3e9 }))
            .bg(gpui::rgb(if picker.dark_preview { 0x1a1d24 } else { 0xf0f2f5 }))
            .child(icon_image(draft, if compact { 76.0 } else { 104.0 }, window));
        let tabs = h_flex().p(px(3.0)).gap_0().rounded(px(6.0)).bg(colors.subtle).children(
            [false, true].into_iter().map(|dark| {
                Button::new(if dark { "icon-preview-dark" } else { "icon-preview-light" })
                    .label(if dark {
                        language.pick("深色背景", "Dark")
                    } else {
                        language.pick("浅色背景", "Light")
                    })
                    .ghost()
                    .h(px(25.0))
                    .px(px(9.0))
                    .text_size(px(10.0))
                    .rounded(px(4.0))
                    .text_color(colors.secondary)
                    .when(picker.dark_preview == dark, |button| {
                        button.bg(colors.surface).text_color(colors.ink)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(picker) = this.appearance_picker.as_mut() {
                            picker.dark_preview = dark;
                        }
                        cx.notify();
                    }))
            }),
        );
        let title = v_flex()
            .items_center()
            .gap(px(5.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .font_semibold()
                    .child(language.pick(draft.palette().name_zh, draft.palette().name_en)),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(colors.secondary)
                    .child(draft.palette().name_en),
            );
        let sizes = h_flex()
            .justify_center()
            .items_end()
            .gap(px(18.0))
            .pt(px(17.0))
            .mt(px(21.0))
            .border_t_1()
            .border_color(colors.line)
            .children([16.0, 24.0, 32.0].map(|size| {
                v_flex().items_center().gap(px(8.0)).child(icon_image(draft, size, window)).child(
                    div()
                        .text_size(px(9.0))
                        .text_color(colors.muted)
                        .child(format!("{size:.0} px")),
                )
            }));
        let help = div()
            .mt(px(if compact { 9.0 } else { 21.0 }))
            .text_size(px(10.5))
            .line_height(gpui::relative(1.7))
            .text_color(colors.secondary)
            .child(language.pick(
                "这里只切换预览背景。图标不会跟随主题自动变化。",
                "This only changes the preview background. Your icon does not follow the theme.",
            ));
        let preview = if compact {
            h_flex()
                .w_full()
                .gap(px(16.0))
                .items_start()
                .child(canvas.w(px(104.0)).flex_shrink_0())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(title)
                        .child(div().mt(px(9.0)).child(tabs))
                        .child(help),
                )
        } else {
            v_flex()
                .w(px(174.0))
                .flex_shrink_0()
                .child(canvas)
                .child(h_flex().mt(px(13.0)).justify_center().child(tabs))
                .child(div().mt(px(19.0)).child(title))
                .child(sizes)
                .child(help)
        };
        if compact {
            v_flex().w(px(width)).gap(px(20.0)).child(preview).child(grid)
        } else {
            h_flex().w(px(width)).items_start().gap(px(25.0)).child(grid).child(preview)
        }
    }
}
