use super::*;

use std::mem;

use crate::event::VoidListener;
use crate::grid::{Grid, Scroll};
use crate::index::{Column, Point, Side};
use crate::selection::{Selection, SelectionType};
use crate::term::cell::{Cell, Flags};
use crate::term::test::TermSize;
use crate::vte::ansi::{self, CharsetIndex, Handler, StandardCharset};

#[test]
fn win32_input_mode_tracks_decset_9001() {
    let size = TermSize::new(5, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    term.set_private_mode(PrivateMode::Unknown(9001));
    assert!(term.mode().contains(TermMode::WIN32_INPUT_MODE));

    term.unset_private_mode(PrivateMode::Unknown(9001));
    assert!(!term.mode().contains(TermMode::WIN32_INPUT_MODE));
}

/// 记录 `Event::PtyWrite` 的监听器：`send_event` 只拿 `&self`，所以要内部
/// 可变性。只留 PtyWrite——2031 的断言只关心回写 PTY 的字节。
#[derive(Clone, Default)]
struct WriteRecorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

impl WriteRecorder {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl EventListener for WriteRecorder {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.borrow_mut().push(text);
        }
    }
}

/// DECSET 2031 订阅的同一刻就要收到当前亮暗：规范里取初值靠
/// `CSI ? 996 n`，但 vte 0.15 不把私有 DSR 路由给 handler，我们答不了那条
/// 查询。订阅即回报是这个洞的替代品，掉了它 app 在下次换主题前无从得知。
#[test]
fn decset_2031_reports_the_current_scheme_immediately() {
    let size = TermSize::new(5, 5);
    let events = WriteRecorder::default();
    let mut term = Term::new(Config::default(), &size, events.clone());

    term.set_private_mode(PrivateMode::Unknown(2031));
    assert!(term.mode().contains(TermMode::COLOR_SCHEME_UPDATES));
    // 构造默认是暗底 → 1。
    assert_eq!(events.take(), vec!["\x1b[?997;1n".to_owned()]);

    term.unset_private_mode(PrivateMode::Unknown(2031));
    assert!(!term.mode().contains(TermMode::COLOR_SCHEME_UPDATES));
}

/// 没订阅的程序不该收到这串字节——它会当成用户敲进来的输入。
#[test]
fn color_scheme_change_stays_quiet_without_a_subscriber() {
    let size = TermSize::new(5, 5);
    let events = WriteRecorder::default();
    let mut term = Term::new(Config::default(), &size, events.clone());

    term.set_color_scheme(false);
    assert!(events.take().is_empty());
}

/// 热应用路径每次设置改动都整体走一遍，同值重复上报会把垃圾字节塞到
/// shell 提示符上，所以变化检测在 `set_color_scheme` 里。
#[test]
fn color_scheme_reports_only_on_a_real_flip() {
    let size = TermSize::new(5, 5);
    let events = WriteRecorder::default();
    let mut term = Term::new(Config::default(), &size, events.clone());

    term.set_private_mode(PrivateMode::Unknown(2031));
    assert_eq!(events.take(), vec!["\x1b[?997;1n".to_owned()]);

    // 暗 → 亮：一条 `;2n`。
    term.set_color_scheme(false);
    assert_eq!(events.take(), vec!["\x1b[?997;2n".to_owned()]);

    // 同值再来两次：一条都不发。
    term.set_color_scheme(false);
    term.set_color_scheme(false);
    assert!(events.take().is_empty());

    // 翻回去要报。
    term.set_color_scheme(true);
    assert_eq!(events.take(), vec!["\x1b[?997;1n".to_owned()]);
}

#[test]
fn background_darkness_uses_perceptual_luminance_not_the_channel_mean() {
    assert!(background_is_dark(0x0f, 0x11, 0x1a)); // Nebula 默认底
    assert!(!background_is_dark(0xff, 0xff, 0xff)); // 纯白
    // 纯蓝三通道平均是 85（<127.5，平均值也判暗），但纯绿的平均值同样是 85
    // 而加权亮度到 182——这一对才是「平均值会判错」的地方。
    assert!(background_is_dark(0x00, 0x00, 0xff));
    assert!(!background_is_dark(0x00, 0xff, 0x00));
}

