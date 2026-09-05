use super::*;

impl SettingsPane {
    pub(super) fn active_provider_index(&self) -> Option<usize> {
        self.provider_store
            .providers
            .iter()
            .position(|provider| provider.id == self.provider_store.active_id)
            .or_else(|| (!self.provider_store.providers.is_empty()).then_some(0))
    }

    pub(super) fn sync_provider_inputs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        let draft =
            crate::ai_providers::ProviderMetadataDraft::from(&self.provider_store.providers[index]);
        let values = [draft.name, draft.note, draft.website_url, draft.base_url, draft.model];
        for (input, value) in self.provider_inputs.iter().zip(values) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
    }

    pub(super) fn select_provider(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.provider_store.providers.iter().any(|provider| provider.id == id) {
            self.provider_store.active_id = id;
            let _ = crate::ai_providers::save(&self.provider_store);
            self.provider_codex_confirm = None;
            self.provider_status = None;
            self.sync_provider_inputs(window, cx);
            cx.notify();
        }
    }

    pub(super) fn save_provider_metadata(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.active_provider_index() else { return false };
        let values: Vec<String> =
            self.provider_inputs.iter().map(|input| input.read(cx).value().to_string()).collect();
        let draft = crate::ai_providers::ProviderMetadataDraft {
            name: values[0].clone(),
            note: values[1].clone(),
            website_url: values[2].clone(),
            base_url: values[3].clone(),
            model: values[4].clone(),
        };
        crate::ai_providers::apply_metadata_draft(&mut self.provider_store.providers[index], draft);
        match crate::ai_providers::save(&self.provider_store) {
            Ok(()) => {
                self.provider_status = Some(ProviderStatus::Saved);
                true
            },
            Err(error) => {
                self.provider_status = Some(ProviderStatus::Error(error.to_string()));
                false
            },
        }
    }

    pub(super) fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_provider_metadata(cx);
        let id = crate::ai_providers::next_custom_id(&self.provider_store);
        self.provider_store.providers.push(crate::ai_providers::AiProvider::preset(
            crate::ai_providers::ProviderKind::Custom,
            &id,
        ));
        self.provider_store.active_id = id;
        self.provider_codex_confirm = None;
        match crate::ai_providers::save(&self.provider_store) {
            Ok(()) => self.provider_status = Some(ProviderStatus::Added),
            Err(error) => self.provider_status = Some(ProviderStatus::Error(error.to_string())),
        }
        self.sync_provider_inputs(window, cx);
        cx.notify();
    }

    pub(super) fn delete_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        if self.provider_store.providers.len() <= 1 {
            self.provider_status = Some(ProviderStatus::AtLeastOneRequired);
            cx.notify();
            return;
        }
        let id = self.provider_store.providers[index].id.clone();
        match crate::ai_providers::remove_provider(&mut self.provider_store, &id) {
            Ok(()) => {
                self.provider_status = Some(ProviderStatus::Deleted);
                self.provider_codex_confirm = None;
                self.sync_provider_inputs(window, cx);
            },
            Err(error) => self.provider_status = Some(ProviderStatus::Error(error.to_string())),
        }
        cx.notify();
    }

    pub(super) fn toggle_provider_flag(
        &mut self,
        flag: &'static str,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.active_provider_index() else { return };
        let provider = &mut self.provider_store.providers[index];
        match flag {
            "enabled" => provider.enabled = value,
            "codex_goals" => provider.codex_goals = value,
            "codex_remote_compaction" => provider.codex_remote_compaction = value,
            _ => return,
        }
        self.provider_codex_confirm = None;
        if let Err(error) = crate::ai_providers::save(&self.provider_store) {
            self.provider_status = Some(ProviderStatus::Error(error.to_string()));
        }
        cx.notify();
    }

    pub(super) fn prompt_provider_key(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.active_provider_index() else { return };
        let provider = &mut self.provider_store.providers[index];
        match crate::ai_providers::prompt_and_store_api_key(provider) {
            Ok(true) => {
                self.provider_status = Some(ProviderStatus::ApiKeySaved);
                if let Err(error) = crate::ai_providers::save(&self.provider_store) {
                    self.provider_status = Some(ProviderStatus::Error(error.to_string()));
                }
            },
            Ok(false) => {},
            Err(error) => self.provider_status = Some(ProviderStatus::Error(error.to_string())),
        }
        cx.notify();
    }

    pub(super) fn test_provider(&mut self, cx: &mut Context<Self>) {
        if self.provider_test_running || !self.save_provider_metadata(cx) {
            return;
        }
        let Some(index) = self.active_provider_index() else { return };
        let provider = self.provider_store.providers[index].clone();
        self.provider_test_seq = self.provider_test_seq.wrapping_add(1);
        let sequence = self.provider_test_seq;
        self.provider_test_running = true;
        self.provider_status = Some(ProviderStatus::Testing);

        let task = cx
            .background_executor()
            .spawn(async move { crate::ai_providers::test_provider(&provider) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                if sequence != pane.provider_test_seq
                    || result.provider_id != pane.provider_store.active_id
                {
                    return;
                }
                pane.provider_test_running = false;
                pane.provider_status = Some(ProviderStatus::TestResult {
                    outcome: result.outcome,
                    elapsed_ms: result.elapsed_ms,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn apply_provider_to_codex(&mut self, cx: &mut Context<Self>) {
        if !self.save_provider_metadata(cx) {
            return;
        }
        let Some(index) = self.active_provider_index() else { return };
        let provider = self.provider_store.providers[index].clone();
        if self.provider_codex_confirm.as_deref() != Some(provider.id.as_str()) {
            self.provider_codex_confirm = Some(provider.id);
            self.provider_status = Some(ProviderStatus::CodexConfirmation);
            cx.notify();
            return;
        }
        self.provider_codex_confirm = None;
        self.provider_status = Some(match crate::codex_config::apply_provider(&provider) {
            Ok(path) => ProviderStatus::AppliedToCodex(path),
            Err(error) => ProviderStatus::Error(error),
        });
        cx.notify();
    }

    // ---- 分区内容（归属对照旧壳各 section）----
    pub(super) fn section_providers(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let hover_bg = crate::gpui_shell::theme::settings_hover_bg(cx, false);
        let active_index = self.active_provider_index().unwrap_or(0);
        let active = self.provider_store.providers.get(active_index).cloned();
        let active_id = active.as_ref().map(|provider| provider.id.clone()).unwrap_or_default();
        let provider_rows = self.provider_store.providers.iter().map(|provider| {
            let id = provider.id.clone();
            let selected = provider.id == active_id;
            let name = provider.name.clone();
            let kind = provider.kind.label();
            h_flex()
                .id(SharedString::from(format!("provider-row-{}", provider.id)))
                .h(px(34.0))
                .w_full()
                .px_2()
                .gap_2()
                .items_center()
                .rounded_md()
                .when(selected, |row| row.bg(theme.list_active))
                .hover(move |row| row.bg(hover_bg))
                .child(Icon::new(IconName::Bot).xsmall().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(
                    div()
                        .max_w(px(78.0))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(kind),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_provider(id.clone(), window, cx);
                }))
        });

        let mut editor = v_flex().flex_1().min_w_0().gap_3();
        if let Some(provider) = active {
            let key_status: SharedString = if provider.api_key_set {
                if provider.api_key_hint.is_empty() {
                    language
                        .pick("已保存在系统凭据管理器", "Saved in the system credential manager")
                        .into()
                } else {
                    provider.api_key_hint.clone().into()
                }
            } else if provider.kind.requires_api_key() {
                language.pick("未设置", "Not set").into()
            } else {
                language
                    .pick("此供应商不需要 API Key", "This provider does not require an API key")
                    .into()
            };
            let enabled = provider.enabled;
            let goals = provider.codex_goals;
            let remote = provider.codex_remote_compaction;
            editor = editor
                .child(
                    self.row(
                        language.pick("启用", "Enabled"),
                        language.pick(
                            "关掉后这个供应商不再出现在 AI 启动器里，配置和密钥都留着，随时可以开回来。",
                            "When disabled, this provider is hidden from the AI launcher. Its settings and key are kept so it can be enabled again later.",
                        ),
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-enabled")
                            .checked(enabled)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("enabled", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(self.row(
                    language.pick("名称", "Name"),
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[0])),
                    cx,
                ))
                .child(self.row(
                    language.pick("备注", "Note"),
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[1])),
                    cx,
                ))
                .child(self.row(
                    language.pick("官方网站", "Official website"),
                    "",
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[2])),
                    cx,
                ))
                .child(self.row(
                    language.pick("API 请求地址", "API endpoint"),
                    language.pick(
                        "供应商的 API 根地址，多数以 `/v1` 结尾。这里填错不会在保存时报错，而是等到第一次对话请求才失败。",
                        "The provider's API base URL, usually ending in `/v1`. An invalid address is detected by the first conversation request, not when these settings are saved.",
                    ),
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[3])),
                    cx,
                ))
                .child(self.row(
                    language.pick("默认模型", "Default model"),
                    language.pick(
                        "新会话默认用的模型名，按供应商文档里的写法逐字填——名字对不上时同样是发起请求那一刻才报错。",
                        "The model name used for new conversations. Enter it exactly as documented by the provider; an invalid name is detected when a request is sent.",
                    ),
                    div().w(px(330.0)).child(Input::new(&self.provider_inputs[4])),
                    cx,
                ))
                .child(
                    self.row(
                        "API Key",
                        language.pick(
                            "密钥不写进设置文件，这里只显示它是否已经设过。换一把新的直接点替换，旧的会被覆盖。",
                            "The key is not written to the settings file. This row only shows whether one is stored; replacing it overwrites the old key.",
                        ),
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(key_status),
                            )
                            .child(
                                NebulaButton::new("provider-set-key")
                                    .label(if provider.api_key_set {
                                        language.pick("替换…", "Replace...")
                                    } else {
                                        language.pick("设置…", "Set...")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.prompt_provider_key(cx);
                                    })),
                            ),
                        cx,
                    ),
                )
                .child(
                    self.row(
                        "Codex Goals",
                        language.pick(
                            "写进 `~/.codex/config.toml` 的 `features.goals`。这是 Codex 自己的特性开关，Pebrel 只负责把它落到配置里，其它供应商不受影响。",
                            "Writes `features.goals` to `~/.codex/config.toml`. This is a Codex feature flag; Pebrel only persists it and other providers are unaffected.",
                        ),
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-goals")
                            .checked(goals)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_goals", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(
                    self.row(
                        language.pick("Codex 远程压缩", "Codex remote compaction"),
                        language.pick(
                            "同上，对应 `features.remote_compaction_v2`。同样只在用 Codex 时生效。",
                            "As above, this controls `features.remote_compaction_v2` and only applies to Codex.",
                        ),
                        crate::gpui_shell::widgets::NebulaSwitch::new("provider-codex-remote")
                            .checked(remote)
                            .on_click(cx.listener(|this, value: &bool, _, cx| {
                                this.toggle_provider_flag("codex_remote_compaction", *value, cx);
                            })),
                        cx,
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(NebulaButton::new("provider-save").label(language.pick("保存", "Save")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.save_provider_metadata(cx);
                                cx.notify();
                            }),
                        ))
                        .child(
                            NebulaButton::new("provider-test")
                                .label(if self.provider_test_running {
                                    language.pick("测试中…", "Testing...")
                                } else {
                                    language.pick("测试连接", "Test connection")
                                })
                                .disabled(self.provider_test_running)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.test_provider(cx);
                                })),
                        )
                        .child(NebulaButton::new("provider-codex").label(language.pick("应用到 Codex", "Apply to Codex")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.apply_provider_to_codex(cx);
                            }),
                        ))
                        .child(
                            NebulaButton::new("provider-delete").label(language.pick("删除", "Delete")).danger().on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.delete_provider(window, cx);
                                }),
                            ),
                        ),
                );
        } else {
            editor = editor.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(language.pick("没有供应商配置", "No provider configured")),
            );
        }

        self.group(language.pick("供应商", "Providers"), cx)
            .child(
                h_flex()
                    .w_full()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .w(px(210.0))
                            .flex_shrink_0()
                            .h(px(420.0))
                            .gap_1()
                            .overflow_y_scrollbar()
                            .children(provider_rows)
                            .child(
                                NebulaButton::new("provider-add")
                                    .label(language.pick("+ 自定义供应商", "+ Custom provider"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_provider(window, cx);
                                    })),
                            ),
                    )
                    .child(editor),
            )
            .when_some(self.provider_status.clone(), |group, status| {
                let error = status.is_error();
                let message = status.text(language);
                group.child(
                    div()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            })
    }
}
