//! Duplicate launch identity and location without sharing a live session.

use gpui::{Context, Window};

use super::NebulaWorkspace;
use crate::session::LaunchSession;

fn inherit_guest_directory(launch: &mut LaunchSession, cwd: &str) -> bool {
    let (program, args) = match launch {
        LaunchSession::Shell { program, args, .. } => (program, args),
        LaunchSession::Profile { command, args, .. } => (command, args),
        _ => return false,
    };
    let Some(updated) = crate::shell_detect::wsl_args_at(program, args, cwd) else {
        return false;
    };
    *args = updated;
    true
}

impl NebulaWorkspace {
    pub(super) fn duplicate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        let Some(view) = tab.focused_view() else { return };
        let (ssh, raw_cwd, remote_cwd, pane_id) = {
            let view = view.read(cx);
            (view.ssh_destination.clone(), view.cwd.clone(), view.remote_cwd(), view.pane_id)
        };
        let meta = self.meta(ix);
        if let Some(destination) = ssh {
            let remote_cwd = remote_cwd.or_else(|| {
                self.remote_browser.path_for(pane_id, &destination).map(ToOwned::to_owned)
            });
            self.add_ssh_terminal_at(destination, remote_cwd, window, cx);
        } else {
            // Old snapshots without an identity resolve the current default shell.
            let mut launch = match meta.launch {
                None | Some(LaunchSession::Default) => Self::configured_local_launch(cx),
                Some(launch) => launch,
            };
            // A guest path must never become CreateProcess's host working directory.
            let cwd = if inherit_guest_directory(&mut launch, &raw_cwd) {
                None
            } else {
                crate::session::valid_dir(&raw_cwd)
            };
            self.add_terminal_with(launch, cwd, None, window, cx);
        }
        if let Some(target) = self.tab_meta.get_mut(self.active) {
            target.custom_name = meta.custom_name;
            target.color = meta.color;
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_wsl_shell_uses_guest_directory_even_when_it_is_not_on_the_host() {
        use crate::gpui_shell::terminal::view::TerminalLaunch;

        let mut launch = LaunchSession::Shell {
            name: "Debian".into(),
            program: "wsl.exe".into(),
            args: vec!["-d".into(), "Debian".into(), "--cd".into(), "/old".into()],
        };
        assert!(inherit_guest_directory(&mut launch, "/home/guest/new project"));
        let TerminalLaunch::Local { cwd, shell: Some(shell), .. } =
            NebulaWorkspace::terminal_launch_from_session(&launch, None)
        else {
            panic!("duplicate must use a new local WSL process");
        };
        assert!(cwd.is_none(), "the guest cwd must not be passed as a host directory");
        assert_eq!(shell.program(), "wsl.exe");
        assert_eq!(shell.args(), ["--cd", "/home/guest/new project", "-d", "Debian"]);
        let LaunchSession::Shell { args, .. } = launch else { panic!("shell identity lost") };
        assert_eq!(args, ["--cd", "/home/guest/new project", "-d", "Debian"]);
    }

    #[test]
    fn imported_wsl_profile_keeps_its_user_and_shell() {
        let mut launch = LaunchSession::Profile {
            name: "Debian Zsh".into(),
            command: "wsl.exe".into(),
            args: ["-d", "Debian", "-u", "guest", "--exec", "zsh", "-l"].map(String::from).to_vec(),
            cwd: None,
            shell_id: None,
        };
        assert!(inherit_guest_directory(&mut launch, "/home/guest"));
        let LaunchSession::Profile { args, .. } = launch else { panic!("profile identity lost") };
        assert_eq!(
            args,
            ["--cd", "/home/guest", "-d", "Debian", "-u", "guest", "--exec", "zsh", "-l"]
        );
    }

    #[test]
    fn invalid_guest_directory_leaves_the_original_profile_untouched() {
        let original = LaunchSession::Shell {
            name: "Debian".into(),
            program: "wsl.exe".into(),
            args: ["-d", "Debian", "--cd", "/original"].map(String::from).to_vec(),
        };
        let mut launch = original.clone();
        assert!(!inherit_guest_directory(&mut launch, "/home/guest\r"));
        assert_eq!(launch, original);
    }
}
