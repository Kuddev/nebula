use super::*;
use nebula_settings::AppIconName;

impl SettingsPane {
    pub(super) fn app_icon_previews(&self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().primary;
        let border = cx.theme().border;
        let cards = h_flex().w_full().flex_wrap().gap(px(16.0)).children(
            AppIconName::ALL.into_iter().map(|variant| {
                let selected = crate::app_icon::selected() == variant;
                let label = match variant {
                    AppIconName::SilverViolet => {
                        language.pick("银紫 · 默认", "Silver violet · Default")
                    },
                    AppIconName::GraphiteViolet => language.pick("石墨紫", "Graphite violet"),
                    AppIconName::Titanium => language.pick("钛银", "Titanium"),
                };
                let preview = h_flex().w_full().children([0xF1F3F6, 0x17191F].map(|background| {
                    h_flex()
                        .flex_1()
                        .h(px(68.0))
                        .justify_center()
                        .items_center()
                        .bg(gpui::rgb(background))
                        .when_some(crate::app_icon::preview(variant), |surface, logo| {
                            surface.child(img(logo).size(px(40.0)).flex_shrink_0())
                        })
                }));
                v_flex()
                    .w(px(190.0))
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .rounded_lg()
                            .overflow_hidden()
                            .border_1()
                            .border_color(if selected { accent } else { border })
                            .child(preview),
                    )
                    .child(div().text_sm().child(label))
                    .child(
                        NebulaButton::new(format!("app-icon-{}", variant.settings_value()))
                            .label(if selected {
                                language.pick("已选择", "Selected")
                            } else {
                                language.pick("使用此图标", "Use this icon")
                            })
                            .when(selected, |button| button.primary())
                            .disabled(selected)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.persist(
                                    &[("app_icon", variant.settings_value().to_owned())],
                                    cx,
                                );
                            })),
                    )
            }),
        );
        let description = if cfg!(windows) {
            language.pick(
                "即时切换窗口、Alt+Tab、托盘和应用内图标，并记住选择。已固定的快捷方式、EXE 和安装器仍使用默认主图标。图标不随终端主题自动变化。",
                "Updates window, Alt+Tab, tray and in-app icons immediately and remembers your choice. Pinned shortcuts, the EXE and installer keep the default icon. The icon does not follow the terminal theme.",
            )
        } else {
            language.pick(
                "切换应用内图标，并记住选择。当前不会更改系统 Dock 或应用启动器图标。",
                "Changes the in-app icon and remembers your choice. System Dock and application launcher icons are not changed yet.",
            )
        };
        self.group(language.pick("应用图标", "App icon"), cx)
            .child(cards)
            .child(div().mt_3().text_sm().text_color(muted).child(description))
    }
}
