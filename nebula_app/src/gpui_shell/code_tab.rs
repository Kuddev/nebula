//! 代码文件 Tab：普通文件只读查看；Git 冲突使用同一 Tab 体系打开三栏合并器。
//!
//! 普通代码继续走组件库的虚拟化 code editor。冲突页遵循 IntelliJ 一类合并器
//! 的空间语义：左侧当前版本、中间可编辑结果、右侧传入版本；外侧两栏只读，
//! 应用结果后写回工作树并 `git add`，不会另开浮窗或挤占 VCS 抽屉。

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div, px, relative,
};

use crate::display::side_panel::GitLocation;
use crate::gpui_shell::prelude::*;

/// 一次性读入的上限。行级虚拟化解决的是渲染成本，解析/塑形仍随内容量
/// 增长；8MB 已覆盖常见源码文件，超限的普通文件截断，冲突文件则拒绝写回。
const MAX_CODE_BYTES: usize = 8 * 1024 * 1024;

/// 扩展名 -> tree-sitter 语言 id（组件库 highlighter 的注册名）。
fn language_for_extension(extension: &str) -> Option<&'static str> {
    Some(match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "bash",
        "ps1" | "psm1" | "psd1" => "powershell",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "zig" => "zig",
        "cmake" => "cmake",
        "dockerfile" => "dockerfile",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "ini" | "cfg" | "conf" => "ini",
        "vim" => "vim",
        "xml" => "xml",
        "json" | "jsonl" | "ndjson" => "json",
        "txt" | "log" | "text" => "text",
        _ => return None,
    })
}

fn language_for_path(path: &str) -> &'static str {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(language_for_extension)
        .unwrap_or("text")
}

/// 文件树双击是否进代码查看 tab（与 markdown/图片路由并列）。
pub fn viewable_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| language_for_extension(extension).is_some())
}

pub enum CodeTabViewEvent {
    GitConflictResolved,
}

impl EventEmitter<CodeTabViewEvent> for CodeTabView {}

#[derive(Clone)]
struct MergeKey {
    location: GitLocation,
    relative_path: String,
}

impl MergeKey {
    fn matches(&self, location: &GitLocation, relative_path: &str) -> bool {
        &self.location == location && self.relative_path == relative_path
    }

    fn display_path(&self) -> String {
        match &self.location {
            GitLocation::Local { root } => root.join(&self.relative_path).display().to_string(),
            GitLocation::Wsl { distro, root } => {
                format!("{distro}:{}", join_guest_path(root, &self.relative_path))
            },
        }
    }
}

#[derive(Clone)]
enum MergeState {
    Loading,
    Ready,
    Saving,
    Resolved,
    Error(String),
}

struct MergeEditor {
    key: MergeKey,
    ours: Entity<InputState>,
    theirs: Entity<InputState>,
    ours_text: String,
    theirs_text: String,
    ours_missing: bool,
    theirs_missing: bool,
    state: MergeState,
}

struct ConflictDocument {
    ours: Option<String>,
    theirs: Option<String>,
    result: String,
}

pub struct CodeTabView {
    pub path: PathBuf,
    pub title: String,
    /// 普通文件的正文；冲突页中它就是唯一可编辑的中栏合并结果。
    input: Entity<InputState>,
    notice: Option<String>,
    lines: usize,
    merge: Option<MergeEditor>,
}

