//! Handle input from winit.
//!
//! Certain key combinations should send some escape sequence back to the PTY.
//! In order to figure that out, state about which modifier keys are pressed
//! needs to be tracked. Additionally, we need a bit of a state machine to
//! determine what to do when a non-modifier key is pressed.

use std::borrow::Cow;
use std::cmp::{Ordering, max, min};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::mem;
use std::time::{Duration, Instant};

use log::debug;
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, Modifiers, MouseButton, MouseScrollDelta, Touch as TouchEvent, TouchPhase,
};
#[cfg(target_os = "macos")]
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
#[cfg(target_os = "macos")]
use winit::platform::macos::ActiveEventLoopExtMacOS;
use winit::window::CursorIcon;

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use nebula_terminal::selection::SelectionType;
use nebula_terminal::term::search::Match;
use nebula_terminal::term::{ClipboardType, Term, TermMode};
use nebula_terminal::vi_mode::ViMotion;
use nebula_terminal::vte::ansi::{ClearMode, Handler};

use crate::clipboard::Clipboard;
#[cfg(target_os = "macos")]
use crate::config::window::Decorations;
use crate::config::{
    Action, BindingMode, MouseAction, MouseEvent, SearchAction, UiConfig, ViAction,
};
use crate::display::hint::HintMatch;
use crate::display::window::{ImeInhibitor, Window};
use crate::display::{Display, SizeInfo};
use crate::event::{
    ClickState, Event, EventType, InlineSearchState, Mouse, TouchPurpose, TouchZoom,
};
use crate::message_bar::{self, Message};
use crate::scheduler::{Scheduler, TimerId, Topic};

pub mod keyboard;

/// Font size change interval in px.
pub const FONT_SIZE_STEP: f32 = 1.;

/// Interval for mouse scrolling during selection outside of the boundaries.
const SELECTION_SCROLLING_INTERVAL: Duration = Duration::from_millis(15);

/// Minimum number of pixels at the bottom/top where selection scrolling is performed.
const MIN_SELECTION_SCROLLING_HEIGHT: f64 = 5.;

/// Number of pixels for increasing the selection scrolling speed factor by one.
const SELECTION_SCROLLING_STEP: f64 = 20.;

/// Distance before a touch input is considered a drag.
const MAX_TAP_DISTANCE: f64 = 20.;

/// Fallback double/triple-click interval where the OS setting isn't
/// available; Windows uses the user's control-panel value instead.
#[cfg(not(windows))]
const CLICK_THRESHOLD: Duration = Duration::from_millis(400);

/// Multi-click interval: the user's system double-click time on Windows.
#[cfg(windows)]
fn multi_click_time() -> Duration {
    let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    Duration::from_millis(u64::from(ms))
}

#[cfg(not(windows))]
fn multi_click_time() -> Duration {
    CLICK_THRESHOLD
}

/// Half the system double-click rectangle: how far apart two presses may
/// land (per axis) and still count as one multi-click sequence.
#[cfg(windows)]
fn double_click_slop() -> (f32, f32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
    };
    let half = |v: i32| (v.max(4) as f32) / 2.0;
    unsafe { (half(GetSystemMetrics(SM_CXDOUBLECLK)), half(GetSystemMetrics(SM_CYDOUBLECLK))) }
}

#[cfg(not(windows))]
fn double_click_slop() -> (f32, f32) {
    (4.0, 4.0)
}

/// Processes input from winit.
///
/// An escape sequence may be emitted in case specific keys or key combinations
/// are activated.
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

impl Action {
    fn toggle_selection<T, A>(ctx: &mut A, ty: SelectionType)
    where
        A: ActionContext<T>,
        T: EventListener,
    {
        ctx.toggle_selection(ty, ctx.terminal().vi_mode_cursor.point, Side::Left);

        // Make sure initial selection is not empty.
        if let Some(selection) = &mut ctx.terminal_mut().selection {
            selection.include_all();
        }
    }
}

trait Execute<T: EventListener> {
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A);
}

