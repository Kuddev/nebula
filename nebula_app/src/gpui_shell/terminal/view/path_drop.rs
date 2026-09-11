//! External file paths enter the same bounded, paste-only path as clipboard text.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Window};

use super::{TerminalView, ui_language};
use crate::display::side_panel::{PathQuote, drop_text_for_paths};
use crate::gpui_shell::toast::{self, ToastKind};
use crate::i18n::Message;

#[derive(Default)]
pub(super) struct PathDropState {
    generation: u64,
    pending: bool,
}

impl PathDropState {
    pub(super) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

impl TerminalView {
    fn path_quote(&self) -> PathQuote {
        if self.ssh_destination.is_some()
            || self.exec_context.as_ref().and_then(|context| context.wsl_distribution()).is_some()
        {
            return PathQuote::Posix;
        }
        let program = self
            .exec_context
            .as_ref()
            .and_then(|context| context.shell_program())
            .unwrap_or("")
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(program.as_str(), "pwsh" | "pwsh.exe" | "powershell" | "powershell.exe") {
            PathQuote::PowerShell
        } else if cfg!(windows) && matches!(program.as_str(), "" | "cmd" | "cmd.exe") {
            PathQuote::CommandPrompt
        } else {
            PathQuote::Posix
        }
    }

    pub(super) fn paste_dropped_paths(
        &mut self,
        paths: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() || self.exited.is_some() {
            return;
        }
        if let Some(text) = drop_text_for_paths(paths, self.path_quote()) {
            // A file path belongs to this pane, including bracketed-paste framing.
            self.paste_now_impl(&text, false, cx);
            window.focus(&self.focus_handle, cx);
        } else {
            toast::toast(
                window,
                cx,
                ToastKind::Warning,
                ui_language().text(Message::CommonDropPathsInvalid),
            );
        }
    }

    pub(super) fn drop_external_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.is_none() || self.exited.is_some() {
            return;
        }
        if self.ssh_destination.is_some() {
            toast::toast(
                window,
                cx,
                ToastKind::Warning,
                ui_language().text(Message::CommonDropPathsRemote),
            );
            return;
        }
        let paths: Option<Vec<String>> =
            paths.iter().map(|path| path.to_str().map(str::to_owned)).collect();
        let Some(paths) =
            paths.filter(|paths| drop_text_for_paths(paths, PathQuote::Posix).is_some())
        else {
            toast::toast(
                window,
                cx,
                ToastKind::Warning,
                ui_language().text(Message::CommonDropPathsInvalid),
            );
            return;
        };
        let distro = self
            .exec_context
            .as_ref()
            .and_then(|context| context.wsl_distribution())
            .map(|distro| distro.map(str::to_owned));
        let Some(distro) = distro else {
            self.paste_dropped_paths(&paths, window, cx);
            return;
        };
        if self.path_drop.pending {
            toast::toast(
                window,
                cx,
                ToastKind::Warning,
                ui_language().text(Message::CommonDropPathsBusy),
            );
            return;
        }
        let user =
            self.exec_context.as_ref().and_then(|context| context.wsl_user()).map(str::to_owned);
        self.path_drop.pending = true;
        let generation = self.path_drop.generation;
        let term = Arc::downgrade(&self.session.as_ref().unwrap().term);
        window.focus(&self.focus_handle, cx);
        let job = cx.background_executor().spawn(async move { wsl_paths(paths, distro, user) });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.path_drop.pending = false;
                let same_session = this.session.as_ref().is_some_and(|session| {
                    term.upgrade().is_some_and(|term| Arc::ptr_eq(&term, &session.term))
                });
                if !same_session
                    || this.exited.is_some()
                    || generation != this.path_drop.generation
                    || !this.focus_handle.is_focused(window)
                {
                    toast::toast(
                        window,
                        cx,
                        ToastKind::Warning,
                        ui_language().text(Message::CommonDropPathsExpired),
                    );
                    return;
                }
                match result {
                    Ok(paths) => this.paste_dropped_paths(&paths, window, cx),
                    Err(error) => {
                        log::warn!("external path translation failed: {error}");
                        toast::toast(
                            window,
                            cx,
                            ToastKind::Warning,
                            ui_language().text(Message::CommonDropPathsFailed),
                        );
                    },
                }
            });
        })
        .detach();
    }
}

fn wsl_paths(
    paths: Vec<String>,
    distro: Option<String>,
    user: Option<String>,
) -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new("wsl.exe");
    if let Some(distro) = distro {
        command.args(["-d", &distro]);
    }
    if let Some(user) = user {
        command.args(["-u", &user]);
    }
    command.args([
        "--exec",
        "sh",
        "-c",
        "for p; do wslpath -a -u \"$p\" || exit; done",
        "pebrel-paths",
    ]);
    command.args(&paths);
    let output = crate::display::side_panel::command_output_with_timeout(
        command,
        Some(Duration::from_secs(10)),
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("wslpath failed".to_owned());
    }
    let output = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let translated: Vec<String> = output.lines().map(str::to_owned).collect();
    if translated.len() != paths.len()
        || translated.iter().any(|path| !path.starts_with('/'))
        || drop_text_for_paths(&translated, PathQuote::Posix).is_none()
    {
        return Err("invalid translated paths".to_owned());
    }
    Ok(translated)
}
