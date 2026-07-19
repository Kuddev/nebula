//! Handle input from winit.
//!
//! The public module owns the processor/context contracts. Concrete input
//! responsibilities live in focused submodules so chrome routing, terminal
//! mouse handling, touch gestures, and action dispatch can evolve independently.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::marker::PhantomData;

use winit::event::Modifiers;
#[cfg(target_os = "macos")]
use winit::event_loop::ActiveEventLoop;

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::Scroll;
use nebula_terminal::index::{Direction, Point, Side};
use nebula_terminal::selection::SelectionType;
use nebula_terminal::term::search::Match;
use nebula_terminal::term::{ClipboardType, Term};

use crate::clipboard::Clipboard;
use crate::config::UiConfig;
use crate::display::hint::HintMatch;
use crate::display::window::Window;
use crate::display::{Display, SizeInfo};
use crate::event::{InlineSearchState, Mouse, TouchPurpose};
use crate::message_bar::Message;
use crate::scheduler::Scheduler;

mod action;
mod chrome;
pub mod keyboard;
mod mouse;
mod touch;

#[cfg(test)]
mod tests;

use action::Execute;

/// Font size change interval in px.
pub const FONT_SIZE_STEP: f32 = 1.;

pub struct Processor<T: EventListener, A: ActionContext<T>> {
    pub ctx: A,
    _phantom: PhantomData<T>,
}

pub trait ActionContext<T: EventListener> {
    fn write_to_pty<B: Into<Cow<'static, [u8]>>>(&self, _data: B) {}
    fn mark_dirty(&mut self) {}
    fn size_info(&self) -> SizeInfo;
    fn copy_selection(&mut self, _ty: ClipboardType) {}
    fn start_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn toggle_selection(&mut self, _ty: SelectionType, _point: Point, _side: Side) {}
    fn update_selection(&mut self, _point: Point, _side: Side) {}
    fn clear_selection(&mut self) {}
    fn selection_is_empty(&self) -> bool;
    fn mouse_mut(&mut self) -> &mut Mouse;
    fn mouse(&self) -> &Mouse;
    fn touch_purpose(&mut self) -> &mut TouchPurpose;
    fn modifiers(&mut self) -> &mut Modifiers;
    fn scroll(&mut self, _scroll: Scroll) {}
    fn window(&mut self) -> &mut Window;
    fn display(&mut self) -> &mut Display;
    /// Stable identity of the pane receiving this processor's terminal input.
    fn pane_id(&self) -> u64 {
        u64::MAX
    }
    /// Whether this context owns Nebula's window chrome. Unit tests that only
    /// exercise terminal selection can turn it off instead of constructing an
    /// OpenGL Display just to get through unrelated hit-testing.
    fn nebula_chrome_active(&self) -> bool {
        true
    }
    fn terminal(&self) -> &Term<T>;
    fn terminal_mut(&mut self) -> &mut Term<T>;
    fn nebula_accept(&self) -> crate::display::AcceptKey {
        crate::display::AcceptKey::default()
    }
    fn nebula_take_suggestion(&mut self) -> String {
        String::new()
    }
    fn nebula_input_char(&mut self, _c: char) {}
    fn nebula_input_text(&mut self, _text: &str) {}
    fn nebula_input_backspace(&mut self) {}
    fn nebula_delete_word(&mut self) {}
    fn nebula_commit_line(&mut self) {}
    fn nebula_clear_line(&mut self) {}
    fn spawn_new_instance(&mut self) {}
    /// Send a Nebula tab management request for this window.
    fn nebula_tab(&self, _request: crate::event::TabRequest) {}
    /// Open the SFTP drawer through this window's event proxy.
    fn nebula_open_sftp(&mut self, _destination: String) {}
    /// Stable SSH identity of the pane receiving this input, when it is a
    /// native SSH pane. Local panes deliberately return `None`.
    fn nebula_ssh_destination(&self) -> Option<&str> {
        None
    }
    /// Open a filesystem path with the system handler (drawer double-click).
    fn open_path(&mut self, _path: &std::path::Path) {}
    /// The active tab's document view, when it is a viewer tab (no pane):
    /// wheel and navigation keys scroll this instead of the grid.
    fn doc_view(&mut self) -> Option<&mut crate::display::markdown_view::DocView> {
        None
    }
    #[cfg(target_os = "macos")]
    fn create_new_window(&mut self, _tabbing_id: Option<String>) {}
    #[cfg(not(target_os = "macos"))]
    fn create_new_window(&mut self) {}
    fn change_font_size(&mut self, _delta: f32) {}
    fn reset_font_size(&mut self) {}
    fn pop_message(&mut self) {}
    fn message(&self) -> Option<&Message>;
    fn config(&self) -> &UiConfig;
    #[cfg(target_os = "macos")]
    fn event_loop(&self) -> &ActiveEventLoop;
    fn mouse_mode(&self) -> bool;
    fn clipboard_mut(&mut self) -> &mut Clipboard;
    fn scheduler_mut(&mut self) -> &mut Scheduler;
    fn start_search(&mut self, _direction: Direction) {}
    fn start_seeded_search(&mut self, _direction: Direction, _text: String) {}
    fn confirm_search(&mut self) {}
    fn cancel_search(&mut self) {}
    fn search_input(&mut self, _c: char) {}
    fn search_pop_word(&mut self) {}
    fn search_history_previous(&mut self) {}
    fn search_history_next(&mut self) {}
    fn search_next(&mut self, origin: Point, direction: Direction, side: Side) -> Option<Match>;
    fn advance_search_origin(&mut self, _direction: Direction) {}
    fn search_direction(&self) -> Direction;
    fn search_active(&self) -> bool;
    fn on_typing_start(&mut self) {}
    fn toggle_vi_mode(&mut self) {}
    fn inline_search_state(&mut self) -> &mut InlineSearchState;
    fn start_inline_search(&mut self, _direction: Direction, _stop_short: bool) {}
    fn inline_search_next(&mut self) {}
    fn inline_search_input(&mut self, _text: &str) {}
    fn inline_search_previous(&mut self) {}
    fn hint_input(&mut self, _character: char) {}
    fn trigger_hint(&mut self, _hint: &HintMatch) {}
    fn expand_selection(&mut self) {}
    fn semantic_word(&self, point: Point) -> String;
    fn on_terminal_input_start(&mut self) {}
    fn paste(&mut self, _text: &str, _bracketed: bool) {}
    /// Paste without the multi-line confirmation gate (used by the confirm
    /// modal's Enter handler once the user approved).
    fn paste_now(&mut self, _text: &str, _bracketed: bool) {}
    fn spawn_daemon<I, S>(&self, _program: &str, _args: I)
    where
        I: IntoIterator<Item = S> + Debug + Copy,
        S: AsRef<OsStr>,
    {
    }
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    pub fn new(ctx: A) -> Self {
        Self { ctx, _phantom: Default::default() }
    }
}
