use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

pub mod document;

pub const MAX_ANSWER_BYTES: usize = 128 * 1024;

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum AssistantAnswer {
    Complete(Arc<str>),
    Missing,
    TooLarge { bytes: usize },
}

impl AssistantAnswer {
    pub fn from_hook(source: &str, payload: &Value) -> Option<Self> {
        let field = match source {
            "claude" if payload.get("hook_event_name")?.as_str()? == "Stop" => {
                "last_assistant_message"
            },
            "codex" if payload.get("type")?.as_str()? == "agent-turn-complete" => {
                "last-assistant-message"
            },
            _ => return None,
        };
        Some(match payload.get(field).and_then(Value::as_str) {
            Some(text) if text.len() > MAX_ANSWER_BYTES => Self::TooLarge { bytes: text.len() },
            Some(text) if !text.trim().is_empty() => Self::Complete(Arc::from(text)),
            _ => Self::Missing,
        })
    }

    pub fn source(&self) -> Option<&Arc<str>> {
        match self {
            Self::Complete(source) => Some(source),
            _ => None,
        }
    }

    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Complete(_) => None,
            Self::Missing => Some("未收到完整回答原文；保留终端内容，不从屏幕猜测。".into()),
            Self::TooLarge { bytes } => Some(format!(
                "回答原文共 {bytes} 字节，超过 {} KiB 阅读上限；未截断渲染，请在终端查看。",
                MAX_ANSWER_BYTES / 1024
            )),
        }
    }
}