impl CodeTabView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let language = language_for_path(&path.to_string_lossy());
        let input = code_input(language, window, cx);
        let mut this = Self { path, title, input, notice: None, lines: 0, merge: None };
        this.reload(window, cx);
        this
    }

    pub fn new_git_merge(
        location: GitLocation,
        relative_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = Path::new(&relative_path)
            .file_name()
            .map(|name| format!("合并 · {}", name.to_string_lossy()))
            .unwrap_or_else(|| format!("合并 · {relative_path}"));
        let language = language_for_path(&relative_path);
        let input = code_input(language, window, cx);
        let ours = code_input(language, window, cx);
        let theirs = code_input(language, window, cx);
        // `path` 只用于现有 tab 图标/标题合同；冲突 tab 的真实唯一键是 MergeKey。
        let path = PathBuf::from(&relative_path);
        let mut this = Self {
            path,
            title,
            input,
            notice: None,
            lines: 0,
            merge: Some(MergeEditor {
                key: MergeKey { location, relative_path },
                ours,
                theirs,
                ours_text: String::new(),
                theirs_text: String::new(),
                ours_missing: false,
                theirs_missing: false,
                state: MergeState::Loading,
            }),
        };
        this.reload_git_merge(window, cx);
        this
    }

    pub fn is_regular_path(&self, path: &Path) -> bool {
        self.merge.is_none() && self.path == path
    }

    pub fn matches_git_merge(&self, location: &GitLocation, relative_path: &str) -> bool {
        self.merge.as_ref().is_some_and(|merge| merge.key.matches(location, relative_path))
    }

    /// 重新读盘（文件树再次双击同一路径时宿主调用）。
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.merge.is_some() {
            self.reload_git_merge(window, cx);
            return;
        }
        let (text, notice) = match std::fs::read(&self.path) {
            Ok(bytes) => {
                let truncated = bytes.len() > MAX_CODE_BYTES;
                let slice = if truncated { &bytes[..MAX_CODE_BYTES] } else { &bytes[..] };
                let text = String::from_utf8_lossy(slice).into_owned();
                let notice = truncated.then(|| {
                    format!("文件超过 {} MB，仅显示开头部分", MAX_CODE_BYTES / 1024 / 1024)
                });
                (text, notice)
            },
            Err(error) => {
                (String::new(), Some(format!("无法读取 {}: {error}", self.path.display())))
            },
        };
        self.lines = text.lines().count();
        self.notice = notice;
        self.input.update(cx, |input, cx| input.set_value(text, window, cx));
        cx.notify();
    }

    pub fn reload_git_merge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(merge) = self.merge.as_mut() else { return };
        merge.state = MergeState::Loading;
        self.notice = None;
        let key = merge.key.clone();
        let task = cx.background_executor().spawn(async move { load_conflict(&key) });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |view, cx| {
                    view.apply_loaded_conflict(loaded, window, cx);
                });
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_loaded_conflict(
        &mut self,
        loaded: Result<ConflictDocument, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(merge) = self.merge.as_mut() else { return };
        match loaded {
            Ok(document) => {
                merge.ours_missing = document.ours.is_none();
                merge.theirs_missing = document.theirs.is_none();
                merge.ours_text = document.ours.unwrap_or_default();
                merge.theirs_text = document.theirs.unwrap_or_default();
                merge
                    .ours
                    .update(cx, |input, cx| input.set_value(merge.ours_text.clone(), window, cx));
                merge
                    .theirs
                    .update(cx, |input, cx| input.set_value(merge.theirs_text.clone(), window, cx));
                self.lines = document.result.lines().count();
                self.input.update(cx, |input, cx| input.set_value(document.result, window, cx));
                merge.state = MergeState::Ready;
            },
            Err(error) => merge.state = MergeState::Error(error),
        }
        cx.notify();
    }

    fn adopt_ours(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(merge) = self.merge.as_ref() else { return };
        if !matches!(merge.state, MergeState::Ready) {
            return;
        }
        let value = merge.ours_text.clone();
        self.lines = value.lines().count();
        self.input.update(cx, |input, cx| input.set_value(value, window, cx));
        cx.notify();
    }

    fn adopt_theirs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(merge) = self.merge.as_ref() else { return };
        if !matches!(merge.state, MergeState::Ready) {
            return;
        }
        let value = merge.theirs_text.clone();
        self.lines = value.lines().count();
        self.input.update(cx, |input, cx| input.set_value(value, window, cx));
        cx.notify();
    }

    fn save_git_merge(&mut self, cx: &mut Context<Self>) {
        let Some(merge) = self.merge.as_mut() else { return };
        if !matches!(merge.state, MergeState::Ready) {
            return;
        }
        let result = self.input.read(cx).value().to_string();
        if contains_conflict_markers(&result) {
            self.notice = Some("合并结果中仍有冲突标记，请处理后再应用".to_owned());
            cx.notify();
            return;
        }
        merge.state = MergeState::Saving;
        self.notice = None;
        let key = merge.key.clone();
        let task =
            cx.background_executor().spawn(async move { write_conflict_result(&key, result) });
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            let _ = this.update(cx, |view, cx| {
                let Some(merge) = view.merge.as_mut() else { return };
                match saved {
                    Ok(()) => {
                        merge.state = MergeState::Resolved;
                        cx.emit(CodeTabViewEvent::GitConflictResolved);
                    },
                    Err(error) => merge.state = MergeState::Error(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn render_file(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let warning = theme.warning;
        let mono_family = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;
        let path_label: SharedString = self.path.display().to_string().into();
        let copy_path = self.path.display().to_string();
        let meta: SharedString = format!("{} 行", self.lines).into();

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(32.0))
                    .flex_shrink_0()
                    .px(px(12.0))
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::File).xsmall().text_color(muted))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono_family.clone())
                                    .text_size(px(12.0))
                                    .text_color(muted)
                                    .child(path_label),
                            ),
                    )
                    .child(div().text_size(px(11.0)).text_color(muted).child(meta))
                    .child(
                        Button::new("code-copy-path")
                            .icon(IconName::Copy)
                            .ghost()
                            .xsmall()
                            .tooltip("复制文件路径")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()));
                            })),
                    )
                    .child(
                        Button::new("code-reload")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("重新读取文件")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reload(window, cx);
                            })),
                    ),
            )
            .when_some(self.notice.clone(), |root, notice| {
                root.child(
                    h_flex()
                        .min_h(px(28.0))
                        .flex_shrink_0()
                        .px(px(12.0))
                        .py_1()
                        .border_b_1()
                        .border_color(border)
                        .text_size(px(11.0))
                        .text_color(warning)
                        .child(notice),
                )
            })
            .child(div().flex_1().min_h_0().child(editor(
                &self.input,
                false,
                mono_family,
                mono_size,
            )))
            .into_any_element()
    }

    fn render_merge(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let merge = self.merge.as_ref().expect("merge renderer requires merge state");
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let mono_family = theme.mono_font_family.clone();
        let mono_size = theme.mono_font_size;
        let display_path = merge.key.display_path();
        let ready = matches!(merge.state, MergeState::Ready);
        let unresolved = contains_conflict_markers(self.input.read(cx).value().as_ref());
        let (status, status_color) = match &merge.state {
            MergeState::Loading => ("正在读取三个 Git 阶段…".to_owned(), muted),
            MergeState::Ready if unresolved => ("仍有冲突标记".to_owned(), theme.warning),
            MergeState::Ready => ("可应用".to_owned(), theme.success),
            MergeState::Saving => ("正在写回并暂存…".to_owned(), muted),
            MergeState::Resolved => ("已写回并暂存".to_owned(), theme.success),
            MergeState::Error(error) => (error.clone(), theme.danger),
        };
        let ours_label =
            if merge.ours_missing { "当前版本（文件不存在）" } else { "当前版本" };
        let theirs_label =
            if merge.theirs_missing { "传入版本（文件不存在）" } else { "传入版本" };

        v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(38.0))
                    .flex_shrink_0()
                    .px(px(12.0))
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Icon::new(Icon::empty())
                            .path(crate::gpui_shell::assets::nav::VCS_CONFLICT)
                            .small()
                            .text_color(theme.warning),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono_family.clone())
                            .text_size(px(12.0))
                            .child(display_path),
                    )
                    .child(
                        div().min_w_0().truncate().text_xs().text_color(status_color).child(status),
                    )
                    .child(
                        Button::new("merge-reload")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("重新读取冲突阶段")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reload_git_merge(window, cx);
                            })),
                    )
                    .child(
                        Button::new("merge-apply")
                            .icon(IconName::Check)
                            .label("应用并暂存")
                            .small()
                            .disabled(!ready || unresolved)
                            .tooltip(if unresolved {
                                "请先清除合并结果中的冲突标记"
                            } else {
                                "写回工作树并执行 git add"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.save_git_merge(cx))),
                    ),
            )
            .when_some(self.notice.clone(), |root, notice| {
                root.child(
                    h_flex()
                        .min_h(px(28.0))
                        .flex_shrink_0()
                        .px(px(12.0))
                        .border_b_1()
                        .border_color(border)
                        .text_xs()
                        .text_color(theme.warning)
                        .child(notice),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(merge_pane(
                        ours_label,
                        &merge.ours,
                        true,
                        Some(
                            Button::new("merge-use-ours")
                                .icon(IconName::ArrowRight)
                                .ghost()
                                .xsmall()
                                .disabled(!ready)
                                .tooltip("用当前版本替换合并结果")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.adopt_ours(window, cx);
                                })),
                        ),
                        mono_family.clone(),
                        mono_size,
                        border,
                        muted,
                    ))
                    .child(merge_pane(
                        "合并结果（可编辑）",
                        &self.input,
                        false,
                        None,
                        mono_family.clone(),
                        mono_size,
                        border,
                        theme.foreground,
                    ))
                    .child(merge_pane(
                        theirs_label,
                        &merge.theirs,
                        true,
                        Some(
                            Button::new("merge-use-theirs")
                                .icon(IconName::ArrowLeft)
                                .ghost()
                                .xsmall()
                                .disabled(!ready)
                                .tooltip("用传入版本替换合并结果")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.adopt_theirs(window, cx);
                                })),
                        ),
                        mono_family,
                        mono_size,
                        border,
                        muted,
                    )),
            )
            .into_any_element()
    }
}

