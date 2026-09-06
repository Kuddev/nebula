use gpui::{App, Context, Window};
use nebula_settings::TabsPositionName;

use super::{NebulaWorkspace, SidebarActivity, TabPresentation};

fn sidebar_state(
    settings_open: bool,
    position: TabsPositionName,
    collapsed: bool,
    restore: Option<bool>,
) -> (bool, Option<bool>) {
    if settings_open && super::settings_should_fold_sidebar(position) {
        (true, Some(restore.unwrap_or(collapsed)))
    } else {
        (restore.unwrap_or(collapsed), None)
    }
}

impl NebulaWorkspace {
    pub(super) fn sync_settings_layout(&mut self) {
        let (collapsed, restore) = sidebar_state(
            self.settings_open,
            self.tabs_position,
            self.sidebar_collapsed,
            self.settings_restore_sidebar_collapsed,
        );
        self.settings_restore_sidebar_collapsed = restore;
        if self.sidebar_collapsed != collapsed {
            self.sidebar_collapsed = collapsed;
            self.sidebar_fold_armed = true;
        }
        if self.settings_open && self.tabs_position == TabsPositionName::Top {
            self.top_tabs_scroll.scroll_to_item(self.tabs.len());
        }
    }

    pub(super) fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open {
            return;
        }
        self.settings_open = false;
        self.sync_settings_layout();
        let restore_panel = std::mem::take(&mut self.settings_restore_side_panel_open);
        if restore_panel && !self.side_panel.open {
            self.toggle_side_panel(self.side_panel.view, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn top_tab_count(&self) -> usize {
        self.tabs.len() + usize::from(self.settings_open)
    }

    pub(super) fn top_tab_presentation(
        &self,
        index: usize,
        cx: &App,
        dark: bool,
    ) -> TabPresentation {
        if index < self.tabs.len() {
            return self.tab_presentation(index, cx, dark);
        }
        TabPresentation {
            title: super::workspace_ui_language().text(crate::i18n::Message::CommonSettings).into(),
            is_settings: true,
            activity: SidebarActivity::Idle,
            logo_image: None,
            program_glyph: None,
            shell_tag: None,
            color: None,
            renaming: None,
            pane_count: 0,
        }
    }

    pub(super) fn activate_top_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open && index == self.tabs.len() {
            self.open_settings(window, cx);
        } else {
            self.activate_tab(index, window, cx);
        }
    }

    pub(super) fn close_top_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_open && index == self.tabs.len() {
            self.close_settings(window, cx);
        } else {
            self.request_close_tab(index, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_top_sidebar_round_trip_preserves_exclusive_settings() {
        let mut state = sidebar_state(true, TabsPositionName::Sidebar, false, None);
        assert_eq!(state, (true, Some(false)));
        state = sidebar_state(true, TabsPositionName::Top, state.0, state.1);
        assert_eq!(state, (false, None));
        state = sidebar_state(true, TabsPositionName::Sidebar, state.0, state.1);
        assert_eq!(state, (true, Some(false)));
        assert_eq!(
            sidebar_state(false, TabsPositionName::Sidebar, state.0, state.1),
            (false, None)
        );
    }

    #[test]
    fn settings_opened_in_top_mode_fold_the_sidebar_after_switching() {
        let state = sidebar_state(true, TabsPositionName::Top, false, None);
        assert_eq!(state, (false, None));
        assert_eq!(
            sidebar_state(true, TabsPositionName::Sidebar, state.0, state.1),
            (true, Some(false))
        );
    }

    #[test]
    fn initially_collapsed_sidebar_stays_collapsed_after_repeated_switches() {
        let mut state = (true, None);
        for _ in 0..3 {
            state = sidebar_state(true, TabsPositionName::Sidebar, state.0, state.1);
            state = sidebar_state(true, TabsPositionName::Top, state.0, state.1);
        }
        assert_eq!(sidebar_state(false, TabsPositionName::Sidebar, state.0, state.1), (true, None));
    }
}
