//! SFTP 右侧抽屉的状态、几何和绘制；传输协议留在 `ssh_sftp`。

use std::time::Instant;

use super::*;
use crate::ssh_sftp::{SftpController, SftpEntry, SftpEntryKind, SftpPhase, SftpSnapshot};

#[derive(Clone, Debug)]
enum EditorKind {
    Filter,
    Path,
    CreateDirectory,
    Rename(SftpEntry),
}

#[derive(Clone, Debug)]
struct Editor {
    kind: EditorKind,
    text: String,
    selection: super::text_input::SelectAllState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SftpHit {
    #[default]
    None,
    Close,
    Path,
    Filter,
    Row(usize),
    Cancel,
    Inside,
}

pub struct SftpPanel {
    controller: SftpController,
    pub filter: String,
    pub selected: Option<String>,
    pub scroll: usize,
    pub hover: SftpHit,
    editor: Option<Editor>,
    editor_error: Option<String>,
    last_click: Option<(String, Instant)>,
}

impl SftpPanel {
    pub fn new(controller: SftpController) -> Self {
        Self {
            controller,
            filter: String::new(),
            selected: None,
            scroll: 0,
            hover: SftpHit::None,
            editor: None,
            editor_error: None,
            last_click: None,
        }
    }

    pub fn snapshot(&self) -> SftpSnapshot {
        self.controller.snapshot()
    }

    pub fn visible_entries(&self) -> Vec<SftpEntry> {
        let snapshot = self.snapshot();
        let mut entries = filtered_entries(&snapshot.entries, &self.filter);
        if snapshot.path != "/" && self.filter.trim().is_empty() {
            entries.insert(0, SftpEntry {
                name: "..".to_owned(),
                path: super::super::ssh_sftp::normalize_remote_path(&snapshot.path, ".."),
                kind: SftpEntryKind::Directory,
                size: 0,
                modified: 0,
                permissions: String::new(),
                is_parent: true,
            });
        }
        entries
    }

    pub fn visible_entry(&self, index: usize) -> Option<SftpEntry> {
        self.visible_entries().into_iter().nth(self.scroll + index)
    }

    pub fn set_hover(&mut self, hit: SftpHit) -> bool {
        if self.hover == hit {
            return false;
        }
        self.hover = hit;
        true
    }

    pub fn scroll_by(&mut self, delta: i32, visible_rows: usize) {
        let max = self.visible_entries().len().saturating_sub(visible_rows);
        self.scroll = (self.scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
    }

    pub fn refresh(&self) {
        self.controller.refresh(self.snapshot().path);
    }

    pub fn navigate(&mut self, entry: &SftpEntry) -> bool {
        if matches!(entry.kind, SftpEntryKind::Directory | SftpEntryKind::Symlink) {
            self.selected = None;
            self.scroll = 0;
            self.controller.refresh(entry.path.clone());
            true
        } else {
            false
        }
    }

    pub fn select_row(&mut self, index: usize) -> Option<(SftpEntry, bool)> {
        let entry = self.visible_entry(index)?;
        self.selected = Some(entry.path.clone());
        let now = Instant::now();
        let double = self.last_click.as_ref().is_some_and(|(path, at)| {
            *path == entry.path && at.elapsed() < std::time::Duration::from_millis(400)
        });
        self.last_click = if double { None } else { Some((entry.path.clone(), now)) };
        Some((entry, double))
    }

    pub fn begin_filter(&mut self) {
        self.begin_editor(EditorKind::Filter, self.filter.clone());
    }

    pub fn begin_path(&mut self) {
        self.begin_editor(EditorKind::Path, self.snapshot().path);
    }

    pub fn begin_create_directory(&mut self) {
        self.begin_editor(EditorKind::CreateDirectory, String::new());
    }

    pub fn begin_rename(&mut self, entry: SftpEntry) {
        if entry.is_parent {
            return;
        }
        let name = entry.name.clone();
        self.begin_editor(EditorKind::Rename(entry), name);
    }

    fn begin_editor(&mut self, kind: EditorKind, text: String) {
        let mut selection = super::text_input::SelectAllState::default();
        selection.select(&text);
        self.editor = Some(Editor { kind, text, selection });
        self.editor_error = None;
    }

    pub fn editor_active(&self) -> bool {
        self.editor.is_some()
    }

    pub fn editor_insert(&mut self, text: &str) {
        let Some(editor) = self.editor.as_mut() else { return };
        let text: String = text.chars().filter(|character| !character.is_control()).collect();
        editor.selection.insert(&mut editor.text, &text);
        if matches!(editor.kind, EditorKind::Filter) {
            self.filter.clone_from(&editor.text);
            self.scroll = 0;
            self.selected = None;
        }
    }

    pub fn editor_backspace(&mut self) {
        let Some(editor) = self.editor.as_mut() else { return };
        editor.selection.backspace(&mut editor.text);
        if matches!(editor.kind, EditorKind::Filter) {
            self.filter.clone_from(&editor.text);
            self.scroll = 0;
            self.selected = None;
        }
    }

    pub fn editor_select_all(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.selection.select(&editor.text);
        }
    }