impl<T: EventListener> Execute<T> for Action {
    #[inline]
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A) {
        match self {
            Action::Esc(s) => ctx.paste(s, false),
            Action::Command(program) => ctx.spawn_daemon(program.program(), program.args()),
            Action::Hint(hint) => {
                ctx.display().hint_state.start(hint.clone());
                ctx.mark_dirty();
            },
            Action::ToggleViMode => {
                ctx.on_typing_start();
                ctx.toggle_vi_mode()
            },
            action @ (Action::ViMotion(_) | Action::Vi(_))
                if !ctx.terminal().mode().contains(TermMode::VI) =>
            {
                debug!("Ignoring {action:?}: Vi mode inactive");
            },
            Action::ViMotion(motion) => {
                ctx.on_typing_start();
                ctx.terminal_mut().vi_motion(*motion);
                ctx.mark_dirty();
            },
            Action::Vi(ViAction::ToggleNormalSelection) => {
                Self::toggle_selection(ctx, SelectionType::Simple);
            },
            Action::Vi(ViAction::ToggleLineSelection) => {
                Self::toggle_selection(ctx, SelectionType::Lines);
            },
            Action::Vi(ViAction::ToggleBlockSelection) => {
                Self::toggle_selection(ctx, SelectionType::Block);
            },
            Action::Vi(ViAction::ToggleSemanticSelection) => {
                Self::toggle_selection(ctx, SelectionType::Semantic);
            },
            Action::Vi(ViAction::Open) => {
                let hint = ctx.display().vi_highlighted_hint.take();
                if let Some(hint) = &hint {
                    ctx.mouse_mut().block_hint_launcher = false;
                    ctx.trigger_hint(hint);
                }
                ctx.display().vi_highlighted_hint = hint;
            },
            Action::Vi(ViAction::SearchNext) => {
                ctx.on_typing_start();

                let terminal = ctx.terminal();
                let direction = ctx.search_direction();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchPrevious) => {
                ctx.on_typing_start();

                let terminal = ctx.terminal();
                let direction = ctx.search_direction().opposite();
                let vi_point = terminal.vi_mode_cursor.point;
                let origin = match direction {
                    Direction::Right => vi_point.add(terminal, Boundary::None, 1),
                    Direction::Left => vi_point.sub(terminal, Boundary::None, 1),
                };

                if let Some(regex_match) = ctx.search_next(origin, direction, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchStart) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.sub(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Left, Side::Left) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.start());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::SearchEnd) => {
                let terminal = ctx.terminal();
                let origin = terminal.vi_mode_cursor.point.add(terminal, Boundary::None, 1);

                if let Some(regex_match) = ctx.search_next(origin, Direction::Right, Side::Right) {
                    ctx.terminal_mut().vi_goto_point(*regex_match.end());
                    ctx.mark_dirty();
                }
            },
            Action::Vi(ViAction::CenterAroundViCursor) => {
                let term = ctx.terminal();
                let display_offset = term.grid().display_offset() as i32;
                let target = -display_offset + term.screen_lines() as i32 / 2 - 1;
                let line = term.vi_mode_cursor.point.line;
                let scroll_lines = target - line.0;

                ctx.scroll(Scroll::Delta(scroll_lines));
            },
            Action::Vi(ViAction::InlineSearchForward) => {
                ctx.start_inline_search(Direction::Right, false)
            },
            Action::Vi(ViAction::InlineSearchBackward) => {
                ctx.start_inline_search(Direction::Left, false)
            },
            Action::Vi(ViAction::InlineSearchForwardShort) => {
                ctx.start_inline_search(Direction::Right, true)
            },
            Action::Vi(ViAction::InlineSearchBackwardShort) => {
                ctx.start_inline_search(Direction::Left, true)
            },
            Action::Vi(ViAction::InlineSearchNext) => ctx.inline_search_next(),
            Action::Vi(ViAction::InlineSearchPrevious) => ctx.inline_search_previous(),
            Action::Vi(ViAction::SemanticSearchForward | ViAction::SemanticSearchBackward) => {
                let seed_text = match ctx.terminal().selection_to_string() {
                    Some(selection) if !selection.is_empty() => selection,
                    // Get semantic word at the vi cursor position.
                    _ => ctx.semantic_word(ctx.terminal().vi_mode_cursor.point),
                };

                if !seed_text.is_empty() {
                    let direction = match self {
                        Action::Vi(ViAction::SemanticSearchForward) => Direction::Right,
                        _ => Direction::Left,
                    };
                    ctx.start_seeded_search(direction, seed_text);
                }
            },
            action @ Action::Search(_) if !ctx.search_active() => {
                debug!("Ignoring {action:?}: Search mode inactive");
            },
            Action::Search(SearchAction::SearchFocusNext) => {
                ctx.advance_search_origin(ctx.search_direction());
            },
            Action::Search(SearchAction::SearchFocusPrevious) => {
                let direction = ctx.search_direction().opposite();
                ctx.advance_search_origin(direction);
            },
            Action::Search(SearchAction::SearchConfirm) => ctx.confirm_search(),
            Action::Search(SearchAction::SearchCancel) => ctx.cancel_search(),
            Action::Search(SearchAction::SearchClear) => {
                let direction = ctx.search_direction();
                ctx.cancel_search();
                ctx.start_search(direction);
            },
            Action::Search(SearchAction::SearchDeleteWord) => ctx.search_pop_word(),
            Action::Search(SearchAction::SearchHistoryPrevious) => ctx.search_history_previous(),
            Action::Search(SearchAction::SearchHistoryNext) => ctx.search_history_next(),
            Action::Mouse(MouseAction::ExpandSelection) => ctx.expand_selection(),
            Action::SearchForward => ctx.start_search(Direction::Right),
            Action::SearchBackward => ctx.start_search(Direction::Left),
            Action::Copy => ctx.copy_selection(ClipboardType::Clipboard),
            #[cfg(not(any(target_os = "macos", windows)))]
            Action::CopySelection => ctx.copy_selection(ClipboardType::Selection),
            Action::ClearSelection => ctx.clear_selection(),
            Action::Paste => {
                let text = ctx.clipboard_mut().load(ClipboardType::Clipboard);
                ctx.paste(&text, true);
            },
            Action::PasteSelection => {
                let text = ctx.clipboard_mut().load(ClipboardType::Selection);
                ctx.paste(&text, true);
            },
            Action::ToggleFullscreen => ctx.window().toggle_fullscreen(),
            Action::ToggleMaximized => ctx.window().toggle_maximized(),
            #[cfg(target_os = "macos")]
            Action::ToggleSimpleFullscreen => ctx.window().toggle_simple_fullscreen(),
            #[cfg(target_os = "macos")]
            Action::Hide => ctx.event_loop().hide_application(),
            #[cfg(target_os = "macos")]
            Action::HideOtherApplications => ctx.event_loop().hide_other_applications(),
            #[cfg(not(target_os = "macos"))]
            Action::Hide => ctx.window().set_visible(false),
            Action::Minimize => ctx.window().set_minimized(true),
            Action::Quit => {
                ctx.window().hold = false;
                ctx.terminal_mut().exit();
            },
            Action::IncreaseFontSize => ctx.change_font_size(FONT_SIZE_STEP),
            Action::DecreaseFontSize => ctx.change_font_size(-FONT_SIZE_STEP),
            Action::ResetFontSize => ctx.reset_font_size(),
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollHalfPageUp
            | Action::ScrollHalfPageDown => {
                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                let (scroll, amount) = match self {
                    Action::ScrollPageUp => (Scroll::PageUp, term.screen_lines() as i32),
                    Action::ScrollPageDown => (Scroll::PageDown, -(term.screen_lines() as i32)),
                    Action::ScrollHalfPageUp => {
                        let amount = term.screen_lines() as i32 / 2;
                        (Scroll::Delta(amount), amount)
                    },
                    Action::ScrollHalfPageDown => {
                        let amount = -(term.screen_lines() as i32 / 2);
                        (Scroll::Delta(amount), amount)
                    },
                    _ => unreachable!(),
                };

                let old_vi_cursor = term.vi_mode_cursor;
                term.vi_mode_cursor = term.vi_mode_cursor.scroll(term, amount);
                if old_vi_cursor != term.vi_mode_cursor {
                    ctx.mark_dirty();
                }

                ctx.scroll(scroll);
            },
            Action::ScrollLineUp => ctx.scroll(Scroll::Delta(1)),
            Action::ScrollLineDown => ctx.scroll(Scroll::Delta(-1)),
            Action::ScrollToTop => {
                ctx.scroll(Scroll::Top);

                // Move vi mode cursor.
                let topmost_line = ctx.terminal().topmost_line();
                ctx.terminal_mut().vi_mode_cursor.point.line = topmost_line;
                ctx.terminal_mut().vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ScrollToBottom => {
                ctx.scroll(Scroll::Bottom);

                // Move vi mode cursor.
                let term = ctx.terminal_mut();
                term.vi_mode_cursor.point.line = term.bottommost_line();

                // Move to beginning twice, to always jump across linewraps.
                term.vi_motion(ViMotion::FirstOccupied);
                term.vi_motion(ViMotion::FirstOccupied);
                ctx.mark_dirty();
            },
            Action::ClearHistory => ctx.terminal_mut().clear_screen(ClearMode::Saved),
            Action::ClearLogNotice => ctx.pop_message(),
            #[cfg(not(target_os = "macos"))]
            Action::CreateNewWindow => ctx.create_new_window(),
            Action::SpawnNewInstance => ctx.spawn_new_instance(),
            #[cfg(target_os = "macos")]
            Action::CreateNewWindow => ctx.create_new_window(None),
            #[cfg(target_os = "macos")]
            Action::CreateNewTab => {
                // Tabs on macOS are not possible without decorations.
                if ctx.config().window.decorations != Decorations::None {
                    let tabbing_id = Some(ctx.window().tabbing_id());
                    ctx.create_new_window(tabbing_id);
                }
            },
            // Elsewhere the tab actions drive Nebula's own tab bar, so config
            // `[[keyboard.bindings]]` can remap them freely (设置→按键映射).
            #[cfg(not(target_os = "macos"))]
            Action::CreateNewTab => ctx.nebula_tab(crate::event::TabRequest::New),
            #[cfg(not(target_os = "macos"))]
            Action::SelectNextTab => ctx.nebula_tab(crate::event::TabRequest::SelectNext),
            #[cfg(not(target_os = "macos"))]
            Action::SelectPreviousTab => ctx.nebula_tab(crate::event::TabRequest::SelectPrev),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab1 => ctx.nebula_tab(crate::event::TabRequest::Select(0)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab2 => ctx.nebula_tab(crate::event::TabRequest::Select(1)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab3 => ctx.nebula_tab(crate::event::TabRequest::Select(2)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab4 => ctx.nebula_tab(crate::event::TabRequest::Select(3)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab5 => ctx.nebula_tab(crate::event::TabRequest::Select(4)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab6 => ctx.nebula_tab(crate::event::TabRequest::Select(5)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab7 => ctx.nebula_tab(crate::event::TabRequest::Select(6)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab8 => ctx.nebula_tab(crate::event::TabRequest::Select(7)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab9 => ctx.nebula_tab(crate::event::TabRequest::Select(8)),
            #[cfg(not(target_os = "macos"))]
            Action::SelectLastTab => {
                // The window context clamps out-of-range indices; usize::MAX
                // is "last" only via SelectTab-specific handling, so send a
                // large index the clamp folds to the final tab.
                ctx.nebula_tab(crate::event::TabRequest::SelectLast)
            },
            #[cfg(target_os = "macos")]
            Action::SelectNextTab => ctx.window().select_next_tab(),
            #[cfg(target_os = "macos")]
            Action::SelectPreviousTab => ctx.window().select_previous_tab(),
            #[cfg(target_os = "macos")]
            Action::SelectTab1 => ctx.window().select_tab_at_index(0),
            #[cfg(target_os = "macos")]
            Action::SelectTab2 => ctx.window().select_tab_at_index(1),
            #[cfg(target_os = "macos")]
            Action::SelectTab3 => ctx.window().select_tab_at_index(2),
            #[cfg(target_os = "macos")]
            Action::SelectTab4 => ctx.window().select_tab_at_index(3),
            #[cfg(target_os = "macos")]
            Action::SelectTab5 => ctx.window().select_tab_at_index(4),
            #[cfg(target_os = "macos")]
            Action::SelectTab6 => ctx.window().select_tab_at_index(5),
            #[cfg(target_os = "macos")]
            Action::SelectTab7 => ctx.window().select_tab_at_index(6),
            #[cfg(target_os = "macos")]
            Action::SelectTab8 => ctx.window().select_tab_at_index(7),
            #[cfg(target_os = "macos")]
            Action::SelectTab9 => ctx.window().select_tab_at_index(8),
            #[cfg(target_os = "macos")]
            Action::SelectLastTab => ctx.window().select_last_tab(),
            _ => (),
        }
    }
}

impl<T: EventListener, A: ActionContext<T>> Processor<T, A> {
    pub fn new(ctx: A) -> Self {
        Self { ctx, _phantom: Default::default() }
    }

    #[inline]
    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let size_info = self.ctx.size_info();
        // Chrome (tabs / title bar / settings) is laid out on the *window*,
        // not the focused pane's viewport; in split mode the two differ.
        let window_size = self.ctx.display().size_info;

        let (x, y) = position.into();

        let lmb_pressed = self.ctx.mouse().left_button_state == ElementState::Pressed;
        let rmb_pressed = self.ctx.mouse().right_button_state == ElementState::Pressed;
        if !self.ctx.selection_is_empty() && (lmb_pressed || rmb_pressed) {
            self.update_selection_scrolling(y);
        }

        let display_offset = self.ctx.terminal().grid().display_offset();
        let old_point = self.ctx.mouse().point(&size_info, display_offset);

        // Clamp to the window, not the pane: chrome to the right/bottom of a
        // split pane must stay hoverable and clickable.
        let x = x.clamp(0, window_size.width() as i32 - 1) as usize;
        let y = y.clamp(0, window_size.height() as i32 - 1) as usize;
        self.ctx.mouse_mut().x = x;
        self.ctx.mouse_mut().y = y;

        // Nebula: while the left button holds a grabbed tab, pointer motion
        // drives the reorder drag instead of hover / text selection.
        if lmb_pressed && self.ctx.display().tab_drag_armed() {
            let active = self.ctx.display().update_tab_drag(x as f32, y as f32);
            let icon = if active { CursorIcon::Grabbing } else { CursorIcon::Pointer };
            self.ctx.window().set_mouse_cursor(icon);
            if active {
                self.ctx.mark_dirty();
            }
            return;
        }

        // Nebula: while the left button holds the scrollback thumb, pointer
        // motion maps 1:1 onto the display offset (scrollbar drag).
        if let Some(grab) = self.ctx.display().nebula_scrollbar_drag {
            if lmb_pressed {
                let total_lines = self.ctx.terminal().total_lines();
                let display_offset = self.ctx.terminal().grid().display_offset();
                let target = self.ctx.display().scrollbar_target_offset(
                    &size_info,
                    total_lines,
                    y as f32,
                    grab,
                );
                let delta = target as i32 - display_offset as i32;
                if delta != 0 {
                    self.ctx.scroll(Scroll::Delta(delta));
                }
                self.ctx.mark_dirty();
                return;
            }
            // Button no longer held (release happened elsewhere): drop the drag.
            self.ctx.display().nebula_scrollbar_drag = None;
        }

        // Nebula chrome: hover state must be updated on raw pixel movement,
        // not only when the terminal cell changes; otherwise tabs/buttons feel
        // like they have no feedback on high-DPI displays.
        let scale = self.ctx.window().scale_factor as f32;
        let settings_open = self.ctx.display().settings_open();
        let settings_section = self.ctx.display().settings_section();
        let settings_scroll = self.ctx.display().settings_scroll();
        let shell_picker_open = self.ctx.display().nebula_shell_picker_open;
        let shell_picker_count = self.ctx.display().shell_picker_count();
        let settings_hover = crate::display::settings_hit(
            &window_size,
            scale,
            x as f32,
            y as f32,
            settings_open,
            settings_section,
            settings_scroll,
            shell_picker_open,
            shell_picker_count,
        );
        let chrome_hover = if crate::display::in_chrome_bar(&window_size, scale, x as f32, y as f32)
        {
            self.ctx.display().chrome_hit(x as f32, y as f32)
        } else {
            crate::display::ChromeHit::None
        };
        self.ctx.display().set_chrome_hover(chrome_hover, settings_hover);

        // Resize cursor on the window border, arrow over the title bar/sidebar,
        // pointer over clickable chrome controls. Chrome/resize geometry is
        // window-level, so hit-test against the full window, not the focused
        // pane's viewport — otherwise a band of the terminal area is mistaken
        // for chrome and swallows link hovers (no underline / no click).
        if let Some(dir) = crate::display::resize_edge(&window_size, scale, x as f32, y as f32) {
            use winit::window::ResizeDirection::*;
            let icon = match dir {
                East | West => CursorIcon::EwResize,
                North | South => CursorIcon::NsResize,
                NorthEast | SouthWest => CursorIcon::NeswResize,
                NorthWest | SouthEast => CursorIcon::NwseResize,
            };
            self.ctx.window().set_mouse_cursor(icon);
            return;
        }
        match settings_hover {
            crate::display::SettingsHit::Toggle
            | crate::display::SettingsHit::Nav(_)
            | crate::display::SettingsHit::Theme(_)
            | crate::display::SettingsHit::GhostToggle
            | crate::display::SettingsHit::AcceptCycle
            | crate::display::SettingsHit::ShellCycle
            | crate::display::SettingsHit::ShellPickerRow(_)
            | crate::display::SettingsHit::FetchToggle
            | crate::display::SettingsHit::PowerlineToggle
            | crate::display::SettingsHit::KeepSessionToggle
            | crate::display::SettingsHit::OpacityDown
            | crate::display::SettingsHit::OpacityUp
            | crate::display::SettingsHit::BackgroundColor
            | crate::display::SettingsHit::BackgroundImage
            | crate::display::SettingsHit::OpenConfigFile
            | crate::display::SettingsHit::Reset => {
                self.ctx.window().set_mouse_cursor(CursorIcon::Pointer);
                return;
            },
            crate::display::SettingsHit::Panel => {
                self.ctx.window().set_mouse_cursor(CursorIcon::Default);
                return;
            },
            crate::display::SettingsHit::Dismiss | crate::display::SettingsHit::None => {},
        }
        if crate::display::in_chrome_bar(&window_size, scale, x as f32, y as f32) {
            let icon = match self.ctx.display().chrome_hit(x as f32, y as f32) {
                crate::display::ChromeHit::Minimize
                | crate::display::ChromeHit::Maximize
                | crate::display::ChromeHit::Close
                | crate::display::ChromeHit::NewTab
                | crate::display::ChromeHit::NewTabMenu
                | crate::display::ChromeHit::Tab(_)
                | crate::display::ChromeHit::TabClose(_)
                | crate::display::ChromeHit::PanelFiles
                | crate::display::ChromeHit::PanelGit
                | crate::display::ChromeHit::Host(_)
                | crate::display::ChromeHit::TabsSection
                | crate::display::ChromeHit::HostsSection
                | crate::display::ChromeHit::SidebarToggle => CursorIcon::Pointer,
                _ => CursorIcon::Default,
            };
            self.ctx.window().set_mouse_cursor(icon);
            return;
        }

        // Drawer hover: rows / header tabs / action buttons light up, and the
        // pointer picks the matching cursor. The drawer overlays the grid, so
        // while the pointer is on it nothing below may react (no link hover,
        // no beam cursor bleeding through). Skipped while the left button is
        // down — an in-progress text drag-selection sweeping across the drawer
        // must keep updating, not freeze at its edge.
        if self.ctx.display().nebula_side_panel.open
            && self.ctx.mouse().left_button_state != ElementState::Pressed
        {
            use crate::display::side_panel::{PanelHit, PanelView, panel_hit};
            let px = x as f32;
            let py = y as f32;
            let layout = self.ctx.display().side_panel_layout();
            let hit = panel_hit(&layout, px, py);
            let panel = &mut self.ctx.display().nebula_side_panel;
            if hit != panel.hover || (hit != PanelHit::None && panel.hover_pos != (px, py)) {
                panel.hover = hit;
                panel.hover_pos = (px, py);
                self.ctx.mark_dirty();
            }
            if hit != PanelHit::None {
                let files = self.ctx.display().nebula_side_panel.view == PanelView::Files;
                let icon = match hit {
                    PanelHit::ViewFiles | PanelHit::ViewGit | PanelHit::Row(_) => {
                        CursorIcon::Pointer
                    },
                    PanelHit::Search if files => CursorIcon::Text,
                    PanelHit::Search => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                };
                self.ctx.window().set_mouse_cursor(icon);
                return;
            }
        } else if self.ctx.display().nebula_side_panel.hover
            != crate::display::side_panel::PanelHit::None
        {
            self.ctx.display().nebula_side_panel.hover = crate::display::side_panel::PanelHit::None;
            self.ctx.mark_dirty();
        }

        // Command palette hover: light up the row under the cursor so the user
        // knows where they'd click. Only track while the palette is open.
        if self.ctx.display().command_palette_open() {
            let px = x as f32;
            let py = y as f32;
            let with_input = !self.ctx.display().command_palette_picking_default();
            let layout = crate::display::command_palette::palette_layout(
                window_size.width(),
                window_size.height(),
                scale,
                with_input,
            );
            let (ix, iy, iw, ih) = layout.panel;
            // Hover detection: the pointer must be inside the panel rectangle,
            // AND below the input box (list_y), AND the computed row must be a
            // real item (< visible count). Without the visible-count check, rows
            // beyond the filtered list — empty space inside the panel — also hover.
            let hover_row = if px >= ix && px < ix + iw && py >= layout.list_y && py < iy + ih {
                let row = ((py - layout.list_y) / layout.row_h) as usize;
                let visible_count =
                    self.ctx.display().nebula_palette_visible_count().min(layout.max_rows);
                if row < visible_count { Some(row) } else { None }
            } else {
                None
            };
            self.ctx.display().palette_hover(hover_row);
        }

        let inside_text_area = size_info.contains_point(x, y);
        let cell_side = self.cell_side(x);

        // Activate a pending tree-file drag once the pointer travels; while
        // active, the ghost chip follows the pointer and the copy cursor
        // shows the drop affordance.
        if let Some(drag) = self.ctx.display().nebula_side_panel.drag_file.as_mut() {
            drag.pos = (x as f32, y as f32);
            if !drag.active {
                let (ox, oy) = drag.origin;
                if (x as f32 - ox).abs() >= 8.0 || (y as f32 - oy).abs() >= 8.0 {
                    drag.active = true;
                }
            }
            let active = drag.active;
            if active {
                self.ctx.mark_dirty();
                self.ctx.window().set_mouse_cursor(CursorIcon::Grabbing);
                return;
            }
        }

        let point = self.ctx.mouse().point(&size_info, display_offset);
        let cell_changed = old_point != point;

        // If the mouse hasn't changed cells, do nothing.
        if !cell_changed
            && self.ctx.mouse().cell_side == cell_side
            && self.ctx.mouse().inside_text_area == inside_text_area
        {
            return;
        }

        self.ctx.mouse_mut().inside_text_area = inside_text_area;
        self.ctx.mouse_mut().cell_side = cell_side;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        if (lmb_pressed || rmb_pressed)
            && (self.ctx.modifiers().state().shift_key() || !self.ctx.mouse_mode())
        {
            // Engage drag-selection only past a real drag distance: at least
            // half a cell (and never under 8px). The old 4px threshold was
            // inside ordinary click jitter, so a plain click kept leaving a
            // one-cell selection behind — Windows Terminal only selects once
            // the pointer actually travels, a click never does.
            let dragging = self.ctx.mouse().drag_active
                || self.ctx.mouse().drag_origin.is_some_and(|(ox, oy)| {
                    let scale = self.ctx.window().scale_factor as f32;
                    let tx = (8.0 * scale).max(size_info.cell_width() * 0.5) as f64;
                    let ty = (8.0 * scale).max(size_info.cell_height() * 0.5) as f64;
                    (x as f64 - ox as f64).abs() >= tx || (y as f64 - oy as f64).abs() >= ty
                });
            if dragging {
                let first = !self.ctx.mouse().drag_active;
                self.ctx.mouse_mut().drag_active = true;
                // A real drag is in progress — don't launch hints on release.
                self.ctx.mouse_mut().block_hint_launcher = true;
                // Crossing the threshold is what STARTS the selection (WT
                // model): anchor at the original press cell, not wherever the
                // pointer is by now. Double/triple clicks selected at press
                // and carry no pending entry — they just extend below.
                if first {
                    if let Some((ty, anchor, anchor_side)) =
                        self.ctx.mouse_mut().pending_selection.take()
                    {
                        self.ctx.start_selection(ty, anchor, anchor_side);
                    }
                }
                self.ctx.update_selection(point, cell_side);
            }
        } else if cell_changed
            && self.ctx.terminal().mode().intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG)
        {
            if lmb_pressed {
                self.mouse_report(32, ElementState::Pressed);
            } else if self.ctx.mouse().middle_button_state == ElementState::Pressed {
                self.mouse_report(33, ElementState::Pressed);
            } else if self.ctx.mouse().right_button_state == ElementState::Pressed {
                self.mouse_report(34, ElementState::Pressed);
            } else if self.ctx.terminal().mode().contains(TermMode::MOUSE_MOTION) {
                self.mouse_report(35, ElementState::Pressed);
            }
        }
    }

    /// Check which side of a cell an X coordinate lies on.
    fn cell_side(&self, x: usize) -> Side {
        let size_info = self.ctx.size_info();

        let cell_x =
            x.saturating_sub(size_info.padding_x() as usize) % size_info.cell_width() as usize;
        let half_cell_width = (size_info.cell_width() / 2.0) as usize;

        let additional_padding =
            (size_info.width() - size_info.padding_x() - size_info.padding_right())
                % size_info.cell_width();
        let end_of_grid = size_info.width() - size_info.padding_right() - additional_padding;

        if cell_x > half_cell_width
            // Edge case when mouse leaves the window.
            || x as f32 >= end_of_grid
        {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn mouse_report(&mut self, button: u8, state: ElementState) {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        // Assure the mouse point is not in the scrollback.
        if point.line < 0 {
            return;
        }

        // Calculate modifiers value.
        let mut mods = 0;
        let modifiers = self.ctx.modifiers().state();
        if modifiers.shift_key() {
            mods += 4;
        }
        if modifiers.alt_key() {
            mods += 8;
        }
        if modifiers.control_key() {
            mods += 16;
        }

        // Report mouse events.
        if self.ctx.terminal().mode().contains(TermMode::SGR_MOUSE) {
            self.sgr_mouse_report(point, button + mods, state);
        } else if let ElementState::Released = state {
            self.normal_mouse_report(point, 3 + mods);
        } else {
            self.normal_mouse_report(point, button + mods);
        }
    }

    fn normal_mouse_report(&mut self, point: Point, button: u8) {
        let Point { line, column } = point;
        let utf8 = self.ctx.terminal().mode().contains(TermMode::UTF8_MOUSE);

        let max_point = if utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.ctx.write_to_pty(msg);
    }

    fn sgr_mouse_report(&mut self, point: Point, button: u8, state: ElementState) {
        let c = match state {
            ElementState::Pressed => 'M',
            ElementState::Released => 'm',
        };

        let msg = format!("\x1b[<{};{};{}{}", button, point.column + 1, point.line + 1, c);
        self.ctx.write_to_pty(msg.into_bytes());
    }

    /// Approve the pending confirm modal: re-dispatch the gated close (the
    /// pending confirm matches, so the handler clears it and closes for real)
    /// or run the gated paste. Shared by the Enter key and the modal's
    /// primary button.
    pub fn nebula_confirm_accept(&mut self, confirm: crate::display::NebulaConfirm) {
        use crate::display::NebulaConfirm;
        match confirm {
            NebulaConfirm::ClosePane { .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::Close);
            },
            NebulaConfirm::CloseTab { index, .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::CloseIndex(index));
            },
            NebulaConfirm::CloseWindow { .. } => {
                self.ctx.nebula_tab(crate::event::TabRequest::CloseWindow);
            },
            NebulaConfirm::Paste { text, bracketed, .. } => {
                self.ctx.display().nebula_confirm = None;
                self.ctx.paste_now(&text, bracketed);
            },
        }
    }

    /// Advance the multi-click state machine for this press — exactly ONCE
    /// per press, at the top of `on_mouse_press` (WT's `_numberOfClicks`
    /// model). A press upgrades Click→Double→Triple only when it is the same
    /// button, within the system double-click time AND within half a cell of
    /// the previous press; anything else resets to a plain Click. The
    /// distance gate keeps "click somewhere, then immediately click-drag
    /// elsewhere" from being misread as a word/line drag.
    fn advance_click_state(&mut self, button: MouseButton) -> ClickState {
        let now = Instant::now();
        let mouse = self.ctx.mouse();
        let elapsed = now - mouse.last_click_timestamp;
        let (last_x, last_y) = mouse.last_click_pos;
        let (slop_x, slop_y) = double_click_slop();
        let near = (mouse.x as f32 - last_x as f32).abs() <= slop_x
            && (mouse.y as f32 - last_y as f32).abs() <= slop_y;
        let state = match mouse.click_state {
            _ if button != mouse.last_click_button || !near || elapsed >= multi_click_time() => {
                ClickState::Click
            },
            ClickState::Click => ClickState::DoubleClick,
            ClickState::DoubleClick => ClickState::TripleClick,
            _ => ClickState::Click,
        };
        let pos = (self.ctx.mouse().x, self.ctx.mouse().y);
        let mouse = self.ctx.mouse_mut();
        mouse.last_click_timestamp = now;
        mouse.last_click_button = button;
        mouse.last_click_pos = pos;
        mouse.click_state = state;
        state
    }

    fn on_mouse_press(&mut self, button: MouseButton) {
        // Multi-click bookkeeping happens here and nowhere else. It used to
        // be advanced both by the chrome block and the terminal block below;
        // the second advance saw elapsed≈0 and upgraded EVERY terminal click
        // to a double/triple — plain drags selected by word or whole line.
        self.advance_click_state(button);

        // A left press anywhere OUTSIDE the rename box ends the edit
        // (canceling, like Esc) — the click itself still lands wherever it
        // was aimed. Clicking inside the box is caret placement (below).
        if button == MouseButton::Left {
            if let Some((idx, _)) = self.ctx.display().nebula_tab_rename.clone() {
                let x = self.ctx.mouse().x as f32;
                let y = self.ctx.mouse().y as f32;
                if self.ctx.display().chrome_hit(x, y) != crate::display::ChromeHit::Tab(idx) {
                    self.ctx.nebula_tab(crate::event::TabRequest::CancelRename);
                }
            }
        }

        if button == MouseButton::Left && self.ctx.display().nebula_ssh_editor.is_some() {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            self.ctx.display().ssh_editor_click(x, y);
            self.ctx.mark_dirty();
            return;
        }

        // Nebula command palette: clicking a row runs it, clicking outside
        // dismisses — same modal semantics as the keyboard path.
        if button == MouseButton::Left && self.ctx.display().command_palette_open() {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let size = self.ctx.display().size_info;
            let scale = self.ctx.window().scale_factor as f32;
            let with_input = !self.ctx.display().command_palette_picking_default();
            let layout = crate::display::command_palette::palette_layout(
                size.width(),
                size.height(),
                scale,
                with_input,
            );
            let (px, py, pw, ph) = layout.panel;
            if x >= px && x < px + pw && y >= py && y < py + ph {
                if y >= layout.list_y {
                    let row = ((y - layout.list_y) / layout.row_h) as usize;
                    if let Some(action) = self.ctx.display().palette_click(row, layout.max_rows) {
                        self.run_palette_action(action);
                    }
                }
            } else {
                self.ctx.display().close_command_palette();
            }
            self.ctx.mark_dirty();
            return;
        }

        // Right-side drawer (directory tree / git): header tabs switch views,
        // directory rows expand/collapse. Sits under the modal layers, so
        // only when no modal owns the pointer.
        if button == MouseButton::Left
            && self.ctx.display().nebula_side_panel.open
            && !self.ctx.display().settings_open()
            && self.ctx.display().nebula_confirm.is_none()
        {
            use crate::display::side_panel::{PanelHit, PanelView, panel_hit};
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let layout = self.ctx.display().side_panel_layout();
            match panel_hit(&layout, x, y) {
                PanelHit::None => {
                    // Clicking anywhere outside the drawer drops search focus
                    // and the persistent file selection.
                    let panel = &mut self.ctx.display().nebula_side_panel;
                    if panel.search_focus || panel.selected.is_some() {
                        panel.search_unfocus(false);
                        panel.selected = None;
                        self.ctx.mark_dirty();
                    }
                },
                hit => {
                    match hit {
                        PanelHit::ViewFiles => {
                            self.ctx.display().toggle_side_panel(PanelView::Files)
                        },
                        PanelHit::ViewGit => self.ctx.display().toggle_side_panel(PanelView::Git),
                        PanelHit::Search => {
                            let files =
                                self.ctx.display().nebula_side_panel.view == PanelView::Files;
                            if files {
                                // The Files view's filter box takes focus.
                                self.ctx.display().nebula_side_panel.search_focus = true;
                            } else {
                                // Git view: that strip is the 暂存/提交/推送
                                // button row (or the commit-message input,
                                // which the keyboard owns — clicks are inert).
                                if !self.ctx.display().nebula_side_panel.commit_focus {
                                    let (sx, _, sw, _) = layout.search;
                                    let gap = 8.0 * self.ctx.window().scale_factor as f32;
                                    let rects =
                                        crate::display::side_panel::git_button_rects(sx, sw, gap);
                                    let panel = &mut self.ctx.display().nebula_side_panel;
                                    if x < rects[0].0 + rects[0].1 {
                                        panel.git_stage_all();
                                    } else if x < rects[1].0 + rects[1].1 {
                                        panel.git_begin_commit();
                                    } else {
                                        panel.git_push();
                                    }
                                }
                            }
                        },
                        PanelHit::Row(row) => {
                            self.ctx.display().nebula_side_panel.search_unfocus(false);
                            let info = self
                                .ctx
                                .display()
                                .nebula_side_panel
                                .visible_row(row)
                                .map(|r| (r.path.clone(), r.is_dir));
                            match info {
                                // Directories expand/collapse on click.
                                Some((_, true)) | None => {
                                    self.ctx.display().nebula_side_panel.click_row(row);
                                },
                                // Files: double-click opens with the system
                                // handler; a single press arms a drag toward
                                // the terminal (drop pastes the path).
                                Some((path, false)) => {
                                    use crate::display::side_panel::FileDrag;
                                    let now = std::time::Instant::now();
                                    let dbl = {
                                        let panel = &mut self.ctx.display().nebula_side_panel;
                                        // Click = persistent selection (until
                                        // clicking off the panel / closing it).
                                        panel.selected = Some(path.clone());
                                        let dbl =
                                            panel.last_file_click.as_ref().is_some_and(|(p, t)| {
                                                *p == path
                                                    && t.elapsed()
                                                        < std::time::Duration::from_millis(400)
                                            });
                                        if dbl {
                                            panel.last_file_click = None;
                                            panel.drag_file = None;
                                        } else {
                                            panel.last_file_click = Some((path.clone(), now));
                                            let name = path
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_default();
                                            panel.drag_file = Some(FileDrag {
                                                path: path.clone(),
                                                name,
                                                origin: (x, y),
                                                pos: (x, y),
                                                active: false,
                                            });
                                        }
                                        dbl
                                    };
                                    if dbl {
                                        // Readable text files open in an
                                        // in-app viewer tab; everything else
                                        // goes to the system handler.
                                        if crate::display::markdown_view::viewable_file(&path) {
                                            self.ctx.nebula_tab(crate::event::TabRequest::OpenDoc(
                                                path,
                                            ));
                                        } else {
                                            self.ctx.open_path(&path);
                                        }
                                    }
                                },
                            }
                        },
                        _ => {
                            self.ctx.display().nebula_side_panel.search_unfocus(false);
                        },
                    }
                    self.ctx.mark_dirty();
                    return;
                },
            }
        }

        // Right-clicking the sidebar "+" opens the quick-launch profile menu
        // (Windows Terminal's profile dropdown); left-click keeps opening the
        // default shell. Right-clicking a tab or SSH host row pins it to the
        // top of its section (a second right-click on a host unpins).
        if button == MouseButton::Right {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            match self.ctx.display().chrome_hit(x, y) {
                crate::display::ChromeHit::NewTab => {
                    let profiles: Vec<String> =
                        self.ctx.config().profiles.iter().map(|p| p.name.clone()).collect();
                    // Detected shells fill the menu even with no config
                    // profiles, so always open it.
                    self.ctx.display().open_shell_menu(&profiles);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::Tab(index) => {
                    if index > 0 {
                        self.ctx.nebula_tab(crate::event::TabRequest::Move { from: index, to: 0 });
                    }
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::Host(index) => {
                    self.ctx.display().pin_host(index);
                    self.ctx.mark_dirty();
                    return;
                },
                _ => {},
            }
        }

        // Nebula chrome: intercept clicks on the custom title bar and window
        // controls before any terminal handling.
        if button == MouseButton::Left {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            // Chrome geometry is window-relative; the pane view would misplace
            // every hit rect in split mode (unclickable gear, wrong tabs).
            let size = self.ctx.display().size_info;
            let scale = self.ctx.window().scale_factor as f32;
            // Window border resize takes priority over the chrome controls.
            if let Some(dir) = crate::display::resize_edge(&size, scale, x, y) {
                self.ctx.window().drag_resize(dir);
                return;
            }
            // The confirm modal owns the pointer while it shows: its two
            // buttons dispatch, any other click is swallowed (modal
            // semantics — nothing may reach the UI behind the veil).
            if let Some(confirm) = self.ctx.display().nebula_confirm.clone() {
                if let Some((primary, cancel)) = self.ctx.display().nebula_confirm_buttons {
                    let hit = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
                        x >= rx && x < rx + rw && y >= ry && y < ry + rh
                    };
                    if hit(primary) {
                        self.nebula_confirm_accept(confirm);
                    } else if hit(cancel) {
                        self.ctx.display().nebula_confirm = None;
                    }
                }
                self.ctx.mark_dirty();
                return;
            }
            let settings_open = self.ctx.display().settings_open();
            let settings_section = self.ctx.display().settings_section();
            let settings_scroll = self.ctx.display().settings_scroll();
            let shell_picker_open = self.ctx.display().nebula_shell_picker_open;
            let shell_picker_count = self.ctx.display().shell_picker_count();
            match crate::display::settings_hit(
                &size,
                scale,
                x,
                y,
                settings_open,
                settings_section,
                settings_scroll,
                shell_picker_open,
                shell_picker_count,
            ) {
                crate::display::SettingsHit::Toggle => {
                    self.ctx.display().toggle_settings();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::Nav(section) => {
                    self.ctx.display().select_settings_section(section);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::Theme(theme) => {
                    self.ctx.display().select_nebula_theme(theme);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::GhostToggle => {
                    self.ctx.display().toggle_ghost();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::AcceptCycle => {
                    self.ctx.display().cycle_accept();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::ShellCycle => {
                    // Toggle inline shell picker (expand/collapse the list).
                    self.ctx.display().toggle_shell_picker();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::ShellPickerRow(index) => {
                    self.ctx.display().set_default_shell_by_index(index);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::FetchToggle => {
                    self.ctx.display().toggle_fetch();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::PowerlineToggle => {
                    self.ctx.display().toggle_powerline();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::KeepSessionToggle => {
                    self.ctx.display().toggle_keep_session();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::OpacityDown => {
                    self.ctx.display().adjust_window_opacity(-0.05);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::OpacityUp => {
                    self.ctx.display().adjust_window_opacity(0.05);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::BackgroundColor => {
                    self.ctx.display().cycle_background_color();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::BackgroundImage => {
                    self.ctx.display().pick_background_image();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::OpenConfigFile => {
                    self.ctx.display().open_settings_file();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::Reset => {
                    self.ctx.display().reset_appearance_settings();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::Panel => return,
                crate::display::SettingsHit::Dismiss => {
                    self.ctx.display().dismiss_settings();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::SettingsHit::None => {},
            }
            let chrome_hit = self.ctx.display().chrome_hit(x, y);
            // Multi-click state was advanced once at the top of this
            // function; read it for the tab double-click rename below.
            let state = self.ctx.mouse().click_state;
            match chrome_hit {
                crate::display::ChromeHit::NewTab => {
                    self.ctx.nebula_tab(crate::event::TabRequest::New);
                    return;
                },
                crate::display::ChromeHit::NewTabMenu => {
                    // The chevron opens the shell dropdown (detected shells +
                    // config profiles), like Windows Terminal's profile menu.
                    let profiles: Vec<String> =
                        self.ctx.config().profiles.iter().map(|p| p.name.clone()).collect();
                    self.ctx.display().open_shell_menu(&profiles);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::AddSshHost => {
                    self.ctx.display().open_ssh_editor();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::HostDelete(index) => {
                    self.ctx.display().delete_ssh_host(index);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::TabClose(index) => {
                    self.ctx.nebula_tab(crate::event::TabRequest::CloseIndex(index));
                    return;
                },
                crate::display::ChromeHit::Tab(index) => {
                    // Clicking inside the rename box places the caret there
                    // (real text-field behaviour) instead of starting a drag.
                    if self
                        .ctx
                        .display()
                        .nebula_tab_rename
                        .as_ref()
                        .is_some_and(|(i, _)| *i == index)
                    {
                        self.ctx.display().tab_rename_click(x);
                        self.ctx.mark_dirty();
                        return;
                    }
                    // Double-click a tab to start renaming (Windows Terminal style).
                    if state == ClickState::DoubleClick {
                        self.ctx.nebula_tab(crate::event::TabRequest::BeginRename(index));
                        return;
                    }
                    // Selection is deferred to release (a plain click becomes
                    // TabDropAction::Click): the terminal area must keep
                    // showing the ACTIVE tab while another tab is dragged over
                    // it toward a dock zone.
                    self.ctx.display().arm_tab_drag(index, x, y);
                    return;
                },
                crate::display::ChromeHit::SidebarToggle => {
                    self.ctx.display().toggle_sidebar();
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::Host(index) => {
                    // Open a new tab connected to this ~/.ssh/config host.
                    let host = self.ctx.display().nebula_ssh_hosts.get(index).cloned();
                    if let Some(host) = host {
                        self.ctx.nebula_tab(crate::event::TabRequest::NewSsh(host));
                    }
                    return;
                },
                crate::display::ChromeHit::TabsSection => {
                    self.ctx.display().toggle_sidebar_section(false);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::HostsSection => {
                    self.ctx.display().toggle_sidebar_section(true);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::PanelFiles => {
                    self.ctx
                        .display()
                        .toggle_side_panel(crate::display::side_panel::PanelView::Files);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::PanelGit => {
                    self.ctx
                        .display()
                        .toggle_side_panel(crate::display::side_panel::PanelView::Git);
                    self.ctx.mark_dirty();
                    return;
                },
                crate::display::ChromeHit::Close => {
                    self.ctx.nebula_tab(crate::event::TabRequest::CloseWindow);
                    return;
                },
                crate::display::ChromeHit::Minimize => {
                    self.ctx.window().set_minimized(true);
                    return;
                },
                crate::display::ChromeHit::Maximize => {
                    self.ctx.window().toggle_maximized();
                    return;
                },
                crate::display::ChromeHit::TitleBar => {
                    self.ctx.window().drag_window();
                    return;
                },
                crate::display::ChromeHit::None => {},
            }

            // Nebula: grab the scrollback thumb (or jump on a track press).
            // Only live while scrolled into history, since the bar auto-hides.
            let view = self.ctx.size_info();
            let display_offset = self.ctx.terminal().grid().display_offset();
            let total_lines = self.ctx.terminal().total_lines();
            if let Some(grab) =
                self.ctx.display().scrollbar_grab(&view, display_offset, total_lines, x, y)
            {
                self.ctx.display().nebula_scrollbar_drag = Some(grab);
                let target =
                    self.ctx.display().scrollbar_target_offset(&view, total_lines, y, grab);
                let delta = target as i32 - display_offset as i32;
                if delta != 0 {
                    self.ctx.scroll(Scroll::Delta(delta));
                }
                self.ctx.mark_dirty();
                return;
            }
        }

        // Nebula: right-click copies the selection, or pastes when there is
        // none (Windows Terminal-style), unless the app is in mouse mode.
        if button == MouseButton::Right
            && !self.ctx.modifiers().state().shift_key()
            && !self.ctx.mouse_mode()
        {
            if self.ctx.selection_is_empty() {
                let text = self.ctx.clipboard_mut().load(ClipboardType::Clipboard);
                self.ctx.paste(&text, true);
            } else {
                self.ctx.copy_selection(ClipboardType::Clipboard);
                self.ctx.clear_selection();
            }
            return;
        }

        // Handle mouse mode.
        if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            self.ctx.mouse_mut().click_state = ClickState::None;

            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons..
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };

            self.mouse_report(code, ElementState::Pressed);
        } else {
            // Multi-click state was advanced once at the top of this function.
            // Load mouse point, treating message bar and padding as the closest cell.
            let display_offset = self.ctx.terminal().grid().display_offset();
            let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

            if let MouseButton::Left = button {
                self.on_left_click(point)
            }
        }
    }

    /// Handle left click selection and vi mode cursor movement.
    fn on_left_click(&mut self, point: Point) {
        let side = self.ctx.mouse().cell_side;
        let control = self.ctx.modifiers().state().control_key();

        match self.ctx.mouse().click_state {
            ClickState::Click => {
                let had_selection = !self.ctx.selection_is_empty();
                // Shift+click extends the existing selection to the clicked
                // cell (Windows Terminal / native text field behaviour)
                // instead of clearing it.
                if self.ctx.modifiers().state().shift_key() && had_selection {
                    self.ctx.update_selection(point, side);
                    return;
                }
                let inside_text_area =
                    self.ctx.size_info().contains_point(self.ctx.mouse().x, self.ctx.mouse().y);
                let mut hint_hit = false;
                if inside_text_area {
                    let mods = self.ctx.modifiers().state();
                    // Query the hint with the SAME viewport the hover path uses
                    // (`pane_view`), not `ctx.size_info()`: the two disagree by
                    // the chrome offset, so the press used to look up a hint on
                    // the wrong row and miss links the hover clearly marked.
                    let hint_point = {
                        let view = self.ctx.display().pane_view();
                        let display_offset = self.ctx.terminal().grid().display_offset();
                        self.ctx.mouse().point(&view, display_offset)
                    };
                    if let Some(hint) = crate::display::hint::highlighted_at(
                        self.ctx.terminal(),
                        self.ctx.config(),
                        hint_point,
                        mods,
                    ) {
                        hint_hit = true;
                        self.ctx.display().highlighted_hint = Some(hint);
                        self.ctx.mouse_mut().block_hint_launcher = false;
                        self.ctx.mark_dirty();
                    }
                }
                crate::display::nebula_link_log(format!(
                    "link_press point={point:?} xy=({:.0},{:.0}) had_sel={had_selection} \
                     ctrl={control} inside={inside_text_area} hint_hit={hint_hit}",
                    self.ctx.mouse().x,
                    self.ctx.mouse().y,
                ));

                // Windows Terminal model: a single click never CREATES a
                // selection — it only clears an existing one. The would-be
                // selection is merely armed here; `mouse_moved` starts it for
                // real once the pointer travels past the drag threshold.
                //
                // Don't launch URLs if this click cleared a selection.
                self.ctx.mouse_mut().block_hint_launcher = had_selection && !hint_hit;
                if had_selection {
                    self.ctx.clear_selection();
                }

                // Ctrl+click on a highlighted link is a link-open gesture, not
                // the start of a block selection: hint hit-testing outranks
                // selection arming (WT parity). A plain click over a link
                // still arms — dragging across a URL must select its text.
                if !(control && hint_hit) {
                    let ty = if control { SelectionType::Block } else { SelectionType::Simple };
                    self.ctx.mouse_mut().pending_selection = Some((ty, point, side));
                }
            },
            ClickState::DoubleClick if !control => {
                // Double-click selects the word under the pointer — but on an
                // EMPTY cell there is no word, and semantically selecting the
                // blank used to paint a stray one-cell block that read as "a
                // click leaves a cursor behind" (WT selects nothing there).
                let cell_char = self.ctx.terminal().grid()[point].c;
                if cell_char != ' ' && cell_char != '\t' && cell_char != '\0' {
                    self.ctx.mouse_mut().block_hint_launcher = true;
                    self.ctx.start_selection(SelectionType::Semantic, point, side);
                }
            },
            ClickState::TripleClick if !control => {
                self.ctx.mouse_mut().block_hint_launcher = true;
                self.ctx.start_selection(SelectionType::Lines, point, side);
            },
            _ => (),
        };

        // Move vi mode cursor to mouse click position.
        if self.ctx.terminal().mode().contains(TermMode::VI) && !self.ctx.search_active() {
            self.ctx.terminal_mut().vi_mode_cursor.point = point;
            self.ctx.mark_dirty();
        }
    }

    fn on_mouse_release(&mut self, button: MouseButton) {
        // Nebula: finish an in-progress tab-bar reorder drag first, so it works
        // even while a TUI has grabbed the mouse. A plain click (never dragged)
        // returns `None` here and falls through to normal release handling.
        if button == MouseButton::Left {
            // Drop a dragged tree file: released over the terminal (anywhere
            // off the drawer) pastes its full path, quoted when needed —
            // Explorer-onto-terminal semantics.
            if let Some(drag) = self.ctx.display().nebula_side_panel.drag_file.take() {
                if drag.active {
                    let x = self.ctx.mouse().x as f32;
                    let y = self.ctx.mouse().y as f32;
                    let layout = self.ctx.display().side_panel_layout();
                    if crate::display::side_panel::panel_hit(&layout, x, y)
                        == crate::display::side_panel::PanelHit::None
                    {
                        let mut text = drag.path.display().to_string();
                        if text.contains(' ') {
                            text = format!("\"{text}\"");
                        }
                        text.push(' ');
                        self.ctx.write_to_pty(text.into_bytes());
                    }
                    self.ctx.mark_dirty();
                    return;
                }
            }
            // Let go of the scrollback thumb.
            if self.ctx.display().nebula_scrollbar_drag.take().is_some() {
                self.ctx.mark_dirty();
                return;
            }
            if let Some(action) = self.ctx.display().end_tab_drag() {
                use crate::display::TabDropAction;
                match action {
                    TabDropAction::Click(index) => {
                        self.ctx.nebula_tab(crate::event::TabRequest::Select(index));
                    },
                    TabDropAction::Reorder { from, to } => {
                        self.ctx.nebula_tab(crate::event::TabRequest::Move { from, to });
                    },
                    TabDropAction::Dock { source, nav } => {
                        self.ctx.nebula_tab(crate::event::TabRequest::DockSplit { source, nav });
                    },
                }
                self.ctx.mark_dirty();
                return;
            }
        }

        if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Can't properly report more than three buttons.
                MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => return,
            };
            self.mouse_report(code, ElementState::Released);
            return;
        }

        // Trigger hints highlighted by the mouse.
        let hint = self.ctx.display().highlighted_hint.take();
        crate::display::nebula_link_log(format!(
            "link_release button={button:?} hint={} block={} sel_empty={}",
            hint.is_some(),
            self.ctx.mouse().block_hint_launcher,
            self.ctx.selection_is_empty()
        ));
        if let Some(hint) = hint.as_ref().filter(|_| button == MouseButton::Left) {
            // The hover highlight is the ground truth the user sees. If this
            // click produced no real selection (no drag, no double-click), a
            // Ctrl+click on a highlighted link opens it — even when the
            // press-side lookup missed, and even though any mouse motion sets
            // `block_hint_launcher` (that flag exists to stop launches after
            // drag-selections, not after ordinary pointer travel).
            //
            // Requiring Ctrl (matching the "Ctrl+点击 打开" hover hint) keeps a
            // plain click free for text selection — a bare click on a link no
            // longer fires the browser/opener by accident.
            let ctrl = self.ctx.modifiers().state().control_key();
            if ctrl && self.ctx.selection_is_empty() {
                self.ctx.mouse_mut().block_hint_launcher = false;
                self.ctx.trigger_hint(hint);
            }
        }
        self.ctx.display().highlighted_hint = hint;

        let timer_id = TimerId::new(Topic::SelectionScrolling, self.ctx.window().id());
        self.ctx.scheduler_mut().unschedule(timer_id);

        if let MouseButton::Left | MouseButton::Right = button {
            // Copy selection on release, to prevent flooding the display server.
            self.ctx.copy_selection(ClipboardType::Selection);
        }
    }

    pub fn mouse_wheel_input(&mut self, delta: MouseScrollDelta, phase: TouchPhase) {
        // The settings modal captures the wheel: scroll its content, not the
        // terminal hiding behind the veil.
        if self.ctx.display().settings_open() {
            let px = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    lines * 3.0 * self.ctx.size_info().cell_height()
                },
                MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
            };
            self.ctx.display().settings_scroll_by(-px);
            return;
        }

        let multiplier = self.ctx.config().scrolling.multiplier;

        // The right-side drawer captures the wheel while the pointer hovers it.
        if self.ctx.display().nebula_side_panel.open {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let layout = self.ctx.display().side_panel_layout();
            if crate::display::side_panel::panel_hit(&layout, x, y)
                != crate::display::side_panel::PanelHit::None
            {
                let rows = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines as i32 * 3,
                    MouseScrollDelta::PixelDelta(pos) => {
                        (-pos.y as f32 / layout.row_h.max(1.0)).round() as i32
                    },
                };
                if rows != 0 {
                    self.ctx.display().nebula_side_panel.scroll_by(rows, layout.max_rows);
                    self.ctx.mark_dirty();
                }
                return;
            }
        }

        // The left sidebar's sections capture the wheel while hovered: each
        // (TABS / SSH HOSTS) scrolls independently in whole rows.
        {
            let x = self.ctx.mouse().x as f32;
            let y = self.ctx.mouse().y as f32;
            let rows = match delta {
                MouseScrollDelta::LineDelta(_, lines) => -lines.signum() as i32,
                MouseScrollDelta::PixelDelta(pos) => -(pos.y.signum() as i32),
            };
            if rows != 0 && self.ctx.display().sidebar_wheel(x, y, rows) {
                self.ctx.mark_dirty();
                return;
            }
        }

        // A document-viewer tab owns the remaining wheel: pixel-scroll the
        // document (no grid behind it to scroll).
        if self.ctx.doc_view().is_some() {
            let px = match delta {
                MouseScrollDelta::LineDelta(_, lines) => {
                    -lines * 3.0 * self.ctx.size_info().cell_height()
                },
                MouseScrollDelta::PixelDelta(pos) => -pos.y as f32,
            };
            let viewport_h = self.ctx.display().terminal_card_rect().3;
            if let Some(doc) = self.ctx.doc_view() {
                doc.scroll_by(px * multiplier as f32, viewport_h);
            }
            self.ctx.mark_dirty();
            return;
        }

        match delta {
            MouseScrollDelta::LineDelta(columns, lines) => {
                let new_scroll_px_x = columns * self.ctx.size_info().cell_width();
                let new_scroll_px_y = lines * self.ctx.size_info().cell_height();
                self.scroll_terminal(
                    new_scroll_px_x as f64,
                    new_scroll_px_y as f64,
                    multiplier as f64,
                );
            },
            MouseScrollDelta::PixelDelta(mut lpos) => {
                match phase {
                    TouchPhase::Started => {
                        // Reset offset to zero.
                        self.ctx.mouse_mut().accumulated_scroll = Default::default();
                    },
                    TouchPhase::Moved => {
                        // When the angle between (x, 0) and (x, y) is lower than ~25 degrees
                        // (cosine is larger that 0.9) we consider this scrolling as horizontal.
                        if lpos.x.abs() / lpos.x.hypot(lpos.y) > 0.9 {
                            lpos.y = 0.;
                        } else {
                            lpos.x = 0.;
                        }

                        self.scroll_terminal(lpos.x, lpos.y, multiplier as f64);
                    },
                    _ => (),
                }
            },
        }
    }

    fn scroll_terminal(&mut self, new_scroll_x_px: f64, new_scroll_y_px: f64, multiplier: f64) {
        const MOUSE_WHEEL_UP: u8 = 64;
        const MOUSE_WHEEL_DOWN: u8 = 65;
        const MOUSE_WHEEL_LEFT: u8 = 66;
        const MOUSE_WHEEL_RIGHT: u8 = 67;

        let width = f64::from(self.ctx.size_info().cell_width());
        let height = f64::from(self.ctx.size_info().cell_height());

        let multiplier = if self.ctx.mouse_mode() { 1. } else { multiplier };

        self.ctx.mouse_mut().accumulated_scroll.x += new_scroll_x_px * multiplier;
        self.ctx.mouse_mut().accumulated_scroll.y += new_scroll_y_px * multiplier;

        let lines = (self.ctx.mouse().accumulated_scroll.y / height).abs() as usize;
        let columns = (self.ctx.mouse().accumulated_scroll.x / width).abs() as usize;

        let is_scroll_up = new_scroll_y_px > 0.;
        let event = if is_scroll_up { MouseEvent::WheelUp } else { MouseEvent::WheelDown };

        if lines != 0 && self.process_mouse_bindings(event) {
            // Repeat for remaining number of lines.
            for _ in 1..lines {
                self.process_mouse_bindings(event);
            }
        } else if self.ctx.mouse_mode() {
            let code = if is_scroll_up { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_DOWN };
            for _ in 0..lines {
                self.mouse_report(code, ElementState::Pressed);
            }

            let code = if new_scroll_x_px > 0. { MOUSE_WHEEL_LEFT } else { MOUSE_WHEEL_RIGHT };
            for _ in 0..columns {
                self.mouse_report(code, ElementState::Pressed);
            }
        } else if self
            .ctx
            .terminal()
            .mode()
            .contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !self.ctx.modifiers().state().shift_key()
        {
            // The chars here are the same as for the respective arrow keys.
            let line_cmd = if is_scroll_up { b'A' } else { b'B' };
            let column_cmd = if new_scroll_x_px > 0. { b'D' } else { b'C' };

            let mut content = Vec::with_capacity(3 * (lines + columns));

            for _ in 0..lines {
                content.push(0x1b);
                content.push(b'O');
                content.push(line_cmd);
            }

            for _ in 0..columns {
                content.push(0x1b);
                content.push(b'O');
                content.push(column_cmd);
            }

            self.ctx.write_to_pty(content);
        } else if lines != 0 {
            let lines = if is_scroll_up { lines as i32 } else { -(lines as i32) };
            self.ctx.scroll(Scroll::Delta(lines));
        }

        self.ctx.mouse_mut().accumulated_scroll.x %= width;
        self.ctx.mouse_mut().accumulated_scroll.y %= height;
    }

    pub fn on_focus_change(&mut self, is_focused: bool) {
        if self.ctx.terminal().mode().contains(TermMode::FOCUS_IN_OUT) {
            let chr = if is_focused { "I" } else { "O" };

            let msg = format!("\x1b[{chr}");
            self.ctx.write_to_pty(msg.into_bytes());
        }
    }

    /// Handle touch input.
    pub fn touch(&mut self, touch: TouchEvent) {
        match touch.phase {
            TouchPhase::Started => self.on_touch_start(touch),
            TouchPhase::Moved => self.on_touch_motion(touch),
            TouchPhase::Ended | TouchPhase::Cancelled => self.on_touch_end(touch),
        }
    }

    /// Handle beginning of touch input.
    pub fn on_touch_start(&mut self, touch: TouchEvent) {
        // Inhibit IME on touch while not focused, forcing a touch tap while focused to enable IME.
        if !self.ctx.terminal().is_focused {
            self.ctx.window().set_ime_inhibitor(ImeInhibitor::TOUCH, true);
        }

        let touch_purpose = self.ctx.touch_purpose();
        *touch_purpose = match mem::take(touch_purpose) {
            TouchPurpose::None => TouchPurpose::Tap(touch),
            TouchPurpose::Tap(start) => TouchPurpose::Zoom(TouchZoom::new((start, touch))),
            TouchPurpose::ZoomPendingSlot(slot) => {
                TouchPurpose::Zoom(TouchZoom::new((slot, touch)))
            },
            TouchPurpose::Zoom(zoom) => {
                let slots = zoom.slots();
                let mut set = HashSet::default();
                set.insert(slots.0.id);
                set.insert(slots.1.id);
                TouchPurpose::Invalid(set)
            },
            TouchPurpose::Scroll(event) | TouchPurpose::Select(event) => {
                let mut set = HashSet::default();
                set.insert(event.id);
                TouchPurpose::Invalid(set)
            },
            TouchPurpose::Invalid(mut slots) => {
                slots.insert(touch.id);
                TouchPurpose::Invalid(slots)
            },
        };
    }

    /// Handle touch input movement.
    pub fn on_touch_motion(&mut self, touch: TouchEvent) {
        let touch_purpose = self.ctx.touch_purpose();
        match touch_purpose {
            TouchPurpose::None => (),
            // Handle transition from tap to scroll/select.
            TouchPurpose::Tap(start) => {
                let delta_x = touch.location.x - start.location.x;
                let delta_y = touch.location.y - start.location.y;
                if delta_x.abs() > MAX_TAP_DISTANCE {
                    // Update gesture state.
                    let start_location = start.location;
                    *touch_purpose = TouchPurpose::Select(*start);

                    // Start simulated mouse input.
                    self.mouse_moved(start_location);
                    self.mouse_input(ElementState::Pressed, MouseButton::Left);

                    // Apply motion since touch start.
                    self.on_touch_motion(touch);
                } else if delta_y.abs() > MAX_TAP_DISTANCE {
                    // Update gesture state.
                    *touch_purpose = TouchPurpose::Scroll(*start);

                    // Apply motion since touch start.
                    self.on_touch_motion(touch);
                }
            },
            TouchPurpose::Zoom(zoom) => {
                let font_delta = zoom.font_delta(touch);
                self.ctx.change_font_size(font_delta);
            },
            TouchPurpose::Scroll(last_touch) => {
                // Calculate delta and update last touch position.
                let delta_y = touch.location.y - last_touch.location.y;
                *touch_purpose = TouchPurpose::Scroll(touch);

                // Use a fixed scroll factor for touchscreens, to accurately track finger motion.
                self.scroll_terminal(0., delta_y, 1.0);
            },
            TouchPurpose::Select(_) => self.mouse_moved(touch.location),
            TouchPurpose::ZoomPendingSlot(_) | TouchPurpose::Invalid(_) => (),
        }
    }

    /// Handle end of touch input.
    pub fn on_touch_end(&mut self, touch: TouchEvent) {
        // Finalize the touch motion up to the release point.
        self.on_touch_motion(touch);

        let touch_purpose = self.ctx.touch_purpose();
        match touch_purpose {
            // Simulate LMB clicks.
            TouchPurpose::Tap(start) => {
                let start_location = start.location;
                *touch_purpose = Default::default();

                self.mouse_moved(start_location);
                self.mouse_input(ElementState::Pressed, MouseButton::Left);
                self.mouse_input(ElementState::Released, MouseButton::Left);

                self.ctx.window().set_ime_inhibitor(ImeInhibitor::TOUCH, false);
            },
            // Transition zoom to pending state once a finger was released.
            TouchPurpose::Zoom(zoom) => {
                let slots = zoom.slots();
                let remaining = if slots.0.id == touch.id { slots.1 } else { slots.0 };
                *touch_purpose = TouchPurpose::ZoomPendingSlot(remaining);
            },
            TouchPurpose::ZoomPendingSlot(_) => *touch_purpose = Default::default(),
            // Reset touch state once all slots were released.
            TouchPurpose::Invalid(slots) => {
                slots.remove(&touch.id);
                if slots.is_empty() {
                    *touch_purpose = Default::default();
                }
            },
            // Release simulated LMB.
            TouchPurpose::Select(_) => {
                *touch_purpose = Default::default();
                self.mouse_input(ElementState::Released, MouseButton::Left);
            },
            // Reset touch state on scroll finish.
            TouchPurpose::Scroll(_) => *touch_purpose = Default::default(),
            TouchPurpose::None => (),
        }
    }

    /// Reset mouse cursor based on modifier and terminal state.
    #[inline]
    pub fn reset_mouse_cursor(&mut self) {
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    /// Modifier state change.
    pub fn modifiers_input(&mut self, modifiers: Modifiers) {
        *self.ctx.modifiers() = modifiers;

        // Prompt hint highlight update.
        self.ctx.mouse_mut().hint_highlight_dirty = true;

        // Update mouse state and check for URL change.
        let mouse_state = self.cursor_state();
        self.ctx.window().set_mouse_cursor(mouse_state);
    }

    pub fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        match button {
            MouseButton::Left => self.ctx.mouse_mut().left_button_state = state,
            MouseButton::Middle => self.ctx.mouse_mut().middle_button_state = state,
            MouseButton::Right => self.ctx.mouse_mut().right_button_state = state,
            _ => (),
        }

        // Drag-threshold bookkeeping (Windows SM_CXDRAG-style): remember
        // where the left press landed; `mouse_moved` starts a drag-selection
        // only once the pointer travels past the threshold, so a plain click
        // never leaves a stray one-cell selection that eats link clicks.
        if button == MouseButton::Left {
            let origin = (self.ctx.mouse().x, self.ctx.mouse().y);
            let mouse = self.ctx.mouse_mut();
            match state {
                ElementState::Pressed => {
                    mouse.drag_origin = Some(origin);
                    mouse.drag_active = false;
                    mouse.pending_selection = None;
                },
                ElementState::Released => {
                    mouse.drag_origin = None;
                    mouse.drag_active = false;
                    mouse.pending_selection = None;
                },
            }
        }

        // Skip normal mouse events if the message bar has been clicked.
        if self.message_bar_cursor_state() == Some(CursorIcon::Pointer)
            && state == ElementState::Pressed
        {
            let size = self.ctx.size_info();

            let current_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            self.ctx.clear_selection();
            self.ctx.pop_message();

            // Reset cursor when message bar height changed or all messages are gone.
            let new_lines = self.ctx.message().map_or(0, |m| m.text(&size).len());

            let new_icon = match current_lines.cmp(&new_lines) {
                Ordering::Less => CursorIcon::Default,
                Ordering::Equal => CursorIcon::Pointer,
                Ordering::Greater => {
                    if self.ctx.mouse_mode() {
                        CursorIcon::Default
                    } else {
                        // Nebula: normal arrow over the terminal area.
                        CursorIcon::Default
                    }
                },
            };

            self.ctx.window().set_mouse_cursor(new_icon);
        } else {
            match state {
                ElementState::Pressed => {
                    // Process mouse press before bindings to update the `click_state`.
                    self.on_mouse_press(button);
                    self.process_mouse_bindings(MouseEvent::Button(button));
                },
                ElementState::Released => self.on_mouse_release(button),
            }
        }
    }

    /// Attempt to find a binding and execute its action.
    ///
    /// The provided mode, mods, and key must match what is allowed by a binding
    /// for its action to be executed.
    fn process_mouse_bindings(&mut self, event: MouseEvent) -> bool {
        let mode = BindingMode::new(self.ctx.terminal().mode(), self.ctx.search_active());
        let mouse_mode = self.ctx.mouse_mode();
        let mods = self.ctx.modifiers().state();
        let mouse_bindings = self.ctx.config().mouse_bindings().to_owned();

        // If mouse mode is active, also look for bindings without shift.
        let fallback_allowed = mouse_mode && mods.contains(ModifiersState::SHIFT);
        let mut match_found: bool = false;

        for binding in &mouse_bindings {
            // Don't trigger normal bindings in mouse mode unless Shift is pressed.
            if binding.is_triggered_by(mode, mods, &event) && (fallback_allowed || !mouse_mode) {
                binding.action.execute(&mut self.ctx);
                match_found = true;
            }
        }

        if fallback_allowed && !match_found {
            let fallback_mods = mods & !ModifiersState::SHIFT;
            for binding in &mouse_bindings {
                if binding.is_triggered_by(mode, fallback_mods, &event) {
                    binding.action.execute(&mut self.ctx);
                    match_found = true;
                }
            }
        }

        match_found
    }

    /// Check mouse icon state in relation to the message bar.
    fn message_bar_cursor_state(&self) -> Option<CursorIcon> {
        // Since search is above the message bar, the button is offset by search's height.
        let search_height = usize::from(self.ctx.search_active());

        // Calculate Y position of the end of the last terminal line.
        let size = self.ctx.size_info();
        let terminal_end = size.padding_y() as usize
            + size.cell_height() as usize * (size.screen_lines() + search_height);

        let mouse = self.ctx.mouse();
        let display_offset = self.ctx.terminal().grid().display_offset();
        let point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);

        if self.ctx.message().is_none() || (mouse.y <= terminal_end) {
            None
        } else if mouse.y <= terminal_end + size.cell_height() as usize
            && point.column + message_bar::CLOSE_BUTTON_TEXT.len() >= size.columns()
        {
            Some(CursorIcon::Pointer)
        } else {
            Some(CursorIcon::Default)
        }
    }

    /// Icon state of the cursor.
    fn cursor_state(&mut self) -> CursorIcon {
        let display_offset = self.ctx.terminal().grid().display_offset();
        let mut point = self.ctx.mouse().point(&self.ctx.size_info(), display_offset);
        // `point` is clamped to `size_info`, but we're about to index the grid,
        // whose column/line count can trail `size_info` by one during a resize
        // or sidebar toggle (asymmetric-padding reflow lands a frame later).
        // Clamp to the grid's real bounds so indexing can never panic.
        {
            let grid = self.ctx.terminal().grid();
            let last_col = grid.columns().saturating_sub(1);
            let last_line = grid.screen_lines().saturating_sub(1) as i32;
            if point.column.0 > last_col {
                point.column = Column(last_col);
            }
            if point.line.0 > last_line {
                point.line = Line(last_line);
            }
        }
        let hyperlink = self.ctx.terminal().grid()[point].hyperlink();

        // Function to check if mouse is on top of a hint.
        let hint_highlighted = |hint: &HintMatch| hint.should_highlight(point, hyperlink.as_ref());

        if let Some(mouse_state) = self.message_bar_cursor_state() {
            mouse_state
        } else if self.ctx.display().highlighted_hint.as_ref().is_some_and(hint_highlighted) {
            CursorIcon::Pointer
        } else if !self.ctx.modifiers().state().shift_key() && self.ctx.mouse_mode() {
            CursorIcon::Default
        } else {
            // Nebula: keep the normal arrow over the terminal area (no I-beam).
            CursorIcon::Default
        }
    }

    /// Handle automatic scrolling when selecting above/below the window.
    fn update_selection_scrolling(&mut self, mouse_y: i32) {
        let scale_factor = self.ctx.window().scale_factor;
        let size = self.ctx.size_info();
        let window_id = self.ctx.window().id();
        let scheduler = self.ctx.scheduler_mut();

        // Scale constants by DPI.
        let min_height = (MIN_SELECTION_SCROLLING_HEIGHT * scale_factor) as i32;
        let step = (SELECTION_SCROLLING_STEP * scale_factor) as i32;

        // Compute the height of the scrolling areas.
        let end_top = max(min_height, size.padding_y() as i32);
        let text_area_bottom = size.padding_y() + size.screen_lines() as f32 * size.cell_height();
        let start_bottom = min(size.height() as i32 - min_height, text_area_bottom as i32);

        // Get distance from closest window boundary.
        let delta = if mouse_y < end_top {
            end_top - mouse_y + step
        } else if mouse_y >= start_bottom {
            start_bottom - mouse_y - step
        } else {
            scheduler.unschedule(TimerId::new(Topic::SelectionScrolling, window_id));
            return;
        };

        // Scale number of lines scrolled based on distance to boundary.
        let event = Event::new(EventType::Scroll(Scroll::Delta(delta / step)), Some(window_id));

        // Schedule event.
        let timer_id = TimerId::new(Topic::SelectionScrolling, window_id);
        scheduler.unschedule(timer_id);
        scheduler.schedule(event, SELECTION_SCROLLING_INTERVAL, true, timer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use winit::event::{DeviceId, Event as WinitEvent, WindowEvent};
    use winit::keyboard::Key;
    use winit::window::WindowId;

    use nebula_terminal::event::Event as TerminalEvent;

    use crate::config::Binding;
    use crate::message_bar::MessageBuffer;

    const KEY: Key<&'static str> = Key::Character("0");

    struct MockEventProxy;
    impl EventListener for MockEventProxy {}

    struct ActionContext<'a, T> {
        pub terminal: &'a mut Term<T>,
        pub size_info: &'a SizeInfo,
        pub mouse: &'a mut Mouse,
        pub clipboard: &'a mut Clipboard,
        pub message_buffer: &'a mut MessageBuffer,
        pub modifiers: Modifiers,
        config: &'a UiConfig,
        inline_search_state: &'a mut InlineSearchState,
    }

    impl<T: EventListener> super::ActionContext<T> for ActionContext<'_, T> {
        fn search_next(
            &mut self,
            _origin: Point,
            _direction: Direction,
            _side: Side,
        ) -> Option<Match> {
            None
        }

        fn search_direction(&self) -> Direction {
            Direction::Right
        }

        fn inline_search_state(&mut self) -> &mut InlineSearchState {
            self.inline_search_state
        }

        fn search_active(&self) -> bool {
            false
        }

        fn terminal(&self) -> &Term<T> {
            self.terminal
        }

        fn terminal_mut(&mut self) -> &mut Term<T> {
            self.terminal
        }

        fn size_info(&self) -> SizeInfo {
            *self.size_info
        }

        fn selection_is_empty(&self) -> bool {
            true
        }

        fn scroll(&mut self, scroll: Scroll) {
            self.terminal.scroll_display(scroll);
        }

        fn mouse_mode(&self) -> bool {
            false
        }

        #[inline]
        fn mouse_mut(&mut self) -> &mut Mouse {
            self.mouse
        }

        #[inline]
        fn mouse(&self) -> &Mouse {
            self.mouse
        }

        #[inline]
        fn touch_purpose(&mut self) -> &mut TouchPurpose {
            unimplemented!();
        }

        fn modifiers(&mut self) -> &mut Modifiers {
            &mut self.modifiers
        }

        fn window(&mut self) -> &mut Window {
            unimplemented!();
        }

        fn display(&mut self) -> &mut Display {
            unimplemented!();
        }

        fn pop_message(&mut self) {
            self.message_buffer.pop();
        }

        fn message(&self) -> Option<&Message> {
            self.message_buffer.message()
        }

        fn config(&self) -> &UiConfig {
            self.config
        }

        fn clipboard_mut(&mut self) -> &mut Clipboard {
            self.clipboard
        }

        #[cfg(target_os = "macos")]
        fn event_loop(&self) -> &ActiveEventLoop {
            unimplemented!();
        }

        fn scheduler_mut(&mut self) -> &mut Scheduler {
            unimplemented!();
        }

        fn semantic_word(&self, _point: Point) -> String {
            unimplemented!();
        }
    }

    macro_rules! test_clickstate {
        {
            name: $name:ident,
            initial_state: $initial_state:expr,
            initial_button: $initial_button:expr,
            input: $input:expr,
            end_state: $end_state:expr,
            input_delay: $input_delay:expr,
        } => {
            #[test]
            fn $name() {
                let mut clipboard = Clipboard::new_nop();
                let cfg = UiConfig::default();
                let size = SizeInfo::new(
                    21.0,
                    51.0,
                    3.0,
                    3.0,
                    0.,
                    0.,
                    false,
                );

                let mut terminal = Term::new(cfg.term_options(), &size, MockEventProxy);

                let mut mouse = Mouse {
                    click_state: $initial_state,
                    last_click_button: $initial_button,
                    last_click_timestamp: Instant::now() - $input_delay,
                    ..Mouse::default()
                };

                let mut inline_search_state = InlineSearchState::default();
                let mut message_buffer = MessageBuffer::default();

                let context = ActionContext {
                    terminal: &mut terminal,
                    mouse: &mut mouse,
                    size_info: &size,
                    clipboard: &mut clipboard,
                    modifiers: Default::default(),
                    message_buffer: &mut message_buffer,
                    inline_search_state: &mut inline_search_state,
                    config: &cfg,
                };

                let mut processor = Processor::new(context);

                let event: WinitEvent::<TerminalEvent> = $input;
                if let WinitEvent::WindowEvent {
                    event: WindowEvent::MouseInput {
                        state,
                        button,
                        ..
                    },
                    ..
                } = event
                {
                    processor.mouse_input(state, button);
                };

                assert_eq!(processor.ctx.mouse.click_state, $end_state);
            }
        }
    }

    macro_rules! test_process_binding {
        {
            name: $name:ident,
            binding: $binding:expr,
            triggers: $triggers:expr,
            mode: $mode:expr,
            mods: $mods:expr,
        } => {
            #[test]
            fn $name() {
                if $triggers {
                    assert!($binding.is_triggered_by($mode, $mods, &KEY));
                } else {
                    assert!(!$binding.is_triggered_by($mode, $mods, &KEY));
                }
            }
        }
    }

    test_clickstate! {
        name: single_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: single_right_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: single_middle_click,
        initial_state: ClickState::None,
        initial_button: MouseButton::Other(0),
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: double_click,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::DoubleClick,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: double_click_failed,
        initial_state: ClickState::Click,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: multi_click_time(),
    }

    test_clickstate! {
        name: triple_click,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id:  DeviceId::dummy(),
            },
            window_id:  WindowId::dummy(),
        },
        end_state: ClickState::TripleClick,
        input_delay: Duration::ZERO,
    }

    test_clickstate! {
        name: triple_click_failed,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: multi_click_time(),
    }

    test_clickstate! {
        name: multi_click_separate_buttons,
        initial_state: ClickState::DoubleClick,
        initial_button: MouseButton::Left,
        input: WinitEvent::WindowEvent {
            event: WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                device_id: DeviceId::dummy(),
            },
            window_id: WindowId::dummy(),
        },
        end_state: ClickState::Click,
        input_delay: Duration::ZERO,
    }

    test_process_binding! {
        name: process_binding_nomode_shiftmod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::SHIFT,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_shift,
        binding: Binding { trigger: KEY, mods: ModifiersState::SHIFT, action: Action::from("\x1b[1;2D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_controlmod,
        binding: Binding { trigger: KEY, mods: ModifiersState::CONTROL, action: Action::from("\x1b[1;5D"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::CONTROL,
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_not_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1b[D"), mode: BindingMode::empty(), notmode: BindingMode::APP_CURSOR },
        triggers: true,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_nomode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_appcursormode_appkeypadmode_nomod_require_appcursor,
        binding: Binding { trigger: KEY, mods: ModifiersState::empty(), action: Action::from("\x1bOD"), mode: BindingMode::APP_CURSOR, notmode: BindingMode::empty() },
        triggers: true,
        mode: BindingMode::APP_CURSOR | BindingMode::APP_KEYPAD,
        mods: ModifiersState::empty(),
    }

    test_process_binding! {
        name: process_binding_fail_with_extra_mods,
        binding: Binding { trigger: KEY, mods: ModifiersState::SUPER, action: Action::from("arst"), mode: BindingMode::empty(), notmode: BindingMode::empty() },
        triggers: false,
        mode: BindingMode::empty(),
        mods: ModifiersState::ALT | ModifiersState::SUPER,
    }
}
