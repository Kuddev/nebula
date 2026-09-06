//! 本仓库自带的图标资源，叠在组件库资源之上。
//!
//! 只补 lucide 缺的那几个，不另起一套。
//!
//! 试过"统一圆角外壳 + 内部挖空骨架"的自绘方案，实机否掉了：外壳吃掉大半视觉
//! 重量后内部只剩 12×10px，20px 显示下无论画什么都糊成"方框里几个点"，反而
//! 比风格不统一更糟。所以导航图标回到 lucide 的线性无框语言，只在它确实没有
//! 对应图标时补画一枚，且照它的路数画（无外框、round cap）。
//!
//! 图标留在本仓库而不是塞进 fork 的资源包：那是给所有下游项目共用的。这一层
//! 做的就是"自己的先查，查不到再回落"。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// 编译期嵌入。就这么几个、每个不到 1KB，用不着 rust-embed 那一套。
macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("icons/nebula-", $name, ".svg"),
            include_bytes!(concat!("../../assets/icons/nebula-", $name, ".svg")).as_slice(),
        )),*]
    };
}

const NEBULA_ICONS: &[(&str, &[u8])] = icons![
    "command-manager",
    "keymap",
    "layout-grid",
    "mouse-pointer",
    "sliders",
    "pin",
    "pencil",
    "trash-2",
    "vcs-changes",
    "vcs-history",
    "vcs-conflict",
];

/// 先查本仓库，未命中再交给组件库。
pub struct NebulaAssets;

impl AssetSource for NebulaAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = NEBULA_ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = gpui_component_assets::Assets.list(path)?;
        names.extend(
            NEBULA_ICONS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(names)
    }
}

/// 自带图标的路径。走路径而不是 `IconName`：扩展那个枚举等于改 fork。
pub mod nav {
    pub const LAYOUT_GRID: &str = "icons/nebula-layout-grid.svg";
    pub const MOUSE_POINTER: &str = "icons/nebula-mouse-pointer.svg";
    pub const SLIDERS: &str = "icons/nebula-sliders.svg";
    pub const PIN: &str = "icons/nebula-pin.svg";
    /// 保存命令列表。闪电表达快捷执行，右侧三横线表达可管理的命令集合。
    pub const COMMAND_MANAGER: &str = "icons/nebula-command-manager.svg";
    /// 键盘。lucide 没有，而「按键映射」原来错挂在字号图标上。
    pub const KEYMAP: &str = "icons/nebula-keymap.svg";
    /// Lucide pencil；固定组件资产集未收录，命令行内编辑动作需要明确图形语义。
    pub const PENCIL: &str = "icons/nebula-pencil.svg";
    /// Lucide trash-2；删除保存命令不能借用表示 Backspace 的 `IconName::Delete`。
    pub const TRASH: &str = "icons/nebula-trash-2.svg";
    /// IDEA Commit 工具窗口同语义的“基线 + 提交节点”：工作区变更入口。
    pub const VCS_CHANGES: &str = "icons/nebula-vcs-changes.svg";
    /// 带分叉节点的提交线路：版本历史入口。
    pub const VCS_HISTORY: &str = "icons/nebula-vcs-history.svg";
    /// 两侧分支汇入结果并在交点标出冲突：三栏冲突解决入口。
    pub const VCS_CONFLICT: &str = "icons/nebula-vcs-conflict.svg";
}
