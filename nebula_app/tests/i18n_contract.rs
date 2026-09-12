#[path = "../build/i18n.rs"]
mod catalog_builder;
#[path = "../src/i18n/mod.rs"]
mod i18n;
#[path = "../src/display/side_panel/notice.rs"]
mod panel_notice;
#[path = "../src/gpui_shell/workspace/vcs_panel/relative_time.rs"]
mod vcs_relative_time;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn record_allocation() {
    let _ = TRACK_ALLOCATIONS.try_with(|tracking| {
        if tracking.get() {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn first_and_repeated_translation_lookups_allocate_nothing() {
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|tracking| tracking.set(true));
    for language in i18n::UiLanguage::ALL {
        for _ in 0..1_000 {
            black_box(language.text(black_box(i18n::Message::SettingsSidebarNetwork)));
            black_box(language.text(black_box(i18n::Message::VcsChanges)));
            black_box(language.text(black_box(i18n::Message::VcsCommitPlaceholder)));
            black_box(language.tr(black_box("settings.sidebar.network")));
            black_box(language.pick(black_box("网络"), black_box("Network")));
            black_box(language.pick(black_box("新文案"), black_box("Unmigrated text")));
        }
    }
    TRACK_ALLOCATIONS.with(|tracking| tracking.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn embedded_translations_stay_within_the_initial_payload_budget() {
    assert!(i18n::TRANSLATED_BYTES < 256 * 1024);
    assert!(i18n::MESSAGE_COUNT >= 200);
}

#[test]
fn vcs_messages_follow_resolved_english_and_fall_back_for_partial_locales() {
    let english = i18n::UiLanguage::for_locale(Some("en-GB"));
    assert_eq!(english.tr("vcs.changes"), "Changes");
    assert_eq!(english.tr("vcs.commit_placeholder"), "Commit message...");
    assert_eq!(i18n::UiLanguage::ZhCn.tr("vcs.changes"), "变更");
    assert_eq!(i18n::UiLanguage::FrFr.tr("vcs.changes"), "Changes");
    assert_eq!(english.tr_args("vcs.refresh_status", &[("vcs", "Git")]), "Refresh Git status");
}