impl Render for CodeTabView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.merge.is_some() { self.render_merge(cx) } else { self.render_file(cx) }
    }
}

fn code_input(
    language: &'static str,
    window: &mut Window,
    cx: &mut Context<CodeTabView>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .code_editor(language)
            .line_number(true)
            .indent_guides(true)
            .soft_wrap(false)
    })
}

fn editor(
    state: &Entity<InputState>,
    read_only: bool,
    family: SharedString,
    size: gpui::Pixels,
) -> Input {
    Input::new(state)
        .h_full()
        .disabled(read_only)
        .bordered(false)
        .focus_bordered(false)
        .rounded(px(0.0))
        .font_family(family)
        .text_size(size)
        .line_height(relative(1.55))
        .px(px(10.0))
        .py(px(8.0))
}

#[allow(clippy::too_many_arguments)]
fn merge_pane(
    label: impl Into<SharedString>,
    state: &Entity<InputState>,
    read_only: bool,
    action: Option<Button>,
    family: SharedString,
    size: gpui::Pixels,
    border: gpui::Hsla,
    label_color: gpui::Hsla,
) -> gpui::AnyElement {
    let label: SharedString = label.into();
    v_flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .border_r_1()
        .border_color(border)
        .child(
            h_flex()
                .h(px(32.0))
                .flex_shrink_0()
                .px(px(10.0))
                .items_center()
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(label_color)
                        .child(label),
                )
                .when_some(action, |header, button| header.child(button)),
        )
        .child(div().flex_1().min_h_0().child(editor(state, read_only, family, size)))
        .into_any_element()
}

