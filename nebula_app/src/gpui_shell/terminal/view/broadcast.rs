//! 广播输入的视图侧边界：把用户输入以**语义**上报给宿主，以及作为接收端
//! 按自己的终端状态重新编码。
//!
//! 从 `view.rs` 拆出来的理由不只是行数预算：这两个方向合起来是一条完整的
//! 合同——「谁 emit、谁不 emit」决定了会不会形成输入环，写在一个文件里才
//! 看得出对称性。
//!
//! - **发送方向**（`write_user_*`）：先按本 pane 的模式编码并写 PTY，再 emit
//!   一条语义事件。emit 是无条件的，广播开关的判定在宿主
//!   （`workspace::pane_header::fan_out_broadcast`），视图这边不留第二份记账。
//! - **接收方向**（`apply_broadcast_*`）：按**自己**的 term mode 重新编码后
//!   直接写 PTY，**不再 emit**——这就是无环的全部保证。
//!
//! 为什么不直接转发发送方编码好的字节：app-cursor、bracketed-paste、kitty
//! 键盘协议都是 per-pane 的终端状态。一个 pane 开着 vim、旁边是普通 shell
//! 时，同一个方向键的正确字节序列根本不同；照搬会往其中一个 pane 里灌乱码。

use gpui::Context;
use nebula_terminal::term::TermMode;

use super::super::keymap;
use super::{TerminalView, TerminalViewEvent};

/// 一次用户输入的语义载荷。
#[derive(Clone)]
pub enum TerminalInput {
    Key(gpui::Keystroke),
    /// `paste` = 走粘贴语义。接收端据此决定要不要套 bracketed-paste 哨兵：
    /// 给单个字符套上哨兵会让 zsh/fish 拒绝执行。
    Text { text: String, paste: bool },
}

impl TerminalView {
    /// 编码器路径的用户按键：本 pane 照原样写入，同时把 keystroke 上报。
    pub(super) fn write_user_key(
        &mut self,
        keystroke: gpui::Keystroke,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TerminalViewEvent::UserInput(TerminalInput::Key(keystroke)));
        self.write_input(bytes, cx);
    }

    /// IME 提交、补全接受与粘贴：上报**文本**而不是已经加过哨兵的字节。
    pub(super) fn write_user_text(
        &mut self,
        text: String,
        paste: bool,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(TerminalViewEvent::UserInput(TerminalInput::Text { text, paste }));
        self.write_input(bytes, cx);
    }

    /// 广播接收端按自己的 mode 编码，并直接写 PTY。这里不再 emit，避免
    /// 多个 pane 之间互相转发形成输入环。
    pub(crate) fn apply_broadcast_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) {
        if self.exited.is_some() {
            return;
        }
        let mode = self.term_mode();
        self.track_encoded_key(keystroke, &mode);
        if let Some(bytes) = keymap::encode(keystroke, &mode) {
            self.write_input(bytes, cx);
        }
    }

    /// 文本提交与粘贴都保留语义，由接收端自行套 bracketed-paste 包装。
    pub(crate) fn apply_broadcast_text(
        &mut self,
        text: &str,
        paste: bool,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() || self.exited.is_some() {
            return;
        }
        if paste {
            self.paste_now_impl(text, false, cx);
        } else {
            if !self.term_mode().contains(TermMode::ALT_SCREEN) {
                crate::display::Display::nebula_input_text(&mut self.suggest, text);
            }
            self.write_input(text.as_bytes().to_vec(), cx);
        }
    }
}
