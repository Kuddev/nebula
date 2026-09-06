use gpui::{Context, EventEmitter as _};

use super::{TerminalView, TerminalViewEvent};

impl TerminalView {
    pub(crate) fn capture_confirmation(
        &mut self,
    ) -> Option<super::super::confirmation::BinaryConfirmation> {
        let (screen, mode) = self.runtime_screen_state()?;
        let program = self.running_program.as_deref().unwrap_or_default();
        let waiting = self.confirmation_waiting();
        self.confirmation.capture(
            &screen,
            program,
            self.ai_session.as_ref().map(|identity| identity.session_id.as_str()),
            mode,
            waiting,
        )
    }

    fn confirmation_waiting(&self) -> bool {
        self.marked_text.is_none()
            && self
                .running_program
                .as_deref()
                .and_then(crate::ai_agents::AgentKind::parse)
                .is_some()
            && matches!(
                self.runtime_task_state(),
                crate::runtime_api::RuntimeTaskState::Attention
                    | crate::runtime_api::RuntimeTaskState::WaitingInput
            )
    }

    pub(crate) fn answer_confirmation(
        &mut self,
        request_id: u64,
        allow: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((screen, mode)) = self.runtime_screen_state() else { return false };
        let waiting = self.confirmation_waiting();
        let reply = self.confirmation.answer(
            request_id,
            allow,
            &screen,
            self.running_program.as_deref().unwrap_or_default(),
            self.ai_session.as_ref().map(|identity| identity.session_id.as_str()),
            mode,
            waiting,
        );
        let Some(reply) = reply else { return false };
        self.write_input(reply, cx);
        self.agent_status = crate::ai_agents::AgentStatus::Working;
        self.agent_turn_active = true;
        self.agent_runtime_submit_pending = true;
        self.idle_screen_streak = 0;
        cx.emit(TerminalViewEvent::TitleChanged);
        true
    }
}