/// 端到端走**真实字节**，而不是直接调 handler。
///
/// 2031 不在 vte 0.15 的 `NamedPrivateMode` 表里，它只能以
/// `PrivateMode::Unknown(2031)` 落到我们的 handler。上面那几个测试是直接调
/// `set_private_mode` 的，绕过了解析；如果解析器把 `\e[?2031h` 归到别处，
/// 整个功能是死的而单测照样全绿。这一条把那段路也钉住。
#[test]
fn decset_2031_survives_the_real_parser() {
    let size = TermSize::new(5, 5);
    let events = WriteRecorder::default();
    let mut term = Term::new(Config::default(), &size, events.clone());
    let mut parser: ansi::Processor = ansi::Processor::new();

    parser.advance(&mut term, b"\x1b[?2031h");
    assert!(
        term.mode().contains(TermMode::COLOR_SCHEME_UPDATES),
        "`\\e[?2031h` 必须经解析器落到 PrivateMode::Unknown(2031)"
    );
    assert_eq!(events.take(), vec!["\x1b[?997;1n".to_owned()]);

    // 主题翻成浅色：订阅方收到 `;2n`。
    term.set_color_scheme(false);
    assert_eq!(events.take(), vec!["\x1b[?997;2n".to_owned()]);

    // 退订之后不再收到。
    parser.advance(&mut term, b"\x1b[?2031l");
    assert!(!term.mode().contains(TermMode::COLOR_SCHEME_UPDATES));
    term.set_color_scheme(true);
    assert!(events.take().is_empty());
}

/// The visual-viewport crop previews the anchor of the deferred resize,
/// so the settled reflow shows exactly the rows the drag was showing.
#[test]
fn viewport_origin_previews_the_deferred_resize_crop() {
    let size = TermSize::new(10, 4);
    let config = Config { conpty_resize: true, ..Config::default() };
    let mut term = Term::new(config, &size, VoidListener);

    // Cursor on row 1 with output left behind on row 3.
    term.grid.cursor.point = Point::new(Line(1), Column(0));
    term.grid[Line(3)][Column(0)].c = 'x';

    // Nothing to crop while the viewport matches or exceeds the grid.
    assert_eq!(term.viewport_origin_for(4), Line(0));
    assert_eq!(term.viewport_origin_for(6), Line(0));

    // ConPTY's shrink keeps the last written row visible; the crop
    // previews that same anchor.
    assert_eq!(term.viewport_origin_for(3), Line(1));

    // The alternate screen is repainted by its application after the
    // commit and only ever follows the cursor.
    term.swap_alt();
    term.grid.cursor.point = Point::new(Line(1), Column(0));
    term.grid[Line(3)][Column(0)].c = 'x';
    assert_eq!(term.viewport_origin_for(3), Line(0));

    // A user scrolled into history stays anchored to the row they are
    // reading instead of the crop.
    term.swap_alt();
    for _ in 0..8 {
        term.newline();
    }
    term.scroll_display(Scroll::Delta(2));
    assert_eq!(term.viewport_origin_for(3), Line(-2));
}

#[test]
fn scroll_display_page_up() {
    let size = TermSize::new(5, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 11 lines of scrollback.
    for _ in 0..20 {
        term.newline();
    }

    // Scrollable amount to top is 11.
    term.scroll_display(Scroll::PageUp);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-1), Column(0)));
    assert_eq!(term.grid.display_offset(), 10);

    // Scrollable amount to top is 1.
    term.scroll_display(Scroll::PageUp);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-2), Column(0)));
    assert_eq!(term.grid.display_offset(), 11);

    // Scrollable amount to top is 0.
    term.scroll_display(Scroll::PageUp);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-2), Column(0)));
    assert_eq!(term.grid.display_offset(), 11);
}

#[test]
fn scroll_display_page_down() {
    let size = TermSize::new(5, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 11 lines of scrollback.
    for _ in 0..20 {
        term.newline();
    }

    // Change display_offset to topmost.
    term.grid_mut().scroll_display(Scroll::Top);
    term.vi_mode_cursor = ViModeCursor::new(Point::new(Line(-11), Column(0)));

    // Scrollable amount to bottom is 11.
    term.scroll_display(Scroll::PageDown);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-1), Column(0)));
    assert_eq!(term.grid.display_offset(), 1);

    // Scrollable amount to bottom is 1.
    term.scroll_display(Scroll::PageDown);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(0)));
    assert_eq!(term.grid.display_offset(), 0);

    // Scrollable amount to bottom is 0.
    term.scroll_display(Scroll::PageDown);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(0)));
    assert_eq!(term.grid.display_offset(), 0);
}

