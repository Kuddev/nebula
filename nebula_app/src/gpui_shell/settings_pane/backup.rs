use super::*;

impl SettingsPane {
    // ---- 备份（本地加密导出/恢复 + 远端同步）----

    /// 读备份密码并预检（真正的强度校验在 `encrypted_backup` 内部再做一
    /// 次；这里只提前给出友好提示）。
    pub(super) fn backup_passphrase(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let pass = self.backup_pass_input.read(cx).value().to_string();
        if pass.chars().count() < 8 {
            self.backup_status = Some(BackupStatus::PassphraseTooShort);
            cx.notify();
            return None;
        }
        Some(pass)
    }

    /// 后台任务只返回语义结果，显示语言在渲染时决定，切换语言不会留下旧文案。
    pub(super) fn backup_run_async(
        &mut self,
        task: impl std::future::Future<Output = Result<BackupCompletion, String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.backup_seq = self.backup_seq.wrapping_add(1);
        let seq = self.backup_seq;
        self.backup_busy = true;
        self.backup_status = Some(BackupStatus::Processing);
        let task = cx.background_executor().spawn(task);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |pane, cx| {
                if seq != pane.backup_seq {
                    return;
                }
                pane.backup_busy = false;
                pane.backup_status = Some(match result {
                    Ok(completion) => BackupStatus::Completed(completion),
                    Err(error) => BackupStatus::Error(error),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 导出：先弹保存对话框（取消则零成本），再后台 collect + seal + 写盘。
    pub(super) fn export_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        if self.backup_selection.is_empty() {
            self.backup_status = Some(BackupStatus::SelectionRequired);
            cx.notify();
            return;
        }
        let selection = self.backup_selection;
        let start_dir = std::env::var_os("USERPROFILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let picked =
            cx.prompt_for_new_path(&start_dir, Some(&format!("nebula-{stamp}.nebula-backup")));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let _ = this.update(cx, |pane, cx| {
                pane.backup_run_async(
                    async move {
                        let archive = crate::encrypted_backup::collect(selection)?;
                        let packet = crate::encrypted_backup::seal(&archive, &pass)?;
                        std::fs::write(&path, packet)
                            .map_err(|err| format!("写入备份文件失败：{err}"))?;
                        Ok(BackupCompletion::Exported(path))
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// 恢复：选文件 → 后台解密落盘 → 热应用（设置/主机列表随之重载）。
    pub(super) fn restore_backup(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                crate::gpui_shell::config::ui_language(cx)
                    .pick("选择 Pebrel 加密备份", "Select an encrypted Pebrel backup")
                    .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |pane, cx| {
                pane.backup_run_async(
                    async move {
                        let packet = std::fs::read(&path)
                            .map_err(|err| format!("读取备份文件失败：{err}"))?;
                        crate::encrypted_backup::restore(&packet, &pass)?;
                        Ok(BackupCompletion::Restored)
                    },
                    cx,
                );
                // 恢复覆盖了 settings/主机列表等文件：立即重载并通知宿主
                // 热应用。后台任务完成前 UI 短暂显示旧值，可接受。
                pane.reload_after_restore(cx);
            });
        })
        .detach();
    }

    /// 恢复后的单一重载入口（设置/SSH 主机/远端配置）。
    pub(super) fn reload_after_restore(&mut self, cx: &mut Context<Self>) {
        self.runtime = RuntimeSettings::load();
        self.ssh_hosts = crate::gpui_shell::ssh_hosts::SshHostLists::load();
        self.backup_remote = crate::backup_remote::BackupRemoteConfig::load();
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        cx.set_global(settings);
        cx.emit(SettingsPaneEvent::Changed);
        cx.notify();
    }

    /// 远端配置写盘（读当前协议的非密文槽位输入）。
    pub(super) fn save_remote_config(&mut self, cx: &mut Context<Self>) {
        let protocol = self.backup_remote.protocol;
        let secret_slot = crate::backup_remote::secret_field(protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            let Some(input) = self.backup_remote_inputs.get(input_ix) else { break };
            let value = input.read(cx).value().trim().to_string();
            self.backup_remote.set_slot(slot, value);
            input_ix += 1;
        }
        self.backup_status = Some(match self.backup_remote.save() {
            Ok(()) => BackupStatus::RemoteConfigSaved,
            Err(err) => BackupStatus::Error(err),
        });
        cx.notify();
    }

    /// 密文槽写入系统凭据管理器（WebDAV 密码 / S3 Secret），随后清空输入。
    pub(super) fn store_remote_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_remote_config(cx);
        let secret = self.backup_secret_input.read(cx).value().to_string();
        if secret.is_empty() {
            self.backup_status = Some(BackupStatus::CredentialEmpty);
            cx.notify();
            return;
        }
        let result = match self.backup_remote.protocol {
            crate::backup_remote::BackupProtocol::WebDav => {
                crate::backup_remote::store_webdav_password(
                    &self.backup_remote.webdav_username,
                    &secret,
                )
            },
            crate::backup_remote::BackupProtocol::S3 => {
                crate::backup_remote::store_s3_secret(&self.backup_remote.s3_access_key, &secret)
            },
            _ => {
                self.backup_status = Some(BackupStatus::CredentialUnsupported);
                cx.notify();
                return;
            },
        };
        self.backup_status = Some(match result {
            Ok(()) => BackupStatus::CredentialSaved,
            Err(err) => BackupStatus::Error(err),
        });
        self.backup_secret_input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// 立即推送：collect + seal + 按配置协议上传（全部后台）。
    pub(super) fn push_remote(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        self.save_remote_config(cx);
        if self.backup_selection.is_empty() {
            self.backup_status = Some(BackupStatus::SelectionRequired);
            cx.notify();
            return;
        }
        let selection = self.backup_selection;
        self.backup_run_async(
            async move {
                crate::backup_remote::validate()?;
                let archive = crate::encrypted_backup::collect(selection)?;
                let packet = crate::encrypted_backup::seal(&archive, &pass)?;
                let location = crate::backup_remote::push(&packet)?;
                Ok(BackupCompletion::Pushed(location))
            },
            cx,
        );
    }

    /// 恢复最新：按配置协议拉取最新备份并解密落盘（全部后台）。
    pub(super) fn pull_remote(&mut self, cx: &mut Context<Self>) {
        if self.backup_busy {
            return;
        }
        let Some(pass) = self.backup_passphrase(cx) else { return };
        self.save_remote_config(cx);
        self.backup_run_async(
            async move {
                crate::backup_remote::validate()?;
                let (name, packet) = crate::backup_remote::pull_latest()?;
                crate::encrypted_backup::restore(&packet, &pass)?;
                Ok(BackupCompletion::Pulled(name))
            },
            cx,
        );
    }

    /// 协议切换：字段独立保存（来回切换不丢配置），槽位输入随协议回填。
    pub(super) fn select_backup_protocol(
        &mut self,
        protocol: crate::backup_remote::BackupProtocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 先把当前协议的输入落到内存配置，再切换（不写盘，写盘归保存钮）。
        let secret_slot = crate::backup_remote::secret_field(self.backup_remote.protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(self.backup_remote.protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            if let Some(input) = self.backup_remote_inputs.get(input_ix) {
                let value = input.read(cx).value().trim().to_string();
                self.backup_remote.set_slot(slot, value);
            }
            input_ix += 1;
        }
        self.backup_remote.protocol = protocol;
        let secret_slot = crate::backup_remote::secret_field(protocol);
        let mut input_ix = 0usize;
        for slot in 0..crate::backup_remote::field_count(protocol) {
            if Some(slot) == secret_slot {
                continue;
            }
            let value = self.backup_remote.slot(slot).unwrap_or_default().to_owned();
            if let Some(input) = self.backup_remote_inputs.get(input_ix) {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
            input_ix += 1;
        }
        cx.notify();
    }

    pub(super) fn section_backup(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        use crate::backup_remote::BackupProtocol;
        let language = crate::gpui_shell::config::ui_language(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let busy = self.backup_busy;
        let selection = self.backup_selection;

        // 类别开关（与共享 `BackupSelection` 一一对应）。
        let categories: [(&'static str, &'static str, bool, fn(&mut Self, bool)); 9] = [
            (
                "bk-appearance",
                language.pick("外观与主题", "Appearance and themes"),
                selection.appearance,
                |s, v| {
                    s.backup_selection.appearance = v;
                },
            ),
            (
                "bk-config",
                language.pick("终端配置", "Terminal configuration"),
                selection.config,
                |s, v| s.backup_selection.config = v,
            ),
            (
                "bk-ssh",
                language.pick("SSH 主机（脱敏）", "SSH hosts (redacted)"),
                selection.ssh,
                |s, v| s.backup_selection.ssh = v,
            ),
            (
                "bk-sync",
                language.pick("同步配置", "Sync configuration"),
                selection.sync,
                |s, v| s.backup_selection.sync = v,
            ),
            (
                "bk-assistant",
                language.pick("AI 助手配置", "AI assistant configuration"),
                selection.assistant,
                |s, v| {
                    s.backup_selection.assistant = v;
                },
            ),
            (
                "bk-session",
                language.pick("会话与工作区", "Sessions and workspaces"),
                selection.session,
                |s, v| {
                    s.backup_selection.session = v;
                },
            ),
            (
                "bk-dirhist",
                language.pick("目录历史", "Directory history"),
                selection.directory_history,
                |s, v| {
                    s.backup_selection.directory_history = v;
                },
            ),
            (
                "bk-cmdhist",
                language.pick("命令历史", "Command history"),
                selection.command_history,
                |s, v| {
                    s.backup_selection.command_history = v;
                },
            ),
            (
                "bk-fonts",
                language.pick("自装字体", "Installed fonts"),
                selection.fonts,
                |s, v| s.backup_selection.fonts = v,
            ),
        ];
        // 这九行是一份勾选清单，不逐项写说明：清单的价值在于一眼扫完，
        // 每行挂两行小字会把它撑成九屏。范围与边界由组顶部那句统一交代
        // （端到端加密、私钥永不进包）。
        let category_rows = categories.map(|(id, label, checked, apply)| {
            self.row(
                label,
                "",
                crate::gpui_shell::widgets::NebulaSwitch::new(id).checked(checked).on_click(
                    cx.listener(move |this, value: &bool, _, cx| {
                        apply(this, *value);
                        cx.notify();
                    }),
                ),
                cx,
            )
        });

        let protocol = self.backup_remote.protocol;
        let protocol_buttons = [
            (BackupProtocol::Off, language.pick("关闭", "Off")),
            (BackupProtocol::Folder, language.pick("目录", "Folder")),
            (BackupProtocol::WebDav, "WebDAV"),
            (BackupProtocol::S3, "S3"),
            (BackupProtocol::Sftp, "SFTP"),
        ]
        .map(|(value, label)| {
            let selected = value == protocol;
            Button::new(SharedString::from(format!("bk-protocol-{}", value.settings_value())))
                .label(label)
                .small()
                .when(selected, |b| b.primary())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_backup_protocol(value, window, cx);
                }))
        });

        // 非密文槽位标签（顺序 = `BackupRemoteConfig::slot` 的下标序）。
        let slot_labels: &[&'static str] = match protocol {
            BackupProtocol::Off => &[],
            BackupProtocol::Folder => &[language.pick("目标目录", "Target folder")],
            BackupProtocol::WebDav => &[
                language.pick("WebDAV 地址", "WebDAV address"),
                language.pick("用户名", "Username"),
            ],
            BackupProtocol::S3 => {
                &["Endpoint", "Region", language.pick("桶/前缀", "Bucket/prefix"), "Access Key"]
            },
            BackupProtocol::Sftp => &[
                language.pick("SSH 目的地 (user@host)", "SSH destination (user@host)"),
                language.pick("远端目录", "Remote directory"),
            ],
        };
        let slot_rows = slot_labels.iter().enumerate().map(|(ix, label)| {
            let input = self.backup_remote_inputs.get(ix).cloned();
            self.row(
                label,
                "",
                div().w(px(300.0)).children(input.map(|input| Input::new(&input))),
                cx,
            )
        });

        let secret_ready = crate::backup_remote::protocol_secret_set(protocol);
        let secret_label = match protocol {
            BackupProtocol::WebDav => Some(language.pick("WebDAV 密码", "WebDAV password")),
            BackupProtocol::S3 => Some("S3 Secret Key"),
            _ => None,
        };

        let mut remote_group = self
            .group(language.pick("远端同步", "Remote sync"), cx)
            .child(div().text_xs().text_color(muted).child(
                language.pick(
                    "推送 = 当前勾选类别加密打包后上传；恢复最新 = 拉取远端最新包解密落盘。SFTP 复用上方 SSH 主机的认证。",
                    "Push encrypts and uploads the selected categories. Restore latest downloads, decrypts, and applies the newest remote archive. SFTP reuses SSH host authentication configured above.",
                ),
            ))
            .child(h_flex().gap_2().children(protocol_buttons))
            .children(slot_rows);
        if let Some(label) = secret_label {
            remote_group = remote_group.child(
                self.row(
                    label,
                    "",
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(muted).child(if secret_ready {
                            language.pick("已设置", "Set")
                        } else {
                            language.pick("未设置", "Not set")
                        }))
                        .child(div().w(px(220.0)).child(Input::new(&self.backup_secret_input)))
                        .child(
                            NebulaButton::new("bk-store-secret")
                                .label(language.pick("保存凭据", "Save credential"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.store_remote_secret(window, cx);
                                })),
                        ),
                    cx,
                ),
            );
        }
        if protocol != BackupProtocol::Off {
            remote_group = remote_group.child(
                h_flex()
                    .gap_2()
                    .child(
                        NebulaButton::new("bk-save-remote")
                            .label(language.pick("保存配置", "Save configuration"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_remote_config(cx);
                            })),
                    )
                    .child(
                        NebulaButton::new("bk-push")
                            .label(if busy {
                                language.pick("处理中…", "Processing...")
                            } else {
                                language.pick("立即推送", "Push now")
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.push_remote(cx);
                            })),
                    )
                    .child(
                        NebulaButton::new("bk-pull")
                            .label(language.pick("恢复最新备份", "Restore latest backup"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pull_remote(cx);
                            })),
                    ),
            );
        }

        let local_group = self
            .group(language.pick("加密备份", "Encrypted backup"), cx)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(language.pick(
                        "端到端加密（密码不落盘）；SSH 私钥永不进包，主机列表脱敏导出。",
                        "End-to-end encrypted; the password is never persisted. SSH private keys are never included and host lists are exported with sensitive data removed.",
                    )),
            )
            .children(category_rows)
            .child(self.row(
                language.pick("备份密码", "Backup password"),
                language.pick("导出时用它加密整个包，恢复时要一模一样的一串。密码不落盘、也无从找回——忘了这份备份就打不开了。", "This password encrypts the entire export and the exact same value is required to restore it. It is neither persisted nor recoverable; losing it makes the backup unreadable."),
                div().w(px(300.0)).child(Input::new(&self.backup_pass_input)),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        NebulaButton::new("bk-export")
                            .label(if busy {
                                language.pick("处理中…", "Processing...")
                            } else {
                                language.pick("导出到文件…", "Export to file...")
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_backup(cx);
                            })),
                    )
                    .child(
                        NebulaButton::new("bk-restore")
                            .label(language.pick("从文件恢复…", "Restore from file..."))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.restore_backup(cx);
                            })),
                    ),
            );

        v_flex().w_full().gap(px(GROUP_GAP)).child(local_group).child(remote_group).when_some(
            self.backup_status.clone(),
            |page, status| {
                let error = status.is_error();
                let message = status.text(language);
                page.child(
                    div()
                        .pt_4()
                        .text_color(if error { theme.danger } else { theme.success })
                        .child(message),
                )
            },
        )
    }
}
