use super::*;

use crate::term::test::TermSize;
use crate::vte::ansi::Processor;

#[derive(Clone, Default)]
struct Replies(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.borrow_mut().push(text);
        }
    }
}

fn assert_steps(steps: &[(&[u8], u8)]) {
    let replies = Replies::default();
    let mut term = Term::new(
        Config { kitty_keyboard: true, ..Config::default() },
        &TermSize::new(80, 24),
        replies.clone(),
    );
    let mut parser: Processor = Processor::new();
    for &(input, expected) in steps {
        parser.advance(&mut term, input);
        parser.advance(&mut term, b"\x1b[?u");
        assert_eq!(
            std::mem::take(&mut *replies.0.borrow_mut()),
            [format!("\x1b[?{expected}u")],
            "input={input:?}"
        );
        assert_eq!(
            *term.mode() & TermMode::KITTY_KEYBOARD_PROTOCOL,
            TermMode::from(KeyboardModes::from_bits(expected).unwrap()),
            "input={input:?}"
        );
    }
}

#[test]
fn keyboard_contract_set_without_push_survives_nested_modes() {
    assert_steps(&[
        (b"\x1b[=5u", 5),
        (b"\x1b[>1u", 1),
        (b"\x1b[=7u", 7),
        (b"\x1b[>3u", 3),
        (b"\x1b[<u", 7),
        (b"\x1b[<u", 5),
        (b"\x1b[<u", 0),
        (b"\x1b[<65535u", 0),
    ]);
}

#[test]
fn keyboard_contract_each_screen_preserves_its_modified_frame() {
    assert_steps(&[
        (b"\x1b[=5u", 5),
        (b"\x1b[?1049h", 0),
        (b"\x1b[>1u\x1b[=7u", 7),
        (b"\x1b[?1049l", 5),
        (b"\x1b[?1049h", 7),
        (b"\x1b[?1049l", 5),
    ]);
}

#[test]
fn keyboard_contract_union_and_difference_persist_after_nested_pop() {
    assert_steps(&[
        (b"\x1b[>1u", 1),
        (b"\x1b[=2;2u", 3),
        (b"\x1b[>8u", 8),
        (b"\x1b[<u", 3),
        (b"\x1b[=1;3u", 2),
        (b"\x1b[>8u", 8),
        (b"\x1b[<u", 2),
    ]);
}

#[test]
fn keyboard_contract_overflow_preserves_titles_and_bounds_keyboard_storage() {
    let mut term = Term::new(
        Config { kitty_keyboard: true, ..Config::default() },
        &TermSize::new(80, 24),
        Replies::default(),
    );
    let mut parser: Processor = Processor::new();
    term.title_stack.push(Some("saved title".into()));
    for _ in 0..KEYBOARD_MODE_STACK_MAX_DEPTH + 2 {
        parser.advance(&mut term, b"\x1b[>1u");
    }
    assert!(term.keyboard_mode_stack.len() <= KEYBOARD_MODE_STACK_MAX_DEPTH);
    assert_eq!(term.title_stack, [Some("saved title".into())]);
}

#[test]
fn keyboard_contract_reset_clears_both_screens() {
    assert_steps(&[
        (b"\x1b[=5u", 5),
        (b"\x1b[?1049h\x1b[>3u", 3),
        (b"\x1bc", 0),
        (b"\x1b[?1049h", 0),
        (b"\x1b[?1049l", 0),
    ]);
}