impl fmt::Debug for AssistantAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete(source) => formatter
                .debug_struct("Complete")
                .field("bytes", &source.len())
                .finish_non_exhaustive(),
            Self::Missing => formatter.write_str("Missing"),
            Self::TooLarge { bytes } => {
                formatter.debug_struct("TooLarge").field("bytes", bytes).finish()
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerSnapshot {
    pub provider: String,
    pub session_id: String,
    pub received_sequence: u64,
    pub content: AssistantAnswer,
    pub cwd: Option<PathBuf>,
}

#[derive(Default)]
pub struct AnswerInbox {
    owner: Option<(String, String)>,
    closed: bool,
    last_sequence: u64,
    pub latest: Option<AnswerSnapshot>,
}

impl AnswerInbox {
    pub fn observe(&mut self, event: &crate::ai_hook::AiHookEvent, pane_id: u64) -> bool {
        use crate::ai_hook::AiHookKind;

        if event.pane != Some(pane_id)
            || !matches!(event.source.as_str(), "claude" | "codex")
            || event.received_sequence <= self.last_sequence
        {
            return false;
        }
        let Some(session_id) = event.session_id.as_deref().filter(|id| !id.is_empty()) else {
            return false;
        };
        let identity = (event.source.clone(), session_id.to_owned());
        let starts_session =
            matches!(event.kind, AiHookKind::SessionStart | AiHookKind::PromptSubmit);
        if self.owner.as_ref().is_some_and(|owner| owner != &identity) && !starts_session {
            return false;
        }
        if self.closed && !starts_session {
            return false;
        }
        if self.owner.as_ref() != Some(&identity) || starts_session && self.closed {
            self.latest = None;
        }
        self.owner = Some(identity);
        self.last_sequence = event.received_sequence;
        if starts_session {
            self.closed = false;
        }
        if event.kind == AiHookKind::SessionEnd {
            self.closed = true;
        }
        if let Some(content) = event.answer.clone() {
            self.latest = Some(AnswerSnapshot {
                provider: event.source.clone(),
                session_id: session_id.to_owned(),
                received_sequence: event.received_sequence,
                content,
                cwd: event.answer_cwd.clone(),
            });
            return true;
        }
        false
    }

    pub fn close(&mut self) {
        self.closed = self.owner.is_some();
    }

    pub fn begin_command(&mut self) {
        self.owner = None;
        self.closed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(provider: &str, text: &str) -> Value {
        match provider {
            "claude" => json!({"hook_event_name": "Stop", "last_assistant_message": text}),
            "codex" => json!({"type": "agent-turn-complete", "last-assistant-message": text}),
            _ => unreachable!(),
        }
    }

    #[test]
    fn both_providers_preserve_complete_utf8_and_math_verbatim() {
        let source = format!(
            "{}\n$$\n\\widehat{{f}}(\\xi)\n=\n+ \\int_0^1 x\\,dx\n\\begin{{cases}}a&b\\\\\nc&d\\end{{cases}}\n$$\n",
            "中文与 $HOME 和代码 `$$`。".repeat(500)
        );
        for provider in ["claude", "codex"] {
            let answer = AssistantAnswer::from_hook(provider, &payload(provider, &source)).unwrap();
            assert_eq!(answer.source().unwrap().as_ref(), source);
        }
    }

    #[test]
    fn oversized_answers_are_reported_not_cut_into_partial_formulas() {
        for provider in ["claude", "codex"] {
            let source = "a".repeat(MAX_ANSWER_BYTES + 1);
            let answer = AssistantAnswer::from_hook(provider, &payload(provider, &source)).unwrap();
            assert_eq!(answer, AssistantAnswer::TooLarge { bytes: source.len() });
            assert!(answer.source().is_none());
            assert!(answer.notice().unwrap().contains("未截断"));
        }
    }

    #[test]
    fn absent_wrongly_named_or_empty_fields_never_become_screen_guesses() {
        for provider in ["claude", "codex"] {
            assert_eq!(
                AssistantAnswer::from_hook(provider, &payload(provider, " \n")),
                Some(AssistantAnswer::Missing)
            );
        }
        assert_eq!(
            AssistantAnswer::from_hook(
                "claude",
                &json!({"hook_event_name": "Stop", "last-assistant-message": "wrong"})
            ),
            Some(AssistantAnswer::Missing)
        );
        assert_eq!(
            AssistantAnswer::from_hook(
                "codex",
                &json!({"type": "agent-turn-complete", "last_assistant_message": "wrong"})
            ),
            Some(AssistantAnswer::Missing)
        );
        assert!(
            AssistantAnswer::from_hook(
                "claude",
                &json!({"hook_event_name": "Notification", "last_assistant_message": "wrong"})
            )
            .is_none()
        );
    }

    #[test]
    fn debug_output_never_contains_answer_text() {
        let answer = AssistantAnswer::Complete(Arc::from("private-source-secret"));
        assert!(!format!("{answer:?}").contains("private-source-secret"));
    }

    fn event(
        provider: &str,
        session: &str,
        pane: u64,
        sequence: u64,
        kind: crate::ai_hook::AiHookKind,
    ) -> crate::ai_hook::AiHookEvent {
        let mut body = payload(provider, "answer");
        body[if provider == "claude" { "session_id" } else { "thread-id" }] = json!(session);
        let wire = format!("nebula-hook/1 source={provider} pane={pane}\n{body}");
        let mut event = crate::ai_hook::parse_remote_envelope(wire.as_bytes(), Some(pane)).unwrap();
        event.received_sequence = sequence;
        event.kind = kind;
        if kind != crate::ai_hook::AiHookKind::TurnDone {
            event.answer = None;
        }
        event
    }

    #[test]
    fn panes_sessions_and_closed_sessions_cannot_cross_bind_answers() {
        use crate::ai_hook::AiHookKind::*;
        for provider in ["claude", "codex"] {
            let mut inbox = AnswerInbox::default();
            assert!(!inbox.observe(&event(provider, "old", 2, 1, TurnDone), 1));
            assert!(inbox.observe(&event(provider, "old", 1, 2, TurnDone), 1));
            inbox.observe(&event(provider, "new", 1, 3, SessionStart), 1);
            assert!(inbox.latest.is_none());
            assert!(!inbox.observe(&event(provider, "old", 1, 4, TurnDone), 1));
            assert!(inbox.observe(&event(provider, "new", 1, 5, TurnDone), 1));
            inbox.observe(&event(provider, "new", 1, 6, SessionEnd), 1);
            assert!(!inbox.observe(&event(provider, "new", 1, 7, TurnDone), 1));
            assert_eq!(inbox.latest.as_ref().unwrap().received_sequence, 5);
        }
    }

    #[test]
    fn fallback_focus_and_out_of_order_events_do_not_supply_reader_content() {
        use crate::ai_hook::AiHookKind::TurnDone;
        let mut inbox = AnswerInbox::default();
        let mut missing_pane = event("codex", "thread", 1, 1, TurnDone);
        missing_pane.pane = None;
        assert!(!inbox.observe(&missing_pane, 1));
        assert!(inbox.observe(&event("codex", "thread", 1, 3, TurnDone), 1));
        assert!(!inbox.observe(&event("codex", "thread", 1, 2, TurnDone), 1));
        inbox.close();
        assert!(!inbox.observe(&event("codex", "thread", 1, 4, TurnDone), 1));
    }

    #[test]
    fn new_cli_command_accepts_another_codex_session_without_erasing_last_answer_early() {
        use crate::ai_hook::AiHookKind::TurnDone;
        let mut inbox = AnswerInbox::default();
        assert!(inbox.observe(&event("codex", "old", 1, 1, TurnDone), 1));
        inbox.close();
        inbox.begin_command();
        assert_eq!(inbox.latest.as_ref().unwrap().session_id, "old");
        assert!(inbox.observe(&event("codex", "new", 1, 2, TurnDone), 1));
        assert_eq!(inbox.latest.as_ref().unwrap().session_id, "new");
        assert!(!inbox.observe(&event("codex", "old", 1, 3, TurnDone), 1));
    }

    #[test]
    fn image_directory_is_captured_with_the_completed_answer() {
        use crate::ai_hook::AiHookKind::TurnDone;
        let directory = tempfile::tempdir().unwrap();
        for provider in ["claude", "codex"] {
            let mut completion = event(provider, "session", 1, 1, TurnDone);
            completion.answer_cwd = Some(directory.path().to_path_buf());
            let mut inbox = AnswerInbox::default();
            assert!(inbox.observe(&completion, 1));
            completion.answer_cwd = None;
            assert_eq!(inbox.latest.as_ref().unwrap().cwd.as_deref(), Some(directory.path()));
        }
    }
}
