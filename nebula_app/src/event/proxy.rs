//! Event-loop proxy bound to a movable terminal pane.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use nebula_terminal::event::{Event as TerminalEvent, EventListener};

use super::{Event, EventType};

#[derive(Debug, Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<Event>,
    /// Shared routing target lets a detached PTY move to a new window without
    /// replacing every proxy clone held by the terminal and I/O loop.
    window_id: Arc<AtomicU64>,
    tab_id: Option<u64>,
}

impl EventProxy {
    pub fn new(proxy: EventLoopProxy<Event>, window_id: WindowId) -> Self {
        Self { proxy, window_id: Arc::new(AtomicU64::new(window_id.into())), tab_id: None }
    }

    pub fn new_tab(proxy: EventLoopProxy<Event>, route: Arc<AtomicU64>, tab_id: u64) -> Self {
        Self { proxy, window_id: route, tab_id: Some(tab_id) }
    }

    fn target(&self) -> WindowId {
        WindowId::from(self.window_id.load(Ordering::Relaxed))
    }

    pub fn send_event(&self, event: EventType) {
        let _ = self.proxy.send_event(Event {
            window_id: Some(self.target()),
            tab_id: self.tab_id,
            payload: event,
        });
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TerminalEvent) {
        if let TerminalEvent::AiHookEnvelope(envelope) = event {
            if let Some(hook) = crate::ai_hook::parse_remote_envelope(&envelope, self.tab_id) {
                self.send_event(EventType::AiHook(hook));
            }
            return;
        }
        let _ = self.proxy.send_event(Event {
            window_id: Some(self.target()),
            tab_id: self.tab_id,
            payload: event.into(),
        });
    }
}
