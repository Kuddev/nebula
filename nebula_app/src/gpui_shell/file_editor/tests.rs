use super::*;
use gpui::{TestAppContext, VisualTestContext};
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

#[gpui::test]
fn source_mode_releases_preview_views_without_changing_the_document(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("preview.md");
    let source = "# Title\n\n$x^2$\n";
    std::fs::write(&path, source).unwrap();
    let (file, mut cx) = open(path, cx);
    cx.update(|window, cx| {
        file.update(cx, |view, cx| {
            assert!(view.preview && !view.loading);
            let block = cx.new(|cx| TextViewState::markdown("cached preview", cx));
            view.blocks.borrow_mut()[0] = Some(block);
            view.all_selected = true;
            view.preview_selection_scroll_active = true;
            let count = view.outline.blocks.len();
            view.toggle_preview(window, cx);
            assert!(!view.preview && !view.all_selected);
            assert!(!view.preview_selection_scroll_active);
            assert!(view.blocks.borrow().iter().all(Option::is_none));
            assert_eq!(view.input.read(cx).value().as_ref(), source);
            assert_eq!(view.outline.blocks.len(), count);
            assert!(!view.dirty);
            view.toggle_preview(window, cx);
            assert!(view.preview);
            assert!(view.blocks.borrow().iter().all(Option::is_none));
        });
    });
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
fn outline_arrow_folds_without_navigating_or_rewriting_the_document(cx: &mut TestAppContext) {
    use gpui::Modifiers;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("tree.md");
    let text = "# Root\n\n## Child\n\n### Leaf\n\n## Sibling\n";
    std::fs::write(&path, text).unwrap();
    let (file, mut cx) = open(path.clone(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let arrow = cx.debug_bounds("outline-fold-0").unwrap().center();
    cx.simulate_mouse_down(arrow, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(arrow, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(file.read_with(&cx, |view, _| view.collapsed_headings.contains(&0)));
    assert_eq!(file.read_with(&cx, |view, _| view.selected_heading), None);
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), text);
}

#[gpui::test]
fn document_chrome_modes_are_idempotent_and_sidebar_tabs_are_exclusive(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("chrome.md");
    let source = "# Title\n\nOriginal text\n";
    std::fs::write(&path, source).unwrap();
    let (file, mut cx) = open(path, cx);
    for (selector, preview) in [
        ("file-mode-read", true),
        ("file-mode-edit", false),
        ("file-mode-edit", false),
        ("file-mode-read", true),
    ] {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let point = cx.debug_bounds(selector).unwrap().center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(file.read_with(&cx, |view, _| view.preview), preview);
        assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), source);
        assert!(!file.read_with(&cx, |view, _| view.dirty));
    }
    for (selector, info) in
        [("file-info", true), ("file-info", true), ("file-toggle-outline", false)]
    {
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let bounds = cx.debug_bounds(selector).unwrap();
        assert!(bounds.size.height >= px(reader_presentation::PANEL_HEADER_HEIGHT - 1.0));
        assert!(bounds.size.width >= px(48.0));
        // Click the padding above the text, not only the label center.
        let point = gpui::point(bounds.center().x, bounds.origin.y + px(4.0));
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(file.read_with(&cx, |view, _| view.show_details));
        assert_eq!(file.read_with(&cx, |view, _| view.info), info);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("file-details-indicator").unwrap().size.height, px(2.0));
        if info {
            for action in ["info-copy-path", "info-copy-name", "info-reveal", "info-open"] {
                assert!(cx.debug_bounds(action).unwrap().size.height >= px(32.0));
            }
        }
    }
}

#[gpui::test]
fn code_actions_copy_raw_source_and_search_languages(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("code.md");
    let source = "```text\nlet x = 42;\n```\n";
    std::fs::write(&path, source).unwrap();
    let (file, mut cx) = open(path, cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let block = cx.debug_bounds("markdown-preview-block-0").unwrap();
    let surface = cx.debug_bounds("pebrel-code-block").unwrap();
    let text = cx.debug_bounds("pebrel-code-text").unwrap();
    let language = cx.debug_bounds("markdown-language-picker").unwrap();
    assert_eq!(
        surface.size.height,
        text.size.height + px(28.0),
        "copy must not reserve a layout row"
    );
    assert!(language.origin.y >= text.bottom(), "language belongs below the code");
    assert!(f32::from(surface.right() - language.right()).abs() < 1.0, "language aligns right");
    cx.simulate_mouse_move(block.center(), None, gpui::Modifiers::default());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let copy = cx.debug_bounds("markdown-copy-code").unwrap().center();
    cx.simulate_mouse_move(copy, None, gpui::Modifiers::default());
    cx.simulate_mouse_down(copy, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(copy, MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();
    let copied = cx.read_from_clipboard().unwrap().text().unwrap();
    assert_eq!(copied.trim_end_matches('\n'), "let x = 42;");
    let picker = cx.debug_bounds("markdown-language-picker").unwrap().center();
    cx.simulate_mouse_down(picker, MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(picker, MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_input("rust");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(file.read_with(&cx, |view, _| view.outline.block_source(0).starts_with("```rust")));
    assert_eq!(file.read_with(&cx, |view, cx| view.input.read(cx).value().to_string()), source);
}

#[gpui::test]
fn long_code_line_does_not_expand_the_reader_or_reserve_a_toolbar(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("long-code.md");
    std::fs::write(&path, format!("```text\n{}\n```\n", "x".repeat(2000))).unwrap();
    let (_, mut cx) = open(path, cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let surface = cx.debug_bounds("pebrel-code-block").unwrap();
    let viewport = cx.debug_bounds("markdown-preview-viewport").unwrap();
    assert!(surface.size.width <= px(reader_presentation::PAGE_WIDTH));
    assert!(surface.right() <= viewport.right());
    assert!(
        surface.size.height < px(120.0),
        "one logical code line must scroll horizontally, not grow into a wrapped wall"
    );
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

#[gpui::test]
fn markdown_preview_and_outline_keep_visible_scrollbar_hosts(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("outline.md");
    std::fs::write(
        &path,
        (0..40).map(|index| format!("## Heading {index}\n\nBody {index}\n\n")).collect::<String>(),
    )
    .unwrap();
    let (_, mut cx) = open(path, cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    assert_eq!(cx.debug_bounds("markdown-preview-scrollbar").unwrap().size.width, px(16.0));
    assert_eq!(cx.debug_bounds("markdown-outline-scrollbar").unwrap().size.width, px(16.0));
}
