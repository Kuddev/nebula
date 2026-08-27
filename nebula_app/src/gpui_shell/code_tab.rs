//! 代码查看 Tab：tree-sitter 语法高亮 + 行级虚拟化的只读查看器。
//!
//! 渲染走组件库 `InputState::code_editor`——它按可视行布局/绘制（约 5 万行
//! 量级依旧流畅），正好满足"超大代码只渲染可视范围"的性能合同；Markdown
//! 文档继续走 `TextView`（根级块虚拟化），两条管线各管各的文件类型。
//!
//! 只读策略：这是查看器不是编辑器——文件树双击的诉求是"看一眼源码"，
//! 写盘语义（保存/冲突/编码回写）超出本 tab 的合同。

use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px, relative,
};

use crate::gpui_shell::prelude::*;

/// 一次性读入的上限。行级虚拟化解决的是渲染成本，解析/塑形仍随内容量
/// 增长；8MB 已覆盖常见源码文件，超限截断并在文首说明。
const MAX_CODE_BYTES: usize = 8 * 1024 * 1024;

/// 扩展名 → tree-sitter 语言 id（组件库 highlighter 的注册名）。
/// 未命中的扩展仍可打开（纯文本、无高亮）。
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
        // 纯文本走 Plain（无高亮）：诉求是行号 + 行级虚拟化，不是着色。
        "txt" | "log" | "text" => "text",
        _ => return None,
    })
}

/// 文件树双击是否进代码查看 tab（与 markdown/图片路由并列；三者都不认的
/// 交系统处理器）。
pub fn viewable_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| language_for_extension(extension).is_some())
}

pub struct CodeTabView {
    pub path: PathBuf,
    pub title: String,
    input: Entity<InputState>,
    notice: Option<String>,
    lines: usize,
}

impl CodeTabView {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let language = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(language_for_extension)
            .unwrap_or("text");
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor(language)
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
        });
        let mut this = Self { path, title, input, notice: None, lines: 0 };
        this.reload(window, cx);
        this
    }

    /// 重新读盘（文件树再次双击同一路径时宿主调用，与 doc tab 同义）。
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}

impl Render for CodeTabView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(
                div().flex_1().min_h_0().child(
                    Input::new(&self.input)
                        .h_full()
                        .bordered(false)
                        .focus_bordered(false)
                        .rounded(px(0.0))
                        .font_family(mono_family)
                        .text_size(mono_size)
                        .line_height(relative(1.55))
                        .px(px(14.0))
                        .py(px(10.0)),
                ),
            )
    }
}