#[test]
fn simple_selection_works() {
    let size = TermSize::new(5, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let grid = term.grid_mut();
    for i in 0..4 {
        if i == 1 {
            continue;
        }

        grid[Line(i)][Column(0)].c = '"';

        for j in 1..4 {
            grid[Line(i)][Column(j)].c = 'a';
        }

        grid[Line(i)][Column(4)].c = '"';
    }
    grid[Line(2)][Column(0)].c = ' ';
    grid[Line(2)][Column(4)].c = ' ';
    grid[Line(2)][Column(4)].flags.insert(Flags::WRAPLINE);
    grid[Line(3)][Column(0)].c = ' ';

    // Multiple lines contain an empty line.
    term.selection = Some(Selection::new(
        SelectionType::Simple,
        Point { line: Line(0), column: Column(0) },
        Side::Left,
    ));
    if let Some(s) = term.selection.as_mut() {
        s.update(Point { line: Line(2), column: Column(4) }, Side::Right);
    }
    assert_eq!(term.selection_to_string(), Some(String::from("\"aaa\"\n\n aaa ")));

    // A wrapline.
    term.selection = Some(Selection::new(
        SelectionType::Simple,
        Point { line: Line(2), column: Column(0) },
        Side::Left,
    ));
    if let Some(s) = term.selection.as_mut() {
        s.update(Point { line: Line(3), column: Column(4) }, Side::Right);
    }
    assert_eq!(term.selection_to_string(), Some(String::from(" aaa  aaa\"")));
}

#[test]
fn semantic_selection_works() {
    let size = TermSize::new(5, 3);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let mut grid: Grid<Cell> = Grid::new(3, 5, 0);
    for i in 0..5 {
        for j in 0..2 {
            grid[Line(j)][Column(i)].c = 'a';
        }
    }
    grid[Line(0)][Column(0)].c = '"';
    grid[Line(0)][Column(3)].c = '"';
    grid[Line(1)][Column(2)].c = '"';
    grid[Line(0)][Column(4)].flags.insert(Flags::WRAPLINE);

    let mut escape_chars = String::from("\"");

    mem::swap(&mut term.grid, &mut grid);
    mem::swap(&mut term.config.semantic_escape_chars, &mut escape_chars);

    {
        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point { line: Line(0), column: Column(1) },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("aa")));
    }

    {
        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point { line: Line(0), column: Column(4) },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
    }

    {
        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point { line: Line(1), column: Column(1) },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
    }
}

#[test]
fn line_selection_works() {
    let size = TermSize::new(5, 1);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let mut grid: Grid<Cell> = Grid::new(1, 5, 0);
    for i in 0..5 {
        grid[Line(0)][Column(i)].c = 'a';
    }
    grid[Line(0)][Column(0)].c = '"';
    grid[Line(0)][Column(3)].c = '"';

    mem::swap(&mut term.grid, &mut grid);

    term.selection = Some(Selection::new(
        SelectionType::Lines,
        Point { line: Line(0), column: Column(3) },
        Side::Left,
    ));
    assert_eq!(term.selection_to_string(), Some(String::from("\"aa\"a\n")));
}

