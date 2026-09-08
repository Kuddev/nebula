use super::*;
use gpui::{TestAppContext, VisualTestContext};
use gpui_component::Root;

fn open(path: PathBuf, cx: &mut TestAppContext) -> (Entity<TextFileView>, VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
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

#[gpui::test]
fn keyboard_save_writes_the_file_and_conflicts_preserve_the_draft(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("code.rs");
    std::fs::write(&path, "fn original() {}\n").unwrap();
    let (file, mut cx) = open(path.clone(), cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            assert!(!view.loading);
            view.input.update(cx, |input, cx| {
                input.replace_all("fn edited() {}\n", window, cx);
                input.focus(window, cx);
            });
        })
    });
    cx.run_until_parked();
    assert!(file.read_with(&cx, |file, _| file.is_dirty()));
    cx.simulate_keystrokes("ctrl-s");
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn edited() {}\n");
    assert!(!file.read_with(&cx, |file, _| file.is_dirty()));

    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.input.update(cx, |input, cx| input.replace_all("my draft", window, cx));
        })
    });
    cx.run_until_parked();
    std::fs::write(&path, "external change").unwrap();
    cx.update(|window, cx| file.update(cx, |view, cx| view.reload(window, cx)));
    assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), "my draft");
    cx.simulate_keystrokes("ctrl-s");
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "external change");
    assert!(file.read_with(&cx, |view, _| view.is_dirty()
        && matches!(view.notice, Some((Message::EditorConflict, _)))));
}

#[gpui::test]
fn editing_during_a_save_stays_dirty_and_markdown_jumps_to_source(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.md");
    std::fs::write(&path, "# First\n\n## Second\ntext\n").unwrap();
    let (file, mut cx) = open(path.clone(), cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            assert_eq!(view.outline.headings.len(), 2);
            view.preview = false;
            view.jump_to_heading(1, window, cx);
            assert_eq!(view.input.read(cx).cursor_position().line, 2);
            view.input.update(cx, |input, cx| input.replace_all("saved snapshot", window, cx));
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            view.save(cx).detach();
            view.input.update(cx, |input, cx| input.replace_all("next draft", window, cx));
        })
    });
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "saved snapshot");
    assert!(file.read_with(&cx, |view, _| view.is_dirty()));
    assert_eq!(
        file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()),
        "next draft"
    );
}
