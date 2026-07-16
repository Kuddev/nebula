//! Shared interaction-state contracts for Nebula's self-drawn UI.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAction {
    None,
    Retry,
    OpenSettings,
    OpenLogs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacingError {
    pub title: String,
    pub cause: String,
    pub suggestion: String,
    pub retry: RetryAction,
    pub details: Option<String>,
}

impl UserFacingError {
    pub fn new(
        title: impl Into<String>,
        cause: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            cause: cause.into(),
            suggestion: suggestion.into(),
            retry: RetryAction::None,
            details: None,
        }
    }

    pub fn retry(mut self, retry: RetryAction) -> Self {
        self.retry = retry;
        self
    }

    pub fn details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    pub fn message(&self) -> String {
        let mut message =
            format!("{}\n原因：{}\n建议：{}", self.title, self.cause, self.suggestion);
        let action = match self.retry {
            RetryAction::None => None,
            RetryAction::Retry => Some("操作：请重试"),
            RetryAction::OpenSettings => Some("操作：打开设置检查配置"),
            RetryAction::OpenLogs => Some("操作：打开日志查看诊断信息"),
        };
        if let Some(action) = action {
            message.push('\n');
            message.push_str(action);
        }
        if let Some(details) = &self.details {
            message.push_str("\n详情：");
            message.push_str(details);
        }
        message
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Determinate,
    Indeterminate,
    Short,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskState {
    Idle,
    Running {
        kind: TaskKind,
        label: String,
        started_at: Instant,
        progress: Option<f32>,
        cancellable: bool,
    },
    Failed(UserFacingError),
}

impl Default for TaskState {
    fn default() -> Self {
        Self::Idle
    }
}

impl TaskState {
    pub fn running(kind: TaskKind, label: impl Into<String>, cancellable: bool) -> Self {
        Self::Running {
            kind,
            label: label.into(),
            started_at: Instant::now(),
            progress: None,
            cancellable,
        }
    }

    pub fn show_busy(&self, now: Instant) -> bool {
        matches!(self, Self::Running { started_at, .. } if now.saturating_duration_since(*started_at) >= Duration::from_millis(150))
    }

    pub fn show_stage(&self, now: Instant) -> bool {
        matches!(self, Self::Running { started_at, .. } if now.saturating_duration_since(*started_at) >= Duration::from_secs(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyState {
    pub title: String,
    pub reason: String,
    pub action: String,
    pub shortcut: Option<String>,
}

impl EmptyState {
    pub fn new(
        title: impl Into<String>,
        reason: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self { title: title.into(), reason: reason.into(), action: action.into(), shortcut: None }
    }
}

/// Retains the last successful value while a refresh is in flight.
#[derive(Debug, Clone, PartialEq)]
pub struct Refreshing<T> {
    value: Option<T>,
    refreshing: bool,
    error: Option<UserFacingError>,
}

impl<T> Default for Refreshing<T> {
    fn default() -> Self {
        Self { value: None, refreshing: false, error: None }
    }
}

impl<T> Refreshing<T> {
    pub fn begin(&mut self) {
        self.refreshing = true;
        self.error = None;
    }

    pub fn succeed(&mut self, value: T) {
        self.value = Some(value);
        self.refreshing = false;
        self.error = None;
    }

    pub fn fail(&mut self, error: UserFacingError) {
        self.refreshing = false;
        self.error = Some(error);
    }

    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FocusIndex {
    current: usize,
}

impl FocusIndex {
    pub fn current(self) -> usize {
        self.current
    }

    pub fn set(&mut self, index: usize, count: usize) {
        self.current = index.min(count.saturating_sub(1));
    }

    pub fn advance(&mut self, count: usize, reverse: bool) {
        if count == 0 {
            self.current = 0;
        } else if reverse {
            self.current = (self.current + count - 1) % count;
        } else {
            self.current = (self.current + 1) % count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_error_contains_cause_and_next_step() {
        let error = UserFacingError::new("连接失败", "主机不可达", "检查地址后重试")
            .retry(RetryAction::Retry)
            .details("timeout");
        let message = error.message();
        assert!(message.contains("原因：主机不可达"));
        assert!(message.contains("建议：检查地址后重试"));
        assert!(message.contains("操作：请重试"));
    }

    #[test]
    fn refresh_keeps_last_successful_value_on_failure() {
        let mut state = Refreshing::default();
        state.succeed(vec![1, 2]);
        state.begin();
        state.fail(UserFacingError::new("刷新失败", "磁盘不可用", "稍后重试"));
        assert_eq!(state.value(), Some(&vec![1, 2]));
    }

    #[test]
    fn focus_wraps_in_both_directions() {
        let mut focus = FocusIndex::default();
        focus.advance(3, true);
        assert_eq!(focus.current(), 2);
        focus.advance(3, false);
        assert_eq!(focus.current(), 0);
    }
}