#[test]
fn block_selection_works() {
    let size = TermSize::new(5, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let grid = term.grid_mut();
    for i in 1..4 {
        grid[Line(i)][Column(0)].c = '"';

        for j in 1..4 {
            grid[Line(i)][Column(j)].c = 'a';
        }

        grid[Line(i)][Column(4)].c = '"';
    }
    grid[Line(2)][Column(2)].c = ' ';
    grid[Line(2)][Column(4)].flags.insert(Flags::WRAPLINE);
    grid[Line(3)][Column(4)].c = ' ';

    term.selection = Some(Selection::new(
        SelectionType::Block,
        Point { line: Line(0), column: Column(3) },
        Side::Left,
    ));

    // The same column.
    if let Some(s) = term.selection.as_mut() {
        s.update(Point { line: Line(3), column: Column(3) }, Side::Right);
    }
    assert_eq!(term.selection_to_string(), Some(String::from("\na\na\na")));

    // The first column.
    if let Some(s) = term.selection.as_mut() {
        s.update(Point { line: Line(3), column: Column(0) }, Side::Left);
    }
    assert_eq!(term.selection_to_string(), Some(String::from("\n\"aa\n\"a\n\"aa")));

    // The last column.
    if let Some(s) = term.selection.as_mut() {
        s.update(Point { line: Line(3), column: Column(4) }, Side::Right);
    }
    assert_eq!(term.selection_to_string(), Some(String::from("\na\"\na\"\na")));
}

/// Check that the grid can be serialized back and forth losslessly.
///
/// This test is in the term module as opposed to the grid since we want to
/// test this property with a T=Cell.
#[test]
#[cfg(feature = "serde")]
fn grid_serde() {
    let grid: Grid<Cell> = Grid::new(24, 80, 0);
    let serialized = serde_json::to_string(&grid).expect("ser");
    let deserialized = serde_json::from_str::<Grid<Cell>>(&serialized).expect("de");

    assert_eq!(deserialized, grid);
}

#[test]
fn input_line_drawing_character() {
    let size = TermSize::new(7, 17);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let cursor = Point::new(Line(0), Column(0));
    term.configure_charset(CharsetIndex::G0, StandardCharset::SpecialCharacterAndLineDrawing);
    term.input('a');

    assert_eq!(term.grid()[cursor].c, '▒');
}

#[test]
fn clearing_viewport_keeps_history_position() {
    let size = TermSize::new(10, 20);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..29 {
        term.newline();
    }

    // Change the display area.
    term.scroll_display(Scroll::Top);

    assert_eq!(term.grid.display_offset(), 10);

    // Clear the viewport.
    term.clear_screen(ansi::ClearMode::All);

    assert_eq!(term.grid.display_offset(), 10);
}

#[test]
fn clearing_viewport_with_vi_mode_keeps_history_position() {
    let size = TermSize::new(10, 20);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..29 {
        term.newline();
    }

    // Enable vi mode.
    term.toggle_vi_mode();

    // Change the display area and the vi cursor position.
    term.scroll_display(Scroll::Top);
    term.vi_mode_cursor.point = Point::new(Line(-5), Column(3));

    assert_eq!(term.grid.display_offset(), 10);

    // Clear the viewport.
    term.clear_screen(ansi::ClearMode::All);

    assert_eq!(term.grid.display_offset(), 10);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(-5), Column(3)));
}

#[test]
fn clearing_scrollback_resets_display_offset() {
    let size = TermSize::new(10, 20);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..29 {
        term.newline();
    }

    // Change the display area.
    term.scroll_display(Scroll::Top);

    assert_eq!(term.grid.display_offset(), 10);

    // Clear the scrollback buffer.
    term.clear_screen(ansi::ClearMode::Saved);

    assert_eq!(term.grid.display_offset(), 0);
}

#[test]
fn clearing_scrollback_sets_vi_cursor_into_viewport() {
    let size = TermSize::new(10, 20);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..29 {
        term.newline();
    }

    // Enable vi mode.
    term.toggle_vi_mode();

    // Change the display area and the vi cursor position.
    term.scroll_display(Scroll::Top);
    term.vi_mode_cursor.point = Point::new(Line(-5), Column(3));

    assert_eq!(term.grid.display_offset(), 10);

    // Clear the scrollback buffer.
    term.clear_screen(ansi::ClearMode::Saved);

    assert_eq!(term.grid.display_offset(), 0);
    assert_eq!(term.vi_mode_cursor.point, Point::new(Line(0), Column(3)));
}

#[test]
fn clear_saved_lines() {
    let size = TermSize::new(7, 17);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Add one line of scrollback.
    term.grid.scroll_up(&(Line(0)..Line(1)), 1);

    // Clear the history.
    term.clear_screen(ansi::ClearMode::Saved);

    // Make sure that scrolling does not change the grid.
    let mut scrolled_grid = term.grid.clone();
    scrolled_grid.scroll_display(Scroll::Top);

    // Truncate grids for comparison.
    scrolled_grid.truncate();
    term.grid.truncate();

    assert_eq!(term.grid, scrolled_grid);
}

