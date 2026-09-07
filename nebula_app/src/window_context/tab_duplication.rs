//! Fresh-session duplication preserves the selected tab's identity and location.

use super::*;

impl WindowContext {
    /// Duplicate the selected tab next to itself. We copy the launch identity,
    /// current cwd, user name and color, but intentionally not the live grid or
    /// split tree: a duplicate is a fresh process/session, matching Windows
    /// Terminal and avoiding shared PTY ownership.
    pub(super) fn duplicate_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else { return };
        let launch = tab.launch.clone();
        let custom_name = tab.custom_name.clone();
        let custom_color = tab.custom_color;
        self.select_tab(index);
        let cwd = self
            .pane(self.focused_pane_id())
            .map(|pane| pane.nebula_state.cwd.clone())
            .unwrap_or_default();
        let before = self.tabs.len();

        match launch {
            TabLaunch::Default => self.spawn_tab_at(self.focused_cwd(), TabPlacement::Created),
            TabLaunch::Profile(mut profile) => {
                if let Some(args) =
                    crate::shell_detect::wsl_args_at(&profile.command, &profile.args, &cwd)
                {
                    profile.args = args;
                }
                self.spawn_tab_profile_value(profile, TabPlacement::Created);
            },
            TabLaunch::Shell { name, shell } => {
                let updated = crate::shell_detect::wsl_args_at(shell.program(), shell.args(), &cwd)
                    .map(|args| tty::Shell::new(shell.program().to_owned(), args));
                self.spawn_tab_shell(name, updated.unwrap_or(shell), TabPlacement::Created);
            },
            TabLaunch::Ssh(host) => {
                self.spawn_tab_ssh_at(host, Some(cwd), TabPlacement::Created);
            },
            TabLaunch::Document(path) => {
                let doc = crate::display::markdown_view::DocView::open(path.clone());
                let label = format!("\u{eb1d} {}", doc.title);
                self.insert_tab(
                    TabEntry {
                        layout: Layout::Leaf(DOC_PANE_ID),
                        active_pane: DOC_PANE_ID,
                        has_bell: false,
                        custom_name: Some(label),
                        custom_color: None,
                        launch: TabLaunch::Document(path),
                        doc: Some(doc),
                        image: None,
                        settings: false,
                    },
                    TabPlacement::Created,
                );
                self.dirty = true;
            },
            TabLaunch::Image(path) => self.open_image_tab(path),
            TabLaunch::Settings => self.open_settings_tab(),
        }

        if self.tabs.len() > before {
            if let Some(duplicate) = self.tabs.get_mut(self.active_tab) {
                duplicate.custom_name = custom_name;
                duplicate.custom_color = custom_color;
            }
            self.sync_chrome_tabs();
            self.mark_session_dirty();
        }
    }
}
