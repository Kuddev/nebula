//! Regex and inline search state shared by terminal input and rendering.

use std::collections::VecDeque;

use nebula_terminal::index::{Direction, Point};
use nebula_terminal::term::search::{Match, RegexSearch};

/// Regex search state.
pub struct SearchState {
    pub direction: Direction,
    pub history_index: Option<usize>,
    pub(super) display_offset_delta: i32,
    pub(super) origin: Point,
    pub(super) focused_match: Option<Match>,
    pub(super) history: VecDeque<String>,
    pub(super) dfas: Option<RegexSearch>,
}

impl SearchState {
    pub fn regex(&self) -> Option<&String> {
        self.history_index.and_then(|index| self.history.get(index))
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn focused_match(&self) -> Option<&Match> {
        self.focused_match.as_ref()
    }

    pub fn clear_focused_match(&mut self) {
        self.focused_match = None;
    }

    pub fn dfas(&mut self) -> Option<&mut RegexSearch> {
        self.dfas.as_mut()
    }

    pub(super) fn regex_mut(&mut self) -> Option<&mut String> {
        self.history_index.and_then(move |index| self.history.get_mut(index))
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            display_offset_delta: 0,
            focused_match: None,
            history_index: None,
            history: VecDeque::new(),
            origin: Point::default(),
            dfas: None,
        }
    }
}

/// Vi inline search state.
pub struct InlineSearchState {
    pub char_pending: bool,
    pub character: Option<char>,
    pub(super) direction: Direction,
    pub(super) stop_short: bool,
}

impl Default for InlineSearchState {
    fn default() -> Self {
        Self {
            direction: Direction::Right,
            char_pending: false,
            stop_short: false,
            character: None,
        }
    }
}