#[test]
fn vi_cursor_keep_pos_on_scrollback_buffer() {
    let size = TermSize::new(5, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 11 lines of scrollback.
    for _ in 0..20 {
        term.newline();
    }

    // Enable vi mode.
    term.toggle_vi_mode();

    term.scroll_display(Scroll::Top);
    term.vi_mode_cursor.point.line = Line(-11);

    term.linefeed();
    assert_eq!(term.vi_mode_cursor.point.line, Line(-12));
}

#[test]
fn grow_lines_updates_active_cursor_pos() {
    let mut size = TermSize::new(100, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..19 {
        term.newline();
    }
    assert_eq!(term.history_size(), 10);
    assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

    // Increase visible lines.
    size.screen_lines = 30;
    term.resize(size);

    assert_eq!(term.history_size(), 0);
    assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
}

#[test]
fn grow_lines_updates_inactive_cursor_pos() {
    let mut size = TermSize::new(100, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..19 {
        term.newline();
    }
    assert_eq!(term.history_size(), 10);
    assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

    // Enter alt screen.
    term.set_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

    // Increase visible lines.
    size.screen_lines = 30;
    term.resize(size);

    // Leave alt screen.
    term.unset_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

    assert_eq!(term.history_size(), 0);
    assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
}

#[test]
fn shrink_lines_updates_active_cursor_pos() {
    let mut size = TermSize::new(100, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..19 {
        term.newline();
    }
    assert_eq!(term.history_size(), 10);
    assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

    // Increase visible lines.
    size.screen_lines = 5;
    term.resize(size);

    assert_eq!(term.history_size(), 15);
    assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
}

#[test]
fn shrink_lines_updates_inactive_cursor_pos() {
    let mut size = TermSize::new(100, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Create 10 lines of scrollback.
    for _ in 0..19 {
        term.newline();
    }
    assert_eq!(term.history_size(), 10);
    assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

    // Enter alt screen.
    term.set_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

    // Increase visible lines.
    size.screen_lines = 5;
    term.resize(size);

    // Leave alt screen.
    term.unset_private_mode(NamedPrivateMode::SwapScreenAndSetRestoreCursor.into());

    assert_eq!(term.history_size(), 15);
    assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
}

#[test]
fn conpty_realign_scrolls_excess_rows_into_history() {
    let size = TermSize::new(8, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    for (row, marker) in ['a', 'b', 'c', 'd', 'e'].into_iter().enumerate() {
        term.grid[Line(row as i32)][Column(0)].c = marker;
    }
    term.grid.cursor.point = Point::new(Line(4), Column(0));

    term.conpty_realign(2);

    assert_eq!(term.grid.cursor.point, Point::new(Line(2), Column(0)));
    assert_eq!(term.history_size(), 2);
    assert_eq!(term.grid[Line(0)][Column(0)].c, 'c');
    assert_eq!(term.grid[Line(1)][Column(0)].c, 'd');
    assert_eq!(term.grid[Line(2)][Column(0)].c, 'e');
    assert_eq!(term.grid[Line(3)][Column(0)].c, ' ');
    assert_eq!(term.grid[Line(4)][Column(0)].c, ' ');
}

#[test]
fn conpty_realign_pushes_content_down_when_conhost_wrapped_more() {
    // 反方向：conhost 的 rewrap 折出的行比本地 reflow 多，它的视口顶部还
    // 留着我们已推进历史的行，提示符与光标都更靠下。缺的行必须在光标上方
    // 补回，否则 PSReadLine 按 conhost 坐标发的绝对 CUP 会落在提示符下面
    // 几行（现场：分屏后 prompts=[15] 而 cursor=19）。
    let size = TermSize::new(8, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    for (row, marker) in ['a', 'b', 'c', 'd', 'e'].into_iter().enumerate() {
        term.grid[Line(row as i32)][Column(0)].c = marker;
    }
    term.grid.cursor.point = Point::new(Line(2), Column(0));

    term.conpty_realign(4);

    assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
    assert_eq!(term.grid[Line(0)][Column(0)].c, ' ');
    assert_eq!(term.grid[Line(1)][Column(0)].c, ' ');
    assert_eq!(term.grid[Line(2)][Column(0)].c, 'a');
    assert_eq!(term.grid[Line(3)][Column(0)].c, 'b');
    assert_eq!(term.grid[Line(4)][Column(0)].c, 'c');
}

#[test]
fn conpty_realign_is_a_noop_when_both_sides_agree() {
    let size = TermSize::new(8, 5);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    for (row, marker) in ['a', 'b', 'c', 'd', 'e'].into_iter().enumerate() {
        term.grid[Line(row as i32)][Column(0)].c = marker;
    }
    term.grid.cursor.point = Point::new(Line(3), Column(0));

    term.conpty_realign(3);

    assert_eq!(term.grid.cursor.point, Point::new(Line(3), Column(0)));
    assert_eq!(term.history_size(), 0);
    assert_eq!(term.grid[Line(0)][Column(0)].c, 'a');
}

#[test]
fn reflow_keeps_prompt_input_at_the_logical_cursor() {
    let size = TermSize::new(6, 3);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    for c in "promp".chars() {
        term.input(c);
    }

    for columns in (3..6).rev() {
        term.resize(TermSize::new(columns, 3));
    }
    for columns in 4..=6 {
        term.resize(TermSize::new(columns, 3));
    }

    term.input('!');

    assert_eq!(term.grid()[Line(0)][Column(5)].c, '!');
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(5)));
    assert!(term.grid.cursor.input_needs_wrap);
}

#[test]
fn reflow_preserves_pending_wrap_before_the_next_input() {
    let size = TermSize::new(6, 3);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    for c in "123456".chars() {
        term.input(c);
    }
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(5)));
    assert!(term.grid.cursor.input_needs_wrap);

    for columns in (3..6).rev() {
        term.resize(TermSize::new(columns, 3));
    }
    for columns in 4..=6 {
        term.resize(TermSize::new(columns, 3));
    }

    // The cursor is visually on the last cell, but the next byte must wrap
    // before writing instead of overwriting that cell.
    assert_eq!(term.grid.cursor.point, Point::new(Line(0), Column(5)));
    assert!(term.grid.cursor.input_needs_wrap);
    term.input('X');

    assert_eq!(term.grid[Line(1)][Column(0)].c, 'X');
    assert_eq!(term.grid.cursor.point, Point::new(Line(1), Column(1)));
    assert!(!term.grid.cursor.input_needs_wrap);
}

#[test]
fn damage_public_usage() {
    let size = TermSize::new(10, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    // Reset terminal for partial damage tests since it's initialized as fully damaged.
    term.reset_damage();

    // Test that we damage input form [`Term::input`].

    let left = term.grid.cursor.point.column.0;
    term.input('d');
    term.input('a');
    term.input('m');
    term.input('a');
    term.input('g');
    term.input('e');
    let right = term.grid.cursor.point.column.0;

    let mut damaged_lines = match term.damage() {
        TermDamage::Full => panic!("Expected partial damage, however got Full"),
        TermDamage::Partial(damaged_lines) => damaged_lines,
    };
    assert_eq!(damaged_lines.next(), Some(LineDamageBounds { line: 0, left, right }));
    assert_eq!(damaged_lines.next(), None);
    term.reset_damage();

    // Create scrollback.
    for _ in 0..20 {
        term.newline();
    }

    match term.damage() {
        TermDamage::Full => (),
        TermDamage::Partial(_) => panic!("Expected Full damage, however got Partial "),
    };
    term.reset_damage();

    term.scroll_display(Scroll::Delta(10));
    term.reset_damage();

    // No damage when scrolled into viewport.
    for idx in 0..term.columns() {
        term.goto(idx as i32, idx);
    }
    let mut damaged_lines = match term.damage() {
        TermDamage::Full => panic!("Expected partial damage, however got Full"),
        TermDamage::Partial(damaged_lines) => damaged_lines,
    };
    assert_eq!(damaged_lines.next(), None);

    // Scroll back into the viewport, so we have 2 visible lines which terminal can write
    // to.
    term.scroll_display(Scroll::Delta(-2));
    term.reset_damage();

    term.goto(0, 0);
    term.goto(1, 0);
    term.goto(2, 0);
    let display_offset = term.grid().display_offset();
    let mut damaged_lines = match term.damage() {
        TermDamage::Full => panic!("Expected partial damage, however got Full"),
        TermDamage::Partial(damaged_lines) => damaged_lines,
    };
    assert_eq!(
        damaged_lines.next(),
        Some(LineDamageBounds { line: display_offset, left: 0, right: 0 })
    );
    assert_eq!(
        damaged_lines.next(),
        Some(LineDamageBounds { line: display_offset + 1, left: 0, right: 0 })
    );
    assert_eq!(damaged_lines.next(), None);
}

#[test]
fn reset_dynamic_colors_clears_overrides_and_marks_full_damage() {
    let size = TermSize::new(10, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let color = ansi::Rgb { r: 1, g: 2, b: 3 };
    term.set_color(NamedColor::Foreground as usize, color);
    term.set_color(NamedColor::Cursor as usize, color);
    term.set_color(24, color);
    term.reset_damage();

    term.reset_dynamic_colors();

    assert_eq!(term.colors()[NamedColor::Foreground], None);
    assert_eq!(term.colors()[NamedColor::Cursor], None);
    assert_eq!(term.colors()[24], None);
    assert!(matches!(term.damage(), TermDamage::Full));
}

#[test]
fn damage_cursor_movements() {
    let size = TermSize::new(10, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);
    let num_cols = term.columns();
    // Reset terminal for partial damage tests since it's initialized as fully damaged.
    term.reset_damage();

    term.goto(1, 1);

    // NOTE While we can use `[Term::damage]` to access terminal damage information, in the
    // following tests we will be accessing `term.damage.lines` directly to avoid adding extra
    // damage information (like cursor and Vi cursor), which we're not testing.

    assert_eq!(term.damage.lines[0], LineDamageBounds { line: 0, left: 0, right: 0 });
    assert_eq!(term.damage.lines[1], LineDamageBounds { line: 1, left: 1, right: 1 });
    term.damage.reset(num_cols);

    term.move_forward(3);
    assert_eq!(term.damage.lines[1], LineDamageBounds { line: 1, left: 1, right: 4 });
    term.damage.reset(num_cols);

    term.move_backward(8);
    assert_eq!(term.damage.lines[1], LineDamageBounds { line: 1, left: 0, right: 4 });
    term.goto(5, 5);
    term.damage.reset(num_cols);

    term.backspace();
    term.backspace();
    assert_eq!(term.damage.lines[5], LineDamageBounds { line: 5, left: 3, right: 5 });
    term.damage.reset(num_cols);

    term.move_up(1);
    assert_eq!(term.damage.lines[5], LineDamageBounds { line: 5, left: 3, right: 3 });
    assert_eq!(term.damage.lines[4], LineDamageBounds { line: 4, left: 3, right: 3 });
    term.damage.reset(num_cols);

    term.move_down(1);
    term.move_down(1);
    assert_eq!(term.damage.lines[4], LineDamageBounds { line: 4, left: 3, right: 3 });
    assert_eq!(term.damage.lines[5], LineDamageBounds { line: 5, left: 3, right: 3 });
    assert_eq!(term.damage.lines[6], LineDamageBounds { line: 6, left: 3, right: 3 });
    term.damage.reset(num_cols);

    term.wrapline();
    assert_eq!(term.damage.lines[6], LineDamageBounds { line: 6, left: 3, right: 3 });
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right: 0 });
    term.move_forward(3);
    term.move_up(1);
    term.damage.reset(num_cols);

    term.linefeed();
    assert_eq!(term.damage.lines[6], LineDamageBounds { line: 6, left: 3, right: 3 });
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 3, right: 3 });
    term.damage.reset(num_cols);

    term.carriage_return();
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right: 3 });
    term.damage.reset(num_cols);

    term.erase_chars(5);
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right: 5 });
    term.damage.reset(num_cols);

    term.delete_chars(3);
    let right = term.columns() - 1;
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right });
    term.move_forward(term.columns());
    term.damage.reset(num_cols);

    term.move_backward_tabs(1);
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 8, right });
    term.save_cursor_position();
    term.goto(1, 1);
    term.damage.reset(num_cols);

    term.restore_cursor_position();
    assert_eq!(term.damage.lines[1], LineDamageBounds { line: 1, left: 1, right: 1 });
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 8, right: 8 });
    term.damage.reset(num_cols);

    term.clear_line(ansi::LineClearMode::All);
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right });
    term.damage.reset(num_cols);

    term.clear_line(ansi::LineClearMode::Left);
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 0, right: 8 });
    term.damage.reset(num_cols);

    term.clear_line(ansi::LineClearMode::Right);
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 8, right });
    term.damage.reset(num_cols);

    term.reverse_index();
    assert_eq!(term.damage.lines[7], LineDamageBounds { line: 7, left: 8, right: 8 });
    assert_eq!(term.damage.lines[6], LineDamageBounds { line: 6, left: 8, right: 8 });
}

