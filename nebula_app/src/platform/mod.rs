//! 平台闸门层（docs/specs/005-cross-platform-foundation.md）。
//!
//! 目的是让业务代码不再自己写 `#[cfg(windows)]`。三平台的差异全部收在
//! 这里的子模块中，对外只暴露平台无关的签名。
//!
//! 设计裁定（005 §4.1）：**不用 trait 对象**。Tabby 的 `PlatformService`
//! 是 OOP + DI 的产物，Rust 里照搬一个 20 方法的 `dyn` trait 只会换来强制
//! 动态分发和造假实现的测试负担——平台实现本就是编译期确定的。这里改用
//! 「按能力切模块 + 模块内 `#[cfg]` 分派」，保留 Tabby 真正值钱的两点：
//! 单一入口，以及能力探测（让 UI 隐藏入口，而不是让功能在别的平台报错）。

pub mod dirs;

/// 运行平台。与「配置平台」分离（005 §4.3）：前者是我真正跑在哪，后者是
/// 该套用哪份键位/修饰键默认值——Mac 用户在 Windows 上可以选 ⌘ 语义。
/// 配置平台待 L5 键位分文件时落地。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOS,
    Linux,
}

impl Platform {
    pub const fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            Self::MacOS
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            Self::Linux
        }
    }
}
