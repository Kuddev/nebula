//! 默认 GPUI 产品需要的共享输入编码器。
//!
//! 旧壳的鼠标、触摸和窗口事件分发仍留在 `input/mod.rs`；这里仅复用不依赖
//! 旧事件循环的终端线协议，避免为了两处运行时发送入口编译整套旧壳输入栈。

#[path = "input/terminal_input.rs"]
pub(crate) mod terminal_input;
