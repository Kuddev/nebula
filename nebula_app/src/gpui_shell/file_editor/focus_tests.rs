//! Focus-mode and details-panel geometry contracts.

use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext, point, px};
use gpui_component::Root;

fn open(path: PathBuf, cx: &mut TestAppContext) -> (Entity<TextFileView>, VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        super::super::math_view::register(cx);
        init(cx);
    });
    let mut file = None;
    let (_, window) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| TextFileView::new(path, window, cx));
        file = Some(view.clone());
        Root::new(view, window, cx)
    });
    window.run_until_parked();
    (file.unwrap(), window.clone())
}

#[test]
fn details_width_clamps_to_the_reader_panel_contract() {
    assert_eq!(
        reader_presentation::clamp_details_width(-1.0),
        reader_presentation::DETAILS_MIN_WIDTH
    );
    assert_eq!(
        reader_presentation::clamp_details_width(reader_presentation::OUTLINE_WIDTH),
        reader_presentation::OUTLINE_WIDTH
    );
    assert_eq!(
        reader_presentation::clamp_details_width(f32::MAX),
        reader_presentation::DETAILS_MAX_WIDTH
    );
}

#[gpui::test]
fn reader_focus_round_trip_restores_details_and_draft_state(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("focus.md");
    std::fs::write(&path, "# Focus\n\nDraft stays local.\n").unwrap();
    let (file, mut cx) = open(path, cx);

    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.info = true;
            view.show_details = true;
            view.input.update(cx, |input, cx| {
                input.replace_all("draft", window, cx);
            });
            view.toggle_reader_focus(cx);
            assert!(view.reader_focus);
            assert!(!view.show_details);
        });
    });
    assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), "draft");

    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.toggle_reader_focus(cx);
            assert!(!view.reader_focus);
            assert!(view.show_details);
            assert!(view.info);
            view.input.update(cx, |input, cx| input.focus(window, cx));
        });
    });
    assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), "draft");
}

#[gpui::test]
fn details_panel_drag_updates_shared_width_and_releases_on_mouse_up(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resize.md");
    std::fs::write(&path, "# Resize\n\nBody\n").unwrap();
    let (file, mut cx) = open(path, cx);

    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let handle = cx.debug_bounds("file-details-resize-handle").unwrap();
    let start = handle.center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let target = point(start.x - px(32.0), start.y);
    cx.simulate_mouse_move(target, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    let width = file.read_with(&cx, |view, _| view.details_width);
    assert!(width > reader_presentation::OUTLINE_WIDTH);
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(file.read_with(&cx, |view, _| view.details_resize_anchor.is_none()));
    cx.update(|window, cx| {
        assert!(window.selected_text(cx).is_empty(), "resizing must not select reader text");
    });
}
