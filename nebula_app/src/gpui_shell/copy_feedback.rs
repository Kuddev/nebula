//! Short-lived visual feedback for clipboard actions.
//!
//! Copy controls use this state for the small, local "copy → check" transition.
//! It deliberately lives with the control's GPUI entity instead of in a global
//! toast or application singleton: dropping the control also drops its timer.

use std::time::Duration;

use gpui::{Context, Task};

/// How long a successful copy control remains in its checked state.
pub(crate) const COPY_FEEDBACK_TTL: Duration = Duration::from_millis(1500);

/// Per-control copy confirmation state.
///
/// The clipboard API currently reports writes as a best-effort `()`. Callers
/// should only invoke [`Self::mark_copied`] after they have produced a valid
/// clipboard payload; error/unavailable paths must leave the state unchanged.
#[derive(Default)]
pub(crate) struct CopyFeedback {
    copied: bool,
    pending: bool,
    generation: u64,
    expiry: Option<Task<()>>,
}

impl CopyFeedback {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_copied(&self) -> bool {
        self.copied
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }

    /// Keep the action visible while an asynchronous clipboard payload is
    /// being prepared.  Starting a new operation also cancels any old
    /// success expiry owned by this control.
    pub(crate) fn mark_pending(&mut self, cx: &mut Context<Self>) {
        self.expiry = None;
        self.generation = self.generation.wrapping_add(1);
        self.pending = true;
        self.copied = false;
        cx.notify();
    }

    /// Enter the checked state and schedule its bounded automatic reset.
    ///
    /// A generation guard prevents an older timer from clearing a newer copy
    /// confirmation when the user clicks repeatedly. Replacing `expiry` also
    /// drops the previous task, so repeated clicks do not accumulate timers.
    pub(crate) fn mark_copied(&mut self, cx: &mut Context<Self>) {
        self.expiry = None;
        self.pending = false;
        self.copied = true;
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.expiry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPY_FEEDBACK_TTL).await;
            let _ = this.update(cx, |feedback, cx| {
                if feedback.generation == generation {
                    feedback.copied = false;
                    feedback.expiry = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Leave the pending/success state without showing a false success result.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.expiry = None;
        self.generation = self.generation.wrapping_add(1);
        self.pending = false;
        self.copied = false;
        cx.notify();
    }
}

#[cfg(all(test, feature = "gpui-test-support"))]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn copy_feedback_uses_controlled_clock_for_success_expiry(cx: &mut TestAppContext) {
        let feedback = cx.update(|cx| cx.new(|_| CopyFeedback::new()));
        cx.update(|cx| {
            feedback.update(cx, |feedback, cx| feedback.mark_copied(cx));
        });
        assert!(cx.read(|cx| feedback.read(cx).is_copied()));

        cx.executor().advance_clock(COPY_FEEDBACK_TTL - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(cx.read(|cx| feedback.read(cx).is_copied()));

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!cx.read(|cx| feedback.read(cx).is_copied()));
    }

    #[gpui::test]
    fn starting_async_copy_clears_old_success_state(cx: &mut TestAppContext) {
        let feedback = cx.update(|cx| cx.new(|_| CopyFeedback::new()));
        cx.update(|cx| {
            feedback.update(cx, |feedback, cx| feedback.mark_copied(cx));
            feedback.update(cx, |feedback, cx| feedback.mark_pending(cx));
        });
        assert!(cx.read(|cx| feedback.read(cx).is_pending()));
        assert!(!cx.read(|cx| feedback.read(cx).is_copied()));
    }
}