fn contains_conflict_markers(text: &str) -> bool {
    let mut ours = false;
    let mut separator = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<<") {
            ours = true;
        } else if ours && line.starts_with("=======") {
            separator = true;
        } else if ours && separator && line.starts_with(">>>>>>>") {
            return true;
        }
    }
    false
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err("冲突文件路径无效".to_owned());
    }
    let local = Path::new(path);
    if local.components().any(|component| {
        matches!(component, Component::Prefix(_) | Component::RootDir | Component::ParentDir)
    }) || path.split('/').any(|part| part == "..")
    {
        return Err("冲突文件不在当前仓库内".to_owned());
    }
    Ok(())
}

fn load_conflict(key: &MergeKey) -> Result<ConflictDocument, String> {
    validate_relative_path(&key.relative_path)?;
    let ours = read_stage(key, 2)?;
    let theirs = read_stage(key, 3)?;
    if ours.is_none() && theirs.is_none() {
        return Err("Git 索引里没有可合并的 :2/:3 阶段".to_owned());
    }
    let result = read_worktree_file(key)?;
    Ok(ConflictDocument {
        ours: ours.map(|bytes| decode_conflict_text(bytes, "当前版本")).transpose()?,
        theirs: theirs.map(|bytes| decode_conflict_text(bytes, "传入版本")).transpose()?,
        result: decode_conflict_text(result, "合并结果")?,
    })
}

