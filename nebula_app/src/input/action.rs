//! Dispatch configured input actions to the active input context.

use log::debug;
#[cfg(target_os = "macos")]
use winit::platform::macos::ActiveEventLoopExtMacOS;

use nebula_terminal::event::EventListener;
use nebula_terminal::grid::{Dimensions, Scroll};
use nebula_terminal::index::{Boundary, Direction, Side};
use nebula_terminal::selection::SelectionType;
use nebula_terminal::term::{ClipboardType, TermMode};
use nebula_terminal::vi_mode::ViMotion;
use nebula_terminal::vte::ansi::{ClearMode, Handler};

#[cfg(target_os = "macos")]
use crate::config::window::Decorations;
use crate::config::{Action, MouseAction, SearchAction, ViAction};

use super::{ActionContext, FONT_SIZE_STEP};

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

pub(super) trait Execute<T: EventListener> {
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A);
}

impl<T: EventListener> Execute<T> for Action {
    #[inline]
    fn execute<A: ActionContext<T>>(&self, ctx: &mut A) {
        match self {
            // Esc/chars bindings are keystroke sequences (e.g. Shift+Enter → ESC+CR for
            // Claude Code multiline), not clipboard pastes. paste() gates on \r/\n and
            // would pop the multi-line confirm modal, so write through paste_now.
            Action::Esc(s) => ctx.paste_now(s, false),
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