#[test]
fn full_damage() {
    let size = TermSize::new(100, 10);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    assert!(term.damage.full);
    for _ in 0..20 {
        term.newline();
    }
    term.reset_damage();

    term.clear_screen(ansi::ClearMode::Above);
    assert!(term.damage.full);
    term.reset_damage();

    term.scroll_display(Scroll::Top);
    assert!(term.damage.full);
    term.reset_damage();

    // Sequential call to scroll display without doing anything shouldn't damage.
    term.scroll_display(Scroll::Top);
    assert!(!term.damage.full);
    term.reset_damage();

    term.set_options(Config::default());
    assert!(term.damage.full);
    term.reset_damage();

    term.scroll_down_relative(Line(5), 2);
    assert!(term.damage.full);
    term.reset_damage();

    term.scroll_up_relative(Line(3), 2);
    assert!(term.damage.full);
    term.reset_damage();

    term.deccolm();
    assert!(term.damage.full);
    term.reset_damage();

    term.decaln();
    assert!(term.damage.full);
    term.reset_damage();

    term.set_mode(NamedMode::Insert.into());
    // Just setting `Insert` mode shouldn't mark terminal as damaged.
    assert!(!term.damage.full);
    term.reset_damage();

    let color_index = 257;
    term.set_color(color_index, Rgb::default());
    assert!(term.damage.full);
    term.reset_damage();

    // Setting the same color once again shouldn't trigger full damage.
    term.set_color(color_index, Rgb::default());
    assert!(!term.damage.full);

    term.reset_color(color_index);
    assert!(term.damage.full);
    term.reset_damage();

    // We shouldn't trigger fully damage when cursor gets update.
    term.set_color(NamedColor::Cursor as usize, Rgb::default());
    assert!(!term.damage.full);

    // However requesting terminal damage should mark terminal as fully damaged in `Insert`
    // mode.
    let _ = term.damage();
    assert!(term.damage.full);
    term.reset_damage();

    term.unset_mode(NamedMode::Insert.into());
    assert!(term.damage.full);
    term.reset_damage();

    // Keep this as a last check, so we don't have to deal with restoring from alt-screen.
    term.swap_alt();
    assert!(term.damage.full);
    term.reset_damage();

    let size = TermSize::new(10, 10);
    term.resize(size);
    assert!(term.damage.full);
}