fn decode_conflict_text(bytes: Vec<u8>, label: &str) -> Result<String, String> {
    if bytes.len() > MAX_CODE_BYTES {
        return Err(format!(
            "{label}超过 {} MB，不能在三栏编辑器中安全处理",
            MAX_CODE_BYTES / 1024 / 1024
        ));
    }
    if bytes.contains(&0) {
        return Err(format!("{label}是二进制内容，三栏文本合并器无法处理"));
    }
    String::from_utf8(bytes)
        .map_err(|_| format!("{label}不是 UTF-8 文本，已阻止可能破坏编码的写回"))
}

fn read_stage(key: &MergeKey, stage: u8) -> Result<Option<Vec<u8>>, String> {
    let spec = format!(":{stage}:{}", key.relative_path);
    let output = git_command(&key.location, &["show", &spec])?;
    if output.status.success() { Ok(Some(output.stdout)) } else { Ok(None) }
}

fn read_worktree_file(key: &MergeKey) -> Result<Vec<u8>, String> {
    match &key.location {
        GitLocation::Local { root } => match std::fs::read(root.join(&key.relative_path)) {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(format!("读取合并结果失败: {error}")),
        },
        GitLocation::Wsl { distro, root } => {
            let path = join_guest_path(root, &key.relative_path);
            let mut command = Command::new("wsl.exe");
            let output = hidden_command(&mut command)
                .args(["-d", distro, "--", "cat", "--", path.as_str()])
                .output()
                .map_err(|error| format!("无法从 WSL 读取冲突文件: {error}"))?;
            if output.status.success() { Ok(output.stdout) } else { Ok(Vec::new()) }
        },
    }
}

fn write_conflict_result(key: &MergeKey, result: String) -> Result<(), String> {
    validate_relative_path(&key.relative_path)?;
    if result.len() > MAX_CODE_BYTES {
        return Err(format!("合并结果超过 {} MB，已取消写回", MAX_CODE_BYTES / 1024 / 1024));
    }
    match &key.location {
        GitLocation::Local { root } => {
            std::fs::write(root.join(&key.relative_path), result.as_bytes())
                .map_err(|error| format!("写回合并结果失败: {error}"))?
        },
        GitLocation::Wsl { distro, root } => {
            let path = join_guest_path(root, &key.relative_path);
            let mut command = Command::new("wsl.exe");
            let mut child = hidden_command(&mut command)
                .args(["-d", distro, "--", "sh", "-c", "cat > \"$1\"", "nebula", path.as_str()])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| format!("无法向 WSL 写回冲突文件: {error}"))?;
            let write_result = child
                .stdin
                .take()
                .ok_or("无法打开 WSL 写入管道".to_owned())?
                .write_all(result.as_bytes());
            if let Err(error) = write_result {
                let _ = child.kill();
                return Err(format!("写入 WSL 冲突文件失败: {error}"));
            }
            let output =
                child.wait_with_output().map_err(|error| format!("等待 WSL 写回失败: {error}"))?;
            if !output.status.success() {
                return Err(first_command_error(&output.stderr, "WSL 写回失败"));
            }
        },
    }
    let staged = git_command(&key.location, &["add", "--", &key.relative_path])?;
    if !staged.status.success() {
        return Err(first_command_error(&staged.stderr, "git add 失败；文件已写回但尚未暂存"));
    }
    Ok(())
}

