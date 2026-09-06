use crate::ai_agents::{AgentKind, AgentStatus};
use gpui::{Context, EventEmitter as _};

impl super::TerminalView {
    pub(crate) fn is_reading_answer(&self) -> bool {
        self.answer_reader.is_some()
    }

    pub(super) fn notify_command_done(&self, cx: &mut Context<Self>) {
        if let Some(started) = self.command_started
            && started.elapsed() >= crate::notify::COMMAND_NOTIFY_MIN
        {
            cx.emit(super::TerminalViewEvent::Notification(
                crate::notify::Notification::CommandDone {
                    duration: started.elapsed(),
                    program: self.running_program.clone(),
                },
            ));
        }
    }
}

pub(super) fn screen_notification(previous: AgentStatus, next: AgentStatus) -> Option<bool> {
    match next {
        AgentStatus::Blocked if previous != AgentStatus::Blocked => Some(true),
        AgentStatus::Done if matches!(previous, AgentStatus::Working | AgentStatus::Blocked) => {
            Some(false)
        },
        _ => None,
    }
}

pub(super) fn screen_program(
    current: Option<&str>,
    identified: Option<AgentKind>,
) -> Option<String> {
    match current {
        Some(program) if AgentKind::parse(program).is_some() => Some(program.to_owned()),
        Some(program) if !screen_identity_allowed(Some(program)) => None,
        _ => identified.map(|agent| agent.slug().to_owned()),
    }
}

pub(super) fn screen_identity_allowed(current: Option<&str>) -> bool {
    current.is_none_or(|program| {
        crate::process_tree::is_interactive_shell_command(program)
            || crate::process_tree::display_name(program).eq_ignore_ascii_case("ssh")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_completion_and_attention_are_edges_not_idle_polling() {
        assert_eq!(screen_notification(AgentStatus::Working, AgentStatus::Done), Some(false));
        assert_eq!(screen_notification(AgentStatus::Blocked, AgentStatus::Done), Some(false));
        assert_eq!(screen_notification(AgentStatus::Working, AgentStatus::Blocked), Some(true));
        assert_eq!(screen_notification(AgentStatus::Unknown, AgentStatus::Blocked), Some(true));
        assert_eq!(screen_notification(AgentStatus::Done, AgentStatus::Done), None);
        assert_eq!(screen_notification(AgentStatus::Blocked, AgentStatus::Blocked), None);
        assert_eq!(screen_notification(AgentStatus::Unknown, AgentStatus::Idle), None);
        assert_eq!(screen_notification(AgentStatus::Idle, AgentStatus::Idle), None);
    }

    #[test]
    fn wsl_and_ssh_wrappers_allow_identifying_the_guest_agent() {
        for wrapper in [None, Some("wsl"), Some("ssh"), Some("bash")] {
            assert_eq!(screen_program(wrapper, Some(AgentKind::Claude)), Some("claude".into()));
        }
        assert_eq!(screen_program(Some("codex"), Some(AgentKind::Claude)), Some("codex".into()));
        assert_eq!(screen_program(Some("vim"), Some(AgentKind::Claude)), None);
        assert_eq!(screen_program(Some("wsl"), None), None);
    }
}
