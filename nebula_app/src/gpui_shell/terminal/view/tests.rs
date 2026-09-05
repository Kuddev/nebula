use gpui::{FontStyle, FontWeight};

use super::{
    TermMode, TerminalView, cursor_blink_allowed, mono_font, paste_line_count,
    paste_needs_confirmation, restart_cursor_blink_phase, selection_scroll_lines,
};

#[test]
fn ime_preedit_stops_blinking_and_restores_visible_cursor() {
    assert!(!cursor_blink_allowed(true, false, true, true, true, true));

    let (mut visible, mut epoch) = (false, 7);
    assert_eq!(restart_cursor_blink_phase(&mut visible, &mut epoch), 8);
    assert!(visible);
}

#[test]
fn unfocused_cursor_does_not_blink_and_stays_visible() {
    assert!(!cursor_blink_allowed(true, false, true, false, false, true));
    assert!(!cursor_blink_allowed(true, false, true, false, true, false));

    let (mut visible, mut epoch) = (false, 2);
    restart_cursor_blink_phase(&mut visible, &mut epoch);
    assert!(visible);
}

#[test]
fn restoring_focus_restarts_blinking_immediately() {
    assert!(!cursor_blink_allowed(true, false, true, false, true, false));
    assert!(cursor_blink_allowed(true, false, true, false, true, true));

    let (mut visible, mut epoch) = (false, 11);
    assert_eq!(restart_cursor_blink_phase(&mut visible, &mut epoch), 12);
    assert!(visible, "restart must restore the visible phase before the next tick");
}

/// 拖选自动回滚的判据（旧壳 `update_selection_scrolling` 的数值合同）：
/// 网格内不滚，越过上边界往回滚历史、越过下边界往前滚，贴边 1 行、每远离
/// 20px 加一档，并且封顶在一屏——指针甩出窗口不该一抖到底。
#[test]
fn selection_scroll_speed_scales_with_distance_past_the_grid() {
    let (top, bottom, rows) = (100.0, 500.0, 30);
    assert_eq!(selection_scroll_lines(300.0, top, bottom, rows), 0, "网格内不滚");
    assert_eq!(selection_scroll_lines(top, top, bottom, rows), 0, "上边界本身仍在网格内");
    assert_eq!(selection_scroll_lines(top - 1.0, top, bottom, rows), 1, "刚越界＝最慢一档");
    assert_eq!(selection_scroll_lines(top - 20.0, top, bottom, rows), 2);
    assert_eq!(selection_scroll_lines(top - 40.0, top, bottom, rows), 3);
    assert_eq!(selection_scroll_lines(bottom, top, bottom, rows), -1, "底边像素属于下方");
    assert_eq!(selection_scroll_lines(bottom + 20.0, top, bottom, rows), -2);
    assert_eq!(
        selection_scroll_lines(-10_000.0, top, bottom, rows),
        rows,
        "甩到屏幕外也每 tick 最多一屏"
    );
    assert_eq!(selection_scroll_lines(10_000.0, top, bottom, rows), -rows);
}

/// 行数判据数的是「落到终端算几行」，不是 `str::lines`：CRLF 一次换行、
/// 裸 CR 也是换行（`str::lines` 会把它整段算成 1 行）、末尾那个换行不多
/// 算一行。这个值用于风险粘贴确认文案。
#[test]
fn paste_line_count_counts_terminal_rows_not_str_lines() {
    assert_eq!(paste_line_count(""), 0);
    assert_eq!(paste_line_count("one"), 1);
    assert_eq!(paste_line_count("one\n"), 1, "末尾换行不额外多算一行");
    assert_eq!(paste_line_count("a\r\nb"), 2, "CRLF 只算一次换行");
    assert_eq!(paste_line_count("a\rb\rc"), 3, "裸 CR 也是换行");
    assert_eq!(paste_line_count("a\r\nb\r\n"), 2);
}

#[test]
fn paste_confirmation_follows_execution_risk_not_volume() {
    let plain = TermMode::empty();
    assert!(!paste_needs_confirmation("echo safe", plain));
    assert!(paste_needs_confirmation("echo one\necho two", plain));
    assert!(paste_needs_confirmation("sudo cargo test", plain));
    assert!(paste_needs_confirmation("su -", plain));
    assert!(paste_needs_confirmation("echo safe\u{3}", plain));

    let many_lines = "echo safe\n".repeat(100);
    assert!(!paste_needs_confirmation(&many_lines, TermMode::BRACKETED_PASTE));
    assert!(!paste_needs_confirmation(&many_lines, TermMode::ALT_SCREEN));
}

#[test]
fn terminal_font_explicitly_enables_maple_ligatures() {
    let font =
        mono_font(crate::font_install::REQUIRED_FONT_FAMILY, FontWeight::NORMAL, FontStyle::Normal);
    assert_eq!(font.features.tag_value_list(), &[("calt".to_owned(), 1)]);
}

#[test]
fn cell_width_mode_matches_the_legacy_grid_rounding_contract() {
    assert_eq!(
        f32::from(TerminalView::effective_cell_width(
            10.8,
            nebula_settings::CellWidthModeName::Compact,
            1.0,
            0.0,
        )),
        10.0
    );
    assert_eq!(
        f32::from(TerminalView::effective_cell_width(
            10.8,
            nebula_settings::CellWidthModeName::Relaxed,
            1.0,
            0.0,
        )),
        11.0
    );

    let compact = f32::from(TerminalView::effective_cell_width(
        9.2,
        nebula_settings::CellWidthModeName::Compact,
        1.5,
        0.0,
    ));
    assert!((compact - 13.0 / 1.5).abs() < f32::EPSILON);

    let relaxed = f32::from(TerminalView::effective_cell_width(
        9.2,
        nebula_settings::CellWidthModeName::Relaxed,
        1.5,
        0.0,
    ));
    assert!((relaxed - 14.0 / 1.5).abs() < f32::EPSILON);
}

#[test]
fn line_height_uses_shaped_metrics_and_device_pixel_offset() {
    // Maple's hhea metrics are 1.32em. At 150% DPI, the legacy 4px
    // Windows offset yields floor(15 * 1.32 * 1.5 + 4) = 33 device px.
    let height = TerminalView::effective_line_height(15.0 * 1.32, 4.0, 1.5);
    assert!((f32::from(height) - 22.0).abs() < f32::EPSILON);
}