fn git_command(location: &GitLocation, args: &[&str]) -> Result<std::process::Output, String> {
    let mut command = match location {
        GitLocation::Local { root } => {
            let mut command = Command::new("git");
            command
                .arg("-c")
                .arg(format!("safe.directory={}", root.display()))
                .arg("--no-optional-locks")
                .arg("-C")
                .arg(root);
            command
        },
        GitLocation::Wsl { distro, root } => {
            let mut command = Command::new("wsl.exe");
            command.args(["-d", distro, "--", "git", "-C", root, "--no-optional-locks"]);
            command
        },
    };
    hidden_command(&mut command)
        .args(args)
        .output()
        .map_err(|error| format!("无法运行 git: {error}"))
}

fn hidden_command(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    command
}

fn first_command_error(stderr: &[u8], fallback: &str) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback.to_owned())
}

fn join_guest_path(root: &str, relative: &str) -> String {
    if root == "/" {
        format!("/{relative}")
    } else {
        format!("{}/{relative}", root.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=Nebula Test", "-c", "user.email=nebula@example.invalid"])
            .args(args)
            .output()
            .expect("run git")
    }

    fn commit(root: &Path, message: &str) {
        let output = git(root, &["commit", "-m", message]);
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn complete_conflict_marker_triplet_is_detected() {
        assert!(contains_conflict_markers(
            "a\n<<<<<<< HEAD\nleft\n=======\nright\n>>>>>>> topic\nz"
        ));
        assert!(!contains_conflict_markers("<<<<<<< shown as documentation only"));
        assert!(!contains_conflict_markers("left\n=======\nright"));
    }

    #[test]
    fn conflict_path_cannot_escape_repository() {
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("../outside.rs").is_err());
        assert!(validate_relative_path("/absolute.rs").is_err());
        assert!(validate_relative_path("bad\nname.rs").is_err());
    }

    #[test]
    fn local_conflict_loads_three_versions_and_stages_result() {
        let directory = tempfile::tempdir().expect("create repository directory");
        let root = directory.path();
        let path = root.join("conflict.txt");

        assert!(git(root, &["init", "--initial-branch=main"]).status.success());
        std::fs::write(&path, "base\n").expect("write base");
        assert!(git(root, &["add", "conflict.txt"]).status.success());
        commit(root, "base");

        assert!(git(root, &["checkout", "-b", "incoming"]).status.success());
        std::fs::write(&path, "incoming\n").expect("write incoming");
        assert!(git(root, &["add", "conflict.txt"]).status.success());
        commit(root, "incoming");

        assert!(git(root, &["checkout", "main"]).status.success());
        std::fs::write(&path, "current\n").expect("write current");
        assert!(git(root, &["add", "conflict.txt"]).status.success());
        commit(root, "current");

        let merge = git(root, &["merge", "incoming"]);
        assert!(!merge.status.success(), "merge unexpectedly succeeded");
        let unmerged = git(root, &["ls-files", "-u", "--", "conflict.txt"]);
        assert!(
            unmerged.status.success() && !unmerged.stdout.is_empty(),
            "merge did not create conflict stages: {} {}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );

        let key = MergeKey {
            location: GitLocation::Local { root: root.to_path_buf() },
            relative_path: "conflict.txt".to_owned(),
        };
        let document = load_conflict(&key).expect("load conflict stages");
        assert_eq!(document.ours.as_deref(), Some("current\n"));
        assert_eq!(document.theirs.as_deref(), Some("incoming\n"));
        assert!(contains_conflict_markers(&document.result));

        write_conflict_result(&key, "resolved\n".to_owned()).expect("write and stage result");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "resolved\n");

        let unmerged = git(root, &["ls-files", "-u", "--", "conflict.txt"]);
        assert!(unmerged.status.success());
        assert!(unmerged.stdout.is_empty(), "unmerged index entries remain");
        let staged = git(root, &["diff", "--cached", "--name-only", "--", "conflict.txt"]);
        assert!(staged.status.success());
        assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "conflict.txt");
    }
}
