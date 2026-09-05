//! 平台能力表。
//!
//! UI 层只问「这台机器能不能」，不问「是不是 Windows」。一个功能在某平台
//! 尚未实现时，对应字段为 `false`，设置页整行不渲染（不是灰掉——灰掉会引来
//! 「为什么灰」的 issue），调用方也不会走进一个静默 no-op。
//!
//! 新增平台差异先在这里加字段，再在各平台实现里对号入座；把 `cfg` 留在
//! `platform/` 之内是 `scripts/check_platform_cfg.py` 卡住的预算规则。

/// 编译期常量的能力集合。字段顺序按用户可见程度排列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// 关窗后进程驻留并隐藏窗口（Windows `SW_HIDE`）。Unix 尚无实现：
    /// `hide_native_window` 在那里是 no-op，开着会导致「既不关也不藏」。
    pub hide_window_on_close: bool,
    /// 系统托盘图标（`crate::tray`）。
    pub system_tray: bool,
    /// 系统通知后端已实现；实际投递仍受系统通知权限控制。
    pub system_notifications: bool,
    /// 系统提示音（`platform::beep`）。
    pub system_bell: bool,
    /// 下载并静默运行安装包完成更新；其余平台只能打开 Releases 页。
    pub self_update_install: bool,
    /// 自动往 Claude / Codex 配置里写入 hook 与 skill（`crate::ai_hook`）。
    pub ai_hook_server: bool,
    /// 全局热键呼出快速终端。
    pub quick_terminal_hotkey: bool,
    /// 枚举系统字体族供字体选择器使用。
    pub system_font_enumeration: bool,
    /// 系统凭据后端已实现；Linux 还需 credentials::can_store 检查运行依赖。
    pub credential_store: bool,
    /// 安装版可把「在此处打开 Nebula」挂进资源管理器右键菜单。
    pub shell_context_menu: bool,
}

pub const CAPABILITIES: Capabilities = {
    #[cfg(windows)]
    {
        Capabilities {
            hide_window_on_close: true,
            system_tray: true,
            system_notifications: true,
            system_bell: true,
            self_update_install: true,
            ai_hook_server: true,
            quick_terminal_hotkey: true,
            system_font_enumeration: true,
            credential_store: true,
            shell_context_menu: true,
        }
    }
    #[cfg(not(windows))]
    {
        Capabilities {
            hide_window_on_close: false,
            system_tray: false,
            system_notifications: true,
            system_bell: false,
            self_update_install: false,
            ai_hook_server: false,
            quick_terminal_hotkey: false,
            system_font_enumeration: true,
            credential_store: true,
            shell_context_menu: false,
        }
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_integrations_never_advertise_unimplemented_features() {
        let all = [
            CAPABILITIES.hide_window_on_close,
            CAPABILITIES.system_tray,
            CAPABILITIES.system_notifications,
            CAPABILITIES.system_bell,
            CAPABILITIES.self_update_install,
            CAPABILITIES.ai_hook_server,
            CAPABILITIES.quick_terminal_hotkey,
            CAPABILITIES.system_font_enumeration,
            CAPABILITIES.credential_store,
            CAPABILITIES.shell_context_menu,
        ];
        if cfg!(windows) {
            assert!(all.iter().all(|flag| *flag));
        } else {
            assert!(CAPABILITIES.system_notifications);
            assert!(CAPABILITIES.system_font_enumeration);
            assert!(CAPABILITIES.credential_store);
            assert!(!CAPABILITIES.hide_window_on_close);
            assert!(!CAPABILITIES.system_tray);
            assert!(!CAPABILITIES.system_bell);
            assert!(!CAPABILITIES.self_update_install);
            assert!(!CAPABILITIES.ai_hook_server);
            assert!(!CAPABILITIES.quick_terminal_hotkey);
            assert!(!CAPABILITIES.shell_context_menu);
        }
    }
}
