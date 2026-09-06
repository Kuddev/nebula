use super::*;

impl SettingsPane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = RuntimeSettings::load();
        let language = crate::gpui_shell::config::ui_language(cx);
        let mut selects: Vec<(&'static str, SharedSelect, &'static [&'static str])> = Vec::new();
        let mut subscriptions = Vec::new();

        let mut add_select = |key: &'static str,
                              values: &'static [&'static str],
                              current: &str,
                              window: &mut Window,
                              cx: &mut Context<Self>| {
            let ix = values.iter().position(|v| *v == current).unwrap_or(0);
            let select = cx.new(|cx| {
                SelectState::new(
                    localized_select_labels(key, values, language),
                    Some(IndexPath::default().row(ix)),
                    window,
                    cx,
                )
            });
            subscriptions.push(cx.subscribe_in(
                &select,
                window,
                move |this: &mut Self,
                      entity: &SharedSelect,
                      event: &SelectEvent<Vec<SharedString>>,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
                    if let SelectEvent::Confirm(Some(_)) = event {
                        let row = entity.read(cx).selected_index(cx).map(|path| path.row);
                        if let Some(value) = row.and_then(|row| values.get(row)) {
                            this.persist(&[(key, (*value).to_string())], cx);
                            if key == "language" {
                                this.refresh_localized_controls(window, cx);
                                cx.refresh_windows();
                            }
                        }
                    }
                },
            ));
            selects.push((key, select, values));
        };

        let cursor_current =
            runtime.cursor_shape.map(|shape| shape.settings_value()).unwrap_or("beam");
        let shell_current = runtime.shell.clone().unwrap_or_else(|| "powershell".into());

        add_select(
            "language",
            nebula_settings::LanguagePref::VALUES,
            runtime.language.settings_value(),
            window,
            cx,
        );
        add_select("theme", &THEME_VALUES, runtime.theme.prompt_name(), window, cx);
        // 选项顺序与文案照抄旧壳 `CURSOR_SHAPE_OPTIONS` / `cursor_shape_label`。
        add_select(
            "cursor_shape",
            &["beam", "underline", "block", "hollow"],
            cursor_current,
            window,
            cx,
        );
        add_select(
            "tabs_position",
            &["sidebar", "top"],
            runtime.tabs_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "tab_reveal",
            &["slide", "instant"],
            runtime.tab_reveal.settings_value(),
            window,
            cx,
        );
        add_select(
            "density",
            &["standard", "compact"],
            runtime.density.settings_value(),
            window,
            cx,
        );
        add_select(
            "new_tab_position",
            &["after_current", "end"],
            runtime.new_tab_position.settings_value(),
            window,
            cx,
        );
        add_select(
            "windowing_behavior",
            &["use_new", "use_any_existing", "use_existing"],
            runtime.windowing_behavior.settings_value(),
            window,
            cx,
        );
        add_select(
            "vcs_display",
            &["auto", "git", "svn"],
            runtime.vcs_display.settings_value(),
            window,
            cx,
        );
        add_select(
            "cell_width_mode",
            &["compact", "relaxed"],
            runtime.cell_width_mode.settings_value(),
            window,
            cx,
        );
        // 文案照抄旧壳 `accept_label` / `completion_style_label`。
        add_select(
            "bell",
            &["none", "visual", "audible", "both"],
            runtime.bell.settings_value(),
            window,
            cx,
        );
        // 五档按 DWM 每帧成本排列，不是质量递进；用户按性能预算选择。
        // Aero 是实时玻璃，Acrylic 是实时材质模糊；两者都会采样窗口后方真实内容。
        // Mica / Mica Alt 使用系统壁纸 backdrop，不由 Nebula 读取或重采样。
        add_select(
            "blur",
            &["none", "mica", "mica-alt", "aero", "acrylic"],
            runtime.blur.settings_value(),
            window,
            cx,
        );
        add_select(
            "accept",
            &["right", "tab", "both"],
            runtime.accept.settings_value(),
            window,
            cx,
        );
        add_select(
            "completion_style",
            &["inline", "popup"],
            runtime.completion_style.settings_value(),
            window,
            cx,
        );
        // 壁纸 fit/对齐：存原文，经旧壳 renderer::image 的 parse 归一化
        // （兼容 cover/contain 等别名），展示用规范记号。
        let bgimg_fit = crate::renderer::image::BackgroundImageFit::parse(
            runtime.background_image_fit.as_deref().unwrap_or(""),
        )
        .unwrap_or_default()
        .settings_value();
        // 顺序与文案照抄旧壳 `BACKGROUND_FIT_OPTIONS` / `background_image_fit_label`。
        add_select(
            "background_image_fit",
            &["fill", "uniform", "uniform_to_fill", "none"],
            bgimg_fit,
            window,
            cx,
        );
        let bgimg_align = crate::renderer::image::BackgroundImageAlignment::parse(
            runtime.background_image_alignment.as_deref().unwrap_or(""),
        )
        .unwrap_or_default()
        .settings_value();
        // 九宫格顺序照抄旧壳 `BACKGROUND_ALIGNMENT_OPTIONS`（左上 → 右下）。
        add_select(
            "background_image_alignment",
            &[
                "top_left",
                "top",
                "top_right",
                "left",
                "center",
                "right",
                "bottom_left",
                "bottom",
                "bottom_right",
            ],
            bgimg_align,
            window,
            cx,
        );
        add_select(
            "ssh_proxy_mode",
            &["off", "system", "custom"],
            runtime.ssh_proxy_mode.settings_value(),
            window,
            cx,
        );

        // 与旧壳的默认 Shell 菜单共用检测层：不能在设置页另维护一份两项
        // 白名单，否则 CMD/Nushell/WSL 会出现在新建终端菜单，却无法设为默认。
        // 选项 = 彩色品牌 PNG（extra/shell-icons，与旧壳设置页/命令面板同
        // 一批资产）+ 名称，闭态与下拉同源（SelectItem::display_title/render）。
        let shell_icon_scale = window.scale_factor().max(0.5);
        let (shell_items, shell_index) =
            shell_select_items(&shell_current, shell_icon_scale, language);
        let shell_select = cx.new(|cx| {
            SelectState::new(shell_items, Some(IndexPath::default().row(shell_index)), window, cx)
        });
        subscriptions.push(cx.subscribe_in(
            &shell_select,
            window,
            move |this: &mut Self,
                  _entity: &SharedShellSelect,
                  event: &SelectEvent<Vec<ShellSelectItem>>,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    // 置顶那行是动作不是选项：走导入流程，且不落盘。
                    if id == SHELL_IMPORT_ACTION_ID {
                        this.import_terminal_directory(window, cx);
                    } else {
                        this.persist(&[("shell", id.clone())], cx);
                    }
                }
            },
        ));

        let bg_hex_input = {
            let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
            let rgb = runtime.background.unwrap_or([term.r, term.g, term.b]);
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("#rrggbb")
                    .default_value(format_hex_rgb(rgb))
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 window: &mut Window,
                 cx: &mut Context<Self>| {
                    this.on_bg_hex_event(event, window, cx);
                },
            ));
            input
        };
        let opacity_slider = cx.new(|_| {
            SliderState::new().min(0.00).max(1.00).step(0.05).default_value(runtime.opacity)
        });
        subscriptions.push(cx.subscribe(&opacity_slider, |this, _, event: &SliderEvent, cx| {
            if let SliderEvent::Change(value) = event {
                this.set_opacity(value.start(), cx);
            }
        }));
        let wallpaper_opacity_slider = cx.new(|_| {
            SliderState::new()
                .min(0.05)
                .max(1.00)
                .step(0.05)
                .default_value(runtime.background_image_opacity)
        });
        subscriptions.push(cx.subscribe(
            &wallpaper_opacity_slider,
            |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Change(value) = event {
                    this.set_wallpaper_opacity(value.start(), cx);
                }
            },
        ));
        let (proxy_protocol, proxy_address) =
            crate::display::manual_proxy_parts(&runtime.ssh_proxy_url);
        let proxy_protocol_ix = crate::display::MANUAL_PROXY_PROTOCOL_OPTIONS
            .iter()
            .position(|item| *item == proxy_protocol)
            .unwrap_or(0);
        let proxy_protocol_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from("SOCKS5"), SharedString::from("HTTP")],
                Some(IndexPath::default().row(proxy_protocol_ix)),
                window,
                cx,
            )
        });
        subscriptions.push(cx.subscribe_in(
            &proxy_protocol_select,
            window,
            |this: &mut Self,
             _,
             event: &SelectEvent<Vec<SharedString>>,
             _,
             cx: &mut Context<Self>| {
                if matches!(event, SelectEvent::Confirm(Some(_))) {
                    this.commit_proxy_address(cx);
                }
            },
        ));
        let proxy_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("127.0.0.1:7890")
                .pattern(regex::Regex::new(r"^[^\s]{0,256}$").expect("static regex"))
                .default_value(proxy_address.to_owned())
        });
        subscriptions.push(cx.subscribe_in(
            &proxy_url_input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx: &mut Context<Self>| {
                this.on_proxy_address_event(event, cx);
            },
        ));
        let provider_store = crate::ai_providers::load();
        let active_provider = provider_store
            .providers
            .iter()
            .find(|provider| provider.id == provider_store.active_id)
            .or_else(|| provider_store.providers.first())
            .cloned()
            .unwrap_or_else(|| {
                crate::ai_providers::AiProvider::preset(
                    crate::ai_providers::ProviderKind::Custom,
                    "custom-1",
                )
            });
        let provider_values = [
            active_provider.name,
            active_provider.note,
            active_provider.website_url,
            active_provider.base_url,
            active_provider.model,
        ];
        let provider_placeholders = provider_input_placeholders(language);
        let provider_inputs = provider_values
            .into_iter()
            .zip(provider_placeholders)
            .map(|(value, placeholder)| {
                cx.new(|cx| {
                    InputState::new(window, cx).placeholder(placeholder).default_value(value)
                })
            })
            .collect();

        // 远端备份的非密文槽位输入（容量按最多槽位的协议 S3 = 4）；值在
        // 构造体内按当前协议回填。
        let backup_remote_inputs: Vec<Entity<InputState>> =
            (0..4).map(|_| cx.new(|cx| InputState::new(window, cx))).collect();

        let ssh_username_input = cx.new(|cx| InputState::new(window, cx).placeholder("root"));
        // 地址框现在只承担 host/IP；仍兼容粘贴整段 user@host，由共享 helper
        // 在保存/测试时拆出内嵌用户名。
        let ssh_destination_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("example.com / 192.0.2.1"));
        // 端口键入即过滤：至多 5 位数字（旧壳同规则；范围校验在保存/测试时做）。
        let ssh_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("22")
                .pattern(regex::Regex::new(r"^\d{0,5}$").expect("static regex"))
        });
        let ssh_label_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_label", language))
        });
        let ssh_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(localized_input_placeholder("ssh_password", language))
        });
        let ssh_icon_filter_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_icon_filter", language))
        });
        let ssh_proxy_host_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("127.0.0.1"));
        let ssh_proxy_port_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("1080")
                .pattern(regex::Regex::new(r"^\d{0,5}$").expect("static regex"))
        });
        let ssh_proxy_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_proxy_username", language))
        });
        let ssh_proxy_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(localized_input_placeholder("ssh_proxy_password", language))
        });
        let ssh_jump_host_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(localized_input_placeholder("ssh_jump_host", language))
        });
        for input in [
            ssh_username_input.clone(),
            ssh_destination_input.clone(),
            ssh_port_input.clone(),
            ssh_label_input.clone(),
            ssh_password_input.clone(),
            ssh_proxy_host_input.clone(),
            ssh_proxy_port_input.clone(),
            ssh_proxy_username_input.clone(),
            ssh_proxy_password_input.clone(),
            ssh_jump_host_input.clone(),
        ] {
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, _, cx: &mut Context<Self>| {
                    if matches!(event, InputEvent::Change) {
                        this.touch_ssh_editor(cx);
                    } else if matches!(event, InputEvent::Focus | InputEvent::Blur) {
                        cx.notify();
                    }
                },
            ));
        }
        let font_family_input =
            Self::new_font_family_input(runtime.font_family.clone(), window, cx);
        subscriptions.push(cx.subscribe_in(
            &font_family_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_font_family_input_event(event, window, cx);
            },
        ));

        let bg_picker_hsv = {
            let term = crate::gpui_shell::theme::chrome_theme_resolved(cx).palette().term_bg;
            let rgb = runtime.background.unwrap_or([term.r, term.g, term.b]);
            crate::display::rgb_to_hsv(crate::display::color::Rgb::new(rgb[0], rgb[1], rgb[2]))
        };

        // GPUI 会先匹配 KeyBinding action，再派发元素的 capture/bubble KeyDown。
        // 因此录制 PageDown 等已有快捷键必须在应用级 interceptor 提前截住；
        // 焦点检查保证后台设置标签或普通设置输入不受影响。
        let keymap_interceptor = cx.listener(|this, event: &gpui::KeystrokeEvent, window, cx| {
            if this.keymap_capture.is_some() && this.focus_handle.contains_focused(window, cx) {
                cx.stop_propagation();
                this.handle_keymap_capture(&event.keystroke, cx);
            }
        });
        subscriptions.push(cx.intercept_keystrokes(keymap_interceptor));
        let appearance_interceptor = cx.listener(Self::intercept_appearance_picker);
        subscriptions.push(cx.intercept_keystrokes(appearance_interceptor));

        let settings_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(language.pick(
                "搜索全部设置，例如「字号」「透明度」「更新」",
                "Search all settings, e.g. font, opacity, update",
            ))
        });
        subscriptions.push(cx.subscribe_in(
            &settings_search_input,
            window,
            |_this: &mut Self,
             _: &Entity<InputState>,
             event: &InputEvent,
             _: &mut Window,
             cx: &mut Context<Self>| {
                if matches!(event, InputEvent::Change | InputEvent::Focus | InputEvent::Blur) {
                    cx.notify();
                }
            },
        ));

        Self {
            focus_handle: cx.focus_handle(),
            runtime,
            active_section: 1,
            appearance_picker: None,
            theme_picker_trigger: cx.focus_handle(),
            icon_picker_trigger: cx.focus_handle(),
            expanded_setting_help: std::collections::HashSet::new(),
            about_update: AboutUpdateState::Idle,
            about_update_seq: 0,
            about_last_checked: None,
            settings_search_input,
            settings_search_trigger_bounds: None,
            selects,
            shell_select,
            bg_picker_open: false,
            bg_picker_hsv,
            bg_picker_drag: None,
            bg_hex_input,
            bg_hex_focused: false,
            bg_hex_syncing: false,
            bg_picker_trigger_bounds: None,
            bg_sv_bounds: None,
            bg_hue_bounds: None,
            opacity_slider,
            wallpaper_opacity_slider,
            proxy_url_input,
            proxy_protocol_select,
            proxy_test_seq: 0,
            proxy_test_status: crate::display::ProxyTestStatus::Idle,
            provider_store,
            provider_inputs,
            provider_status: None,
            provider_test_seq: 0,
            provider_test_running: false,
            provider_codex_confirm: None,
            ssh_hosts: crate::gpui_shell::ssh_hosts::SshHostLists::load(),
            ssh_username_input,
            ssh_destination_input,
            ssh_port_input,
            ssh_label_input,
            ssh_password_input,
            ssh_proxy_host_input,
            ssh_proxy_port_input,
            ssh_proxy_username_input,
            ssh_proxy_password_input,
            ssh_jump_host_input,
            ssh_username_picker_open: false,
            ssh_username_trigger_bounds: None,
            ssh_icon_picker_open: false,
            ssh_icon_filter_input,
            ssh_icon_trigger_bounds: None,
            ssh_editor: None,
            ssh_editor_focus_handle: cx.focus_handle(),
            ssh_editor_seq: 0,
            ssh_test_seq: 0,
            ssh_status: None,
            ssh_show_hidden: false,
            ssh_delete_confirm: None,
            ssh_delete_undo: None,
            ssh_undo_seq: 0,
            font_picker_open: false,
            font_loading: false,
            font_system: None,
            font_imported: Vec::new(),
            font_family_input,
            font_picker_trigger_bounds: None,
            backup_selection: crate::encrypted_backup::BackupSelection::default(),
            backup_pass_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(localized_input_placeholder("backup_password", language))
            }),
            backup_status: None,
            backup_busy: false,
            backup_seq: 0,
            backup_remote: {
                let cfg = crate::backup_remote::BackupRemoteConfig::load();
                for (ix, input) in backup_remote_inputs.iter().enumerate() {
                    let value = cfg.slot(ix).unwrap_or_default().to_owned();
                    input.update(cx, |input, cx| input.set_value(value, window, cx));
                }
                cfg
            },
            backup_remote_inputs,
            backup_secret_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(localized_input_placeholder("backup_secret", language))
            }),
            keymap_search_input: {
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(localized_input_placeholder("keymap_search", language))
                });
                subscriptions.push(cx.subscribe_in(
                    &input,
                    window,
                    |_this: &mut Self,
                     _: &Entity<InputState>,
                     event: &InputEvent,
                     _: &mut Window,
                     cx: &mut Context<Self>| {
                        // 搜索词变化只影响可见行集合；捕获态不因打字被打断
                        // （捕获期间焦点在分区根上，输入框收不到键）。
                        if matches!(event, InputEvent::Change) {
                            cx.notify();
                        }
                    },
                ));
                input
            },
            keymap_capture: None,
            keymap_capture_preview: String::new(),
            keymap_binds: nebula_settings::keybind_pairs(),
            slider_persist: None,
            _subscriptions: subscriptions,
        }
    }
}
