//! Shell/profile palette rows and their shared icon fallback.

use super::{
    WorkspacePaletteAction, WorkspacePaletteHintStyle, WorkspacePaletteRow, ssh_host_icon_ids,
};

/// shell 行的回落字形：没有品牌贴图时用 `shell_detect::icon_for_id` 的
/// id-keyed Nerd Font 字形（与设置页下拉、命令面板同一口径）。
///
/// `has_brand` 为真时返回 `None`——贴图已经画了，两个都留同行会出现两个图标。
fn fallback_shell_glyph(id: &str, has_brand: bool) -> Option<char> {
    if has_brand {
        return None;
    }
    crate::shell_detect::icon_for_id(id).chars().next()
}

/// 新建终端弹窗的行：已检测 shell + SSH 主机，分组对照旧壳
/// `CommandPalette::open_profiles`（推荐 / 所有 Shell / SSH 主机）。
/// 三点菜单与 Ctrl+K 打开的是这份列表，不是通用命令面板。
pub(super) fn shell_palette_rows(
    shells: Vec<crate::shell_detect::DetectedShell>,
    profiles: Vec<crate::config::ui_config::Profile>,
    ssh_hosts: impl IntoIterator<Item = String>,
    default_shell_id: &str,
    language: crate::display::UiLanguage,
    scale_factor: f32,
) -> Vec<WorkspacePaletteRow> {
    const SHELL_ICON_PX: f32 = 22.0;
    let recommended = language.pick("推荐", "Recommended");
    let all_shells = language.pick("所有 Shell", "All shells");
    let ssh_group = language.pick("SSH 主机", "SSH hosts");
    let mut rows: Vec<WorkspacePaletteRow> = shells
        .into_iter()
        .map(|shell| {
            let is_default = shell.id == default_shell_id;
            let icon = crate::gpui_shell::widgets::shell_brand_image(
                &shell.id,
                SHELL_ICON_PX,
                scale_factor,
            );
            // 没有品牌贴图的 shell（zsh、csh、ksh…）不能就这么空着：回落到
            // 按 id 取字的 Nerd Font 字形，与设置页下拉同一口径。
            let icon_glyph = fallback_shell_glyph(&shell.id, icon.is_some());
            WorkspacePaletteRow {
                group_order: if is_default { 0 } else { 1 },
                group: if is_default { recommended.to_owned() } else { all_shells.to_owned() },
                label: shell.name.clone(),
                hint: shell.program.clone(),
                hint_style: WorkspacePaletteHintStyle::Metadata,
                search: format!("{} {} shell profile", shell.name, shell.id).to_lowercase(),
                icon,
                icon_glyph,
                icon_path: None,
                action: WorkspacePaletteAction::LaunchShell(shell),
            }
        })
        .collect();
    rows.extend(profiles.into_iter().filter_map(|profile| {
        let id = profile.settings_id()?;
        let is_default = id.eq_ignore_ascii_case(default_shell_id);
        let icon_id = profile.shell_id.as_deref().unwrap_or(&id);
        let icon =
            crate::gpui_shell::widgets::shell_brand_image(icon_id, SHELL_ICON_PX, scale_factor);
        // 借用在 `profile` 被移进 action 之前结束。
        let icon_glyph = fallback_shell_glyph(icon_id, icon.is_some());
        let label = profile.name.clone();
        let hint = profile.command.clone();
        Some(WorkspacePaletteRow {
            group_order: if is_default { 0 } else { 1 },
            group: if is_default { recommended.to_owned() } else { all_shells.to_owned() },
            search: format!("{} {} {} shell profile", profile.name, id, profile.command)
                .to_lowercase(),
            label,
            hint,
            hint_style: WorkspacePaletteHintStyle::Metadata,
            action: WorkspacePaletteAction::LaunchProfile(profile),
            icon_glyph,
            icon,
            icon_path: None,
        })
    }));
    if let Some(position) = rows.iter().position(|row| row.group_order == 0) {
        let default_row = rows.remove(position);
        rows.insert(0, default_row);
    }
    let ssh_icons = ssh_host_icon_ids(&crate::display::nebula_data_dir());
    rows.extend(ssh_hosts.into_iter().map(|host| {
        let glyph =
            crate::display::ui::os_icons::resolve(ssh_icons.get(&host).map(String::as_str)).glyph;
        WorkspacePaletteRow {
            group_order: 2,
            group: ssh_group.to_owned(),
            label: host.clone(),
            hint: "SSH".to_owned(),
            hint_style: WorkspacePaletteHintStyle::Metadata,
            search: format!("{host} ssh host remote lianjie 连接").to_lowercase(),
            action: WorkspacePaletteAction::LaunchSshHost(host),
            icon: None,
            icon_glyph: Some(glyph),
            icon_path: None,
        }
    }));
    rows
}