    pub fn editor_selected_text(&self) -> Option<String> {
        self.editor.as_ref().and_then(|editor| editor.selection.selected_text(&editor.text))
    }

    pub fn editor_cancel(&mut self) {
        if self.editor.as_ref().is_some_and(|editor| matches!(editor.kind, EditorKind::Filter)) {
            self.filter.clear();
            self.scroll = 0;
        }
        self.editor = None;
        self.editor_error = None;
    }

    pub fn editor_unfocus(&mut self) {
        if self.editor.as_ref().is_some_and(|editor| matches!(editor.kind, EditorKind::Filter)) {
            self.editor = None;
        } else {
            self.editor_cancel();
        }
    }

    pub fn editor_submit(&mut self) -> Result<(), String> {
        let Some(editor) = self.editor.take() else { return Ok(()) };
        let result = match editor.kind.clone() {
            EditorKind::Filter => {
                self.filter.clone_from(&editor.text);
                Ok(())
            },
            EditorKind::Path => {
                self.scroll = 0;
                self.selected = None;
                self.controller.refresh(editor.text.clone());
                Ok(())
            },
            EditorKind::CreateDirectory => self.controller.create_directory(editor.text.trim()),
            EditorKind::Rename(entry) => self.controller.rename(entry, editor.text.trim()),
        };
        if let Err(err) = result.as_ref() {
            self.editor_error = Some(err.clone());
            self.editor = Some(editor);
        } else {
            self.editor_error = None;
        }
        result
    }

    pub fn editor_view(&self) -> Option<(&str, bool, &'static str)> {
        let editor = self.editor.as_ref()?;
        let hint = match editor.kind {
            EditorKind::Filter => "筛选远端文件",
            EditorKind::Path => "输入远端路径",
            EditorKind::CreateDirectory => "新文件夹名称",
            EditorKind::Rename(_) => "输入新名称",
        };
        Some((&editor.text, editor.selection.is_selected(), hint))
    }

    pub fn upload_paths(&self, paths: Vec<std::path::PathBuf>) {
        self.controller.upload_paths(paths);
    }

    pub fn download(&self, entry: SftpEntry, directory: std::path::PathBuf) {
        if entry.is_parent {
            return;
        }
        self.controller.download(entry, directory);
    }

    pub fn delete(&self, entry: SftpEntry) {
        if entry.is_parent {
            return;
        }
        self.controller.delete(entry);
    }

    pub fn cancel_transfer(&self) {
        self.controller.cancel();
    }
}

fn filtered_entries(entries: &[SftpEntry], query: &str) -> Vec<SftpEntry> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| query.is_empty() || entry.name.to_lowercase().contains(&query))
        .cloned()
        .collect()
}

#[derive(Clone, Debug)]
pub struct SftpLayout {
    pub panel: (f32, f32, f32, f32),
    pub close: (f32, f32, f32, f32),
    pub path: (f32, f32, f32, f32),
    pub filter: (f32, f32, f32, f32),
    pub list_y: f32,
    pub row_h: f32,
    pub max_rows: usize,
    pub cancel: (f32, f32, f32, f32),
}

pub fn layout(base: &side_panel::PanelLayout, scale: f32) -> SftpLayout {
    let s = |value: f32| value * scale;
    let (x, y, width, height) = base.panel;
    let close = (x + width - s(36.0), y + s(6.0), s(28.0), s(28.0));
    let content_x = x + s(12.0);
    let content_w = width - s(24.0);
    let path = (content_x, y + s(46.0), content_w, s(34.0));
    let filter = (content_x, path.1 + path.3 + s(8.0), content_w, s(34.0));
    let list_y = filter.1 + filter.3 + s(10.0);
    let row_h = s(34.0);
    let cancel = (x + s(12.0), y + height - s(38.0), width - s(24.0), s(28.0));
    let max_rows = ((cancel.1 - s(8.0) - list_y) / row_h).max(0.0) as usize;
    SftpLayout { panel: base.panel, close, path, filter, list_y, row_h, max_rows, cancel }
}

