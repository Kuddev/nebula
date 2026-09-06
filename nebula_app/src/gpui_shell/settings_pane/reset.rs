use super::*;

impl SettingsPane {
    pub(super) fn reset_all_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let language = crate::gpui_shell::config::ui_language(cx);
        self.slider_persist = None;
        let backup = match nebula_settings::restore_default_settings() {
            Ok(backup) => backup,
            Err(error) => {
                crate::gpui_shell::toast::toast(
                    window,
                    cx,
                    crate::gpui_shell::toast::ToastKind::Warning,
                    format!(
                        "{}: {error}",
                        language.pick("恢复默认设置失败", "Failed to restore defaults")
                    ),
                );
                return;
            },
        };
        let active_section = self.active_section;
        let about_update = std::mem::replace(&mut self.about_update, AboutUpdateState::Idle);
        let about_last_checked = self.about_last_checked.take();
        let about_update_seq = self.about_update_seq;
        let proxy_test_seq = self.proxy_test_seq.wrapping_add(1);
        let provider_test_seq = self.provider_test_seq.wrapping_add(1);
        let ssh_editor_seq = self.ssh_editor_seq.wrapping_add(1);
        let ssh_test_seq = self.ssh_test_seq.wrapping_add(1);
        let ssh_undo_seq = self.ssh_undo_seq.wrapping_add(1);
        let backup_seq = self.backup_seq.wrapping_add(1);
        let settings = crate::gpui_shell::config::Settings::load(
            crate::gpui_shell::theme::effective_theme_name(cx),
        );
        gpui_component::set_locale(settings.ui_language.gpui_component_locale());
        cx.set_global(settings);
        *self = Self::new(window, cx);
        self.active_section = active_section;
        self.about_update = about_update;
        self.about_last_checked = about_last_checked;
        self.about_update_seq = about_update_seq;
        self.proxy_test_seq = proxy_test_seq;
        self.provider_test_seq = provider_test_seq;
        self.ssh_editor_seq = ssh_editor_seq;
        self.ssh_test_seq = ssh_test_seq;
        self.ssh_undo_seq = ssh_undo_seq;
        self.backup_seq = backup_seq;
        window.focus(&self.focus_handle, cx);
        cx.emit(SettingsPaneEvent::Changed);
        cx.refresh_windows();
        cx.notify();
        crate::gpui_shell::toast::toast(
            window,
            cx,
            crate::gpui_shell::toast::ToastKind::Success,
            if backup.is_some() {
                language.pick("已恢复默认设置；主机与凭据保持不变，原设置已备份。", "Defaults restored. Hosts and credentials are unchanged; previous settings were backed up.")
            } else {
                language.pick(
                    "已恢复默认设置；主机与凭据保持不变。",
                    "Defaults restored. Hosts and credentials are unchanged.",
                )
            },
        );
    }
}
