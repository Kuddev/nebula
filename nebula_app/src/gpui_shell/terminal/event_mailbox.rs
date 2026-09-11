//! Coalesce repaint wakeups before enqueueing; retain every semantic event.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use futures::{Stream, channel::mpsc};
use nebula_terminal::event::Event;

#[derive(Clone)]
pub(super) struct EventSender {
    sender: mpsc::UnboundedSender<Event>,
    wake_pending: Arc<AtomicBool>,
}

pub(super) struct EventReceiver {
    receiver: mpsc::UnboundedReceiver<Event>,
    wake_pending: Arc<AtomicBool>,
}

pub(super) fn channel() -> (EventSender, EventReceiver) {
    let (sender, receiver) = mpsc::unbounded();
    let wake_pending = Arc::new(AtomicBool::new(false));
    (
        EventSender { sender, wake_pending: wake_pending.clone() },
        EventReceiver { receiver, wake_pending },
    )
}

impl EventSender {
    pub(super) fn send(&self, event: Event) {
        let wake = matches!(event, Event::Wakeup);
        if wake && self.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.sender.unbounded_send(event).is_err() && wake {
            self.wake_pending.store(false, Ordering::Release);
        }
    }
}

impl EventReceiver {
    fn acknowledge(&self, event: &Event) {
        if matches!(event, Event::Wakeup) {
            // Clear before processing: output arriving during UI work must
            // still enqueue a subsequent repaint, even on an inactive tab.
            self.wake_pending.store(false, Ordering::Release);
        }
    }

    pub(super) fn try_recv(&mut self) -> Result<Event, mpsc::TryRecvError> {
        let event = self.receiver.try_recv()?;
        self.acknowledge(&event);
        Ok(event)
    }
}

impl Stream for EventReceiver {
    type Item = Event;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        let result = Pin::new(&mut self.receiver).poll_next(cx);
        if let Poll::Ready(Some(event)) = &result {
            self.acknowledge(event);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighty_sessions_keep_all_agent_edges_through_a_repaint_flood() {
        let mut sessions: Vec<_> = (0..80).map(|_| channel()).collect();
        for (id, (sender, _)) in sessions.iter().enumerate() {
            sender.send(Event::CommandStart);
            for _ in 0..1000 {
                sender.send(Event::Wakeup);
            }
            sender.send(Event::AiHookEnvelope(format!("working:{id}").into_bytes()));
            sender.send(Event::AiHookEnvelope(format!("attention:{id}").into_bytes()));
            sender.send(Event::AiHookEnvelope(format!("done:{id}").into_bytes()));
            sender.send(Event::CommandDone { exit_code: Some(0) });
        }
        for (id, (sender, receiver)) in sessions.iter_mut().enumerate() {
            assert!(matches!(receiver.try_recv().unwrap(), Event::CommandStart));
            assert!(matches!(receiver.try_recv().unwrap(), Event::Wakeup));
            for edge in ["working", "attention", "done"] {
                let Event::AiHookEnvelope(bytes) = receiver.try_recv().unwrap() else {
                    panic!("lost agent edge")
                };
                assert_eq!(bytes, format!("{edge}:{id}").into_bytes());
            }
            assert!(matches!(
                receiver.try_recv().unwrap(),
                Event::CommandDone { exit_code: Some(0) }
            ));
            assert!(receiver.try_recv().is_err());
            sender.send(Event::Wakeup);
            assert!(matches!(receiver.try_recv().unwrap(), Event::Wakeup));
        }
    }

    #[test]
    fn sender_clones_share_wakeup_state_but_retain_exit() {
        let (sender, mut receiver) = channel();
        sender.send(Event::Wakeup);
        sender.clone().send(Event::Wakeup);
        sender.send(Event::Exit);
        assert!(matches!(receiver.try_recv().unwrap(), Event::Wakeup));
        assert!(matches!(receiver.try_recv().unwrap(), Event::Exit));
        assert!(receiver.try_recv().is_err());
    }
}
