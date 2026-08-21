//! 文件树 → 终端的拖放契约。
//!
//! 载荷类型必须住在文件树和终端**都能引用**的地方：前者在
//! `workspace::file_tree`（`workspace.rs` 的私有子模块），后者在
//! `terminal::view`，两边互相看不见对方的私有模块。
//!
//! 与旧壳的分工：路径写进 PTY 的**规则**（拒绝控制字符、含空白加引号、尾随
//! 一个空格）是共享的，见 [`crate::display::side_panel::drop_text_for_path`]；
//! 这里只负责 GPUI 这一侧的"拖的是什么、拖起来长什么样"。旧壳的
//! [`crate::display::side_panel::FileDrag`] 还要自己追踪按压阈值、指针位置和
//! 是否悬停在终端上——GPUI 的拖放引擎原生管这些，所以本模块的载荷是纯数据。

use gpui::{Context, IntoElement, ParentElement as _, Render, Styled as _, Window, div, px};

use crate::gpui_shell::prelude::*;

/// 一次从文件树拖向终端的条目。
#[derive(Clone, Debug)]
pub struct FileTreeDrag {
    /// 落进 PTY 的路径**原文**。
    ///
    /// WSL 行带的是来宾路径（`/home/x`）而不是那个只用作展开键的 `PathBuf`：
    /// 拖一个 WSL 目录到 WSL 终端里，用户要的是能直接 `cd` 的来宾路径，宿主
    /// 的拼写在那个 shell 里根本不存在。
    pub path_text: String,
    /// 跟着指针走的标签。
    pub name: String,
}

/// 跟随指针的拖动预览。
///
/// GPUI 的 `on_drag` 要一个 `Entity<impl Render>` 而不是普通元素——预览独立于
/// 源元素的生命周期，拖过程中源行可能已经因为树刷新而不存在了。
pub struct FileDragGhost {
    name: String,
}

impl FileDragGhost {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

impl Render for FileDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.foreground)
            .text_sm()
            .max_w(px(320.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(self.name.clone())
    }
}