fn contains(rect: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

pub fn hit_test(layout: &SftpLayout, working: bool, x: f32, y: f32) -> SftpHit {
    if !contains(layout.panel, x, y) {
        return SftpHit::None;
    }
    if contains(layout.close, x, y) {
        return SftpHit::Close;
    }
    if contains(layout.path, x, y) {
        return SftpHit::Path;
    }
    if contains(layout.filter, x, y) {
        return SftpHit::Filter;
    }
    if working && contains(layout.cancel, x, y) {
        return SftpHit::Cancel;
    }
    if y >= layout.list_y {
        let row = ((y - layout.list_y) / layout.row_h) as usize;
        if row < layout.max_rows {
            return SftpHit::Row(row);
        }
    }
    SftpHit::Inside
}

pub(super) fn push_quads(
    panel: &SftpPanel,
    layout: &SftpLayout,
    theme: &NebulaTheme,
    quads: &mut Vec<UiQuad>,
    scale: f32,
    cell_w: f32,
) {
    let s = |value: f32| value * scale;
    let palette = theme.palette();
    let skin = theme.skin();
    let radius = s(UI_CORNER_RADIUS_LOGICAL);
    let (x, y, width, height) = layout.panel;
    quads.push(UiQuad::solid(x, y, width, height, radius, palette.panel));

    for (rect, focused) in [
        (
            layout.path,
            panel.editor.as_ref().is_some_and(|editor| matches!(editor.kind, EditorKind::Path)),
        ),
        (
            layout.filter,
            panel.editor.as_ref().is_some_and(|editor| !matches!(editor.kind, EditorKind::Path)),
        ),
    ] {
        if focused {
            quads.push(UiQuad::solid(
                rect.0 - s(1.0),
                rect.1 - s(1.0),
                rect.2 + s(2.0),
                rect.3 + s(2.0),
                s(7.0),
                Rgba::new(skin.accent.r, skin.accent.g, skin.accent.b, 190),
            ));
        }
        quads.push(UiQuad::solid(rect.0, rect.1, rect.2, rect.3, s(6.0), skin.input));
    }
    if let Some((text, true, _)) = panel.editor_view() {
        let rect = if panel
            .editor
            .as_ref()
            .is_some_and(|editor| matches!(editor.kind, EditorKind::Path))
        {
            layout.path
        } else {
            layout.filter
        };
        let columns: usize = text.chars().map(|character| character.width().unwrap_or(0)).sum();
        let width = (columns as f32 * cell_w).min(rect.2 - s(16.0));
        quads.push(UiQuad::solid(
            rect.0 + s(6.0),
            rect.1 + s(6.0),
            width + s(4.0),
            rect.3 - s(12.0),
            s(4.0),
            skin.accent_soft,
        ));
    }
    if panel.hover == SftpHit::Close {
        quads.push(UiQuad::solid(
            layout.close.0,
            layout.close.1,
            layout.close.2,
            layout.close.3,
            s(6.0),
            skin.hover,
        ));
    }

    if let SftpHit::Row(index) = panel.hover {
        let row_y = layout.list_y + index as f32 * layout.row_h;
        quads.push(UiQuad::solid(
            x + s(8.0),
            row_y,
            width - s(16.0),
            layout.row_h - s(3.0),
            s(6.0),
            skin.hover,
        ));
    }
    if let Some(selected) = panel.selected.as_ref() {
        if let Some(index) = panel
            .visible_entries()
            .iter()
            .skip(panel.scroll)
            .take(layout.max_rows)
            .position(|entry| &entry.path == selected)
        {
            let row_y = layout.list_y + index as f32 * layout.row_h;
            quads.push(UiQuad::solid(
                x + s(8.0),
                row_y,
                width - s(16.0),
                layout.row_h - s(3.0),
                s(6.0),
                skin.accent_soft,
            ));
        }
    }

    let snapshot = panel.snapshot();
    if snapshot.phase == SftpPhase::Working {
        quads.push(UiQuad::solid(
            layout.cancel.0,
            layout.cancel.1,
            layout.cancel.2,
            layout.cancel.3,
            s(6.0),
            skin.input,
        ));
        if let Some(progress) = snapshot.progress {
            quads.push(UiQuad::solid(
                layout.cancel.0,
                layout.cancel.1,
                layout.cancel.2 * progress.fraction(),
                layout.cancel.3,
                s(6.0),
                skin.accent_soft,
            ));
        }
    }
}

pub(super) fn draw_text(
    panel: &SftpPanel,
    layout: &SftpLayout,
    theme: &NebulaTheme,
    ls: super::side_panel::LsColors,
    renderer: &mut Renderer,
    glyph_cache: &mut GlyphCache,
    size: &SizeInfo,
    scale: f32,
) {
    let s = |value: f32| value * scale;
    let skin = theme.skin();
    let cell_w = size.cell_width();
    let cell_h = size.cell_height();
    let snapshot = panel.snapshot();
    let text_y = |rect: (f32, f32, f32, f32)| rect.1 + (rect.3 - cell_h) * 0.5;

    renderer.draw_chrome_text(
        size,
        layout.panel.0 + s(14.0),
        layout.panel.1 + s(10.0),
        skin.ink_strong,
        "SFTP",
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        layout.panel.0 + s(58.0),
        layout.panel.1 + s(10.0),
        skin.ink_dim,
        &super::truncate_tab_label(&snapshot.destination, 19),
        glyph_cache,
    );
    renderer.draw_chrome_text(
        size,
        layout.close.0 + (layout.close.2 - cell_w) * 0.5,
        text_y(layout.close),
        skin.ink_dim,
        "×",
        glyph_cache,
    );

    let path_editor =
        panel.editor.as_ref().filter(|editor| matches!(editor.kind, EditorKind::Path));
    let path_text = path_editor.map(|editor| editor.text.as_str()).unwrap_or(&snapshot.path);
    let path_shown = if path_editor
        .is_some_and(|editor| !editor.selection.is_selected() && super::caret_blink_on())
    {
        format!("{path_text}▏")
    } else {
        path_text.to_owned()
    };
    renderer.draw_chrome_text(
        size,
        layout.path.0 + s(8.0),
        text_y(layout.path),
        skin.ink_strong,
        &super::truncate_tab_label(&path_shown, 30),
        glyph_cache,
    );

    let filter_editor =
        panel.editor.as_ref().filter(|editor| !matches!(editor.kind, EditorKind::Path));
    let filter_text = filter_editor.map(|editor| editor.text.as_str()).unwrap_or(&panel.filter);
    let filter_hint = filter_editor.map_or("筛选文件", |editor| match editor.kind {
        EditorKind::Filter => "筛选文件",
        EditorKind::CreateDirectory => "新文件夹名称",
        EditorKind::Rename(_) => "输入新名称",
        EditorKind::Path => "远端路径",
    });
    let filter_shown = if filter_editor
        .is_some_and(|editor| !editor.selection.is_selected() && super::caret_blink_on())
    {
        format!("{filter_text}▏")
    } else {
        filter_text.to_owned()
    };
    renderer.draw_chrome_text(
        size,
        layout.filter.0 + s(8.0),
        text_y(layout.filter),
        if filter_text.is_empty() { skin.ink_faint } else { skin.ink_strong },
        if filter_shown.is_empty() { filter_hint } else { &filter_shown },
        glyph_cache,
    );

    if let Some(error) = panel.editor_error.as_deref().or(snapshot.error.as_deref()) {
        let user_error = crate::ux::UserFacingError::new(
            "远端文件操作失败",
            error,
            "检查网络和目录权限，然后点击刷新重试。",
        )
        .retry(crate::ux::RetryAction::Retry);
        renderer.draw_chrome_text(
            size,
            layout.panel.0 + s(14.0),
            layout.list_y,
            Rgb::new(skin.danger.r, skin.danger.g, skin.danger.b),
            &super::truncate_tab_label(&user_error.title, 30),
            glyph_cache,
        );
        renderer.draw_chrome_text(
            size,
            layout.panel.0 + s(14.0),
            layout.list_y + s(20.0),
            skin.ink_dim,
            &super::truncate_tab_label(&format!("原因：{}", user_error.cause), 30),
            glyph_cache,
        );
        renderer.draw_chrome_text(
            size,
            layout.panel.0 + s(14.0),
            layout.list_y + s(40.0),
            skin.accent,
            "建议：检查权限后点击刷新",
            glyph_cache,
        );
    } else if matches!(snapshot.phase, SftpPhase::Connecting | SftpPhase::Loading) {
        renderer.draw_chrome_text(
            size,
            layout.panel.0 + s(14.0),
            layout.list_y,
            skin.ink_dim,
            "正在读取远端目录…",
            glyph_cache,
        );
    } else {
        for (index, entry) in
            panel.visible_entries().iter().skip(panel.scroll).take(layout.max_rows).enumerate()
        {
            let y = layout.list_y
                + index as f32 * layout.row_h
                + (layout.row_h - cell_h) * 0.5;
            // 与本地 Files 面板共用同一套 Codicon 与 ANSI 颜色。远端列表如果
            // 单独使用三角形/圆点，会让同一个“文件”入口看起来像两套产品。
            let (chevron, icon, icon_ink, name_ink) = match entry.kind {
                SftpEntryKind::Directory => (
                    Some(super::side_panel::ICON_CHEVRON_RIGHT),
                    super::side_panel::ICON_FOLDER,
                    ls.dir,
                    ls.dir,
                ),
                SftpEntryKind::Symlink => (
                    None,
                    "\u{ea71}", // codicon-link
                    skin.ink_dim,
                    skin.ink_strong,
                ),
                SftpEntryKind::File => (
                    None,
                    super::side_panel::file_type_icon(&entry.name),
                    skin.ink_dim,
                    skin.ink_strong,
                ),
            };
            let icon_x = layout.panel.0 + s(14.0);
            if let Some(chevron) = chevron {
                renderer.draw_chrome_text(
                    size,
                    icon_x,
                    y,
                    skin.ink_faint,
                    chevron,
                    glyph_cache,
                );
            }
            let file_icon_x = icon_x + cell_w * 1.9;
            renderer.draw_chrome_text(
                size,
                file_icon_x,
                y,
                icon_ink,
                icon,
                glyph_cache,
            );
            let name_x = file_icon_x + cell_w * 2.2;
            renderer.draw_chrome_text(
                size,
                name_x,
                y,
                name_ink,
                &super::truncate_tab_label(&entry.name, 27),
                glyph_cache,
            );
        }
        if panel.visible_entries().is_empty() {
            let empty = if panel.filter.is_empty() {
                crate::ux::EmptyState::new(
                    "此目录为空",
                    "远端目录中还没有文件或子目录。",
                    "使用上方上传按钮添加第一个文件。",
                )
            } else {
                crate::ux::EmptyState::new(
                    "没有匹配文件",
                    "当前筛选条件未匹配任何远端条目。",
                    "修改筛选词或清空筛选框。",
                )
            };
            renderer.draw_chrome_text(
                size,
                layout.panel.0 + s(14.0),
                layout.list_y,
                skin.ink_faint,
                &empty.title,
                glyph_cache,
            );
            renderer.draw_chrome_text(
                size,
                layout.panel.0 + s(14.0),
                layout.list_y + s(20.0),
                skin.ink_dim,
                &super::truncate_tab_label(&empty.reason, 31),
                glyph_cache,
            );
            renderer.draw_chrome_text(
                size,
                layout.panel.0 + s(14.0),
                layout.list_y + s(40.0),
                skin.accent,
                &super::truncate_tab_label(&empty.action, 31),
                glyph_cache,
            );
        }
    }

    if snapshot.phase == SftpPhase::Working {
        let label = snapshot
            .progress
            .as_ref()
            .map_or("正在处理 · 点击取消", |progress| progress.label.as_str());
        renderer.draw_chrome_text(
            size,
            layout.cancel.0 + s(8.0),
            text_y(layout.cancel),
            skin.ink_strong,
            &super::truncate_tab_label(label, 24),
            glyph_cache,
        );
        renderer.draw_chrome_text(
            size,
            layout.cancel.0 + layout.cancel.2 - s(42.0),
            text_y(layout.cancel),
            skin.ink_dim,
            "取消",
            glyph_cache,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> SftpEntry {
        SftpEntry {
            name: name.to_owned(),
            path: format!("/{name}"),
            kind: SftpEntryKind::File,
            size: 0,
            modified: 0,
            permissions: "-rw-r--r--".to_owned(),
            is_parent: false,
        }
    }

    #[test]
    fn filter_is_case_insensitive_without_reordering_entries() {
        let entries = vec![entry("Alpha.txt"), entry("beta.log"), entry("alphabet.md")];
        let filtered = filtered_entries(&entries, "ALPHA");
        assert_eq!(
            filtered.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            vec!["Alpha.txt", "alphabet.md"]
        );
    }

    #[test]
    fn filter_hit_uses_the_drawn_geometry_without_a_toolbar_band() {
        let base = side_panel::panel_layout(1000.0, 800.0, 0.0, 0.0, 1.0, 1.0);
        let layout = layout(&base, 1.0);
        let rect = layout.filter;
        assert_eq!(
            hit_test(&layout, false, rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5),
            SftpHit::Filter
        );
    }
}