#[test]
fn window_title() {
    let size = TermSize::new(7, 17);
    let mut term = Term::new(Config::default(), &size, VoidListener);

    // Title None by default.
    assert_eq!(term.title, None);

    // Title can be set.
    term.set_title(Some("Test".into()));
    assert_eq!(term.title, Some("Test".into()));

    // Title can be pushed onto stack.
    term.push_title();
    term.set_title(Some("Next".into()));
    assert_eq!(term.title, Some("Next".into()));
    assert_eq!(term.title_stack.first().unwrap(), &Some("Test".into()));

    // Title can be popped from stack and set as the window title.
    term.pop_title();
    assert_eq!(term.title, Some("Test".into()));
    assert!(term.title_stack.is_empty());

    // Title stack doesn't grow infinitely.
    for _ in 0..4097 {
        term.push_title();
    }
    assert_eq!(term.title_stack.len(), 4096);

    // Title and title stack reset when terminal state is reset.
    term.push_title();
    term.reset_state();
    assert_eq!(term.title, None);
    assert!(term.title_stack.is_empty());

    // Title stack pops back to default.
    term.title = None;
    term.push_title();
    term.set_title(Some("Test".into()));
    term.pop_title();
    assert_eq!(term.title, None);

    // Title can be reset to default.
    term.title = Some("Test".into());
    term.set_title(None);
    assert_eq!(term.title, None);
}

#[test]
fn parse_cargo_version() {
    // Floor: at least 0.1.0 — the version reported via Secondary DA.
    assert!(version_number(env!("CARGO_PKG_VERSION")) >= 1_00);
    assert_eq!(version_number("0.0.1-dev"), 1);
    assert_eq!(version_number("0.1.2-dev"), 1_02);
    assert_eq!(version_number("1.2.3-dev"), 1_02_03);
    assert_eq!(version_number("999.99.99"), 9_99_99_99);
}
