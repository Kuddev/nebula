//! 统一 Quick Jump 的名词投影。
//!
//! Overlay、筛选、键盘和执行都由父级 command palette 负责；这里仅把现有
//! Tabs、Panes、目录 frecency、SSH 与 AI sessions 投影成同一种行模型。

use gpui::App;
use gpui_component::IconNamed as _;

use super::*;

const DIRECTORY_LIMIT: usize = 128;
const AI_SESSION_LIMIT: usize = 30;

pub(super) fn rows(workspace: &NebulaWorkspace, cx: &App) -> Vec<WorkspacePaletteRow> {
    let language = workspace_ui_language();
    let mut rows = tab_rows(workspace, cx, language);
    rows.extend(directory_rows(language));
    rows.extend(ssh_rows(language));

    let ai_group = language.pick("AI 会话", "AI sessions");
    rows.extend(
        ai_session_palette_rows(crate::ai_sessions::scan(AI_SESSION_LIMIT)).into_iter().map(
            |mut row| {
                row.group_order += 4;
                row.group = format!("{ai_group} · {}", row.group);
                // 统一星标表达「可恢复的 Agent 会话」，provider 留在文字
                // 元数据中；这里不把 Claude/Codex 等 CLI 品牌误当成动作图标。
                row.icon_path = Some(IconName::Star.path());
                row
            },
        ),
    );
    rows
}

fn tab_rows(
    workspace: &NebulaWorkspace,
    cx: &App,
    language: crate::display::UiLanguage,
) -> Vec<WorkspacePaletteRow> {
    let tab_group = language.pick("标签页", "Tabs");
    let pane_group = language.pick("分屏", "Panes");
    let current = language.pick("当前", "Current");
    let mut rows = Vec::new();

    for (tab_ix, tab) in workspace.tabs.iter().enumerate() {
        let title = workspace.tab_title(tab_ix, cx).to_string();
        let (kind, icon_path) = match tab {
            WorkspaceTab::Terminal { .. } => {
                (language.pick("终端", "Terminal"), IconName::SquareTerminal.path())
            },
            WorkspaceTab::Settings { .. } => {
                (language.pick("设置", "Settings"), IconName::Settings.path())
            },
            WorkspaceTab::Image { .. } => (language.pick("图片", "Image"), IconName::File.path()),
            WorkspaceTab::Document { .. } => {
                (language.pick("文档", "Document"), IconName::File.path())
            },
            WorkspaceTab::Code { .. } => (language.pick("代码", "Code"), IconName::File.path()),
        };
        let hint = if tab_ix == workspace.active {
            format!("{current} · {kind}")
        } else {
            format!("{} {} · {kind}", language.pick("标签", "Tab"), tab_ix + 1)
        };
        rows.push(WorkspacePaletteRow {
            group_order: 0,
            group: tab_group.to_owned(),
            label: title.clone(),
            hint,
            hint_style: super::WorkspacePaletteHintStyle::Metadata,
            search: format!("{title} {kind} tab 标签 biaoqian {}", tab_ix + 1).to_lowercase(),
            action: WorkspacePaletteAction::FocusTab(tab_ix),
            icon: None,
            icon_glyph: None,
            icon_path: Some(icon_path),
        });

        let WorkspaceTab::Terminal { panes, focused, .. } = tab else { continue };
        for pane in panes {
            let view = pane.view.read(cx);
            let pane_title = view.tab_label();
            let location = view
                .ssh_destination
                .clone()
                .or_else(|| view.local_cwd().map(|path| path.display().to_string()))
                .unwrap_or_else(|| view.cwd.clone());
            let pane_hint = if tab_ix == workspace.active && pane.id == *focused {
                format!("{current} · {location}")
            } else {
                location.clone()
            };
            rows.push(WorkspacePaletteRow {
                group_order: 1,
                group: pane_group.to_owned(),
                label: format!("{title} · {pane_title}"),
                hint: pane_hint,
                hint_style: super::WorkspacePaletteHintStyle::Metadata,
                search: format!(
                    "{title} {pane_title} {location} pane split 分屏 fenping {}",
                    pane.id
                )
                .to_lowercase(),
                action: WorkspacePaletteAction::FocusPane { tab: tab_ix, pane: pane.id },
                icon: None,
                icon_glyph: None,
                icon_path: Some(IconName::PanelRight.path()),
            });
        }
    }
    rows
}

fn directory_rows(language: crate::display::UiLanguage) -> Vec<WorkspacePaletteRow> {
    let group = language.pick("常用目录", "Frequent directories");
    crate::directory_history::global()
        .search("", DIRECTORY_LIMIT)
        .into_iter()
        .map(|path| {
            let full = path.display().to_string();
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| full.clone());
            WorkspacePaletteRow {
                group_order: 2,
                group: group.to_owned(),
                label,
                hint: full.clone(),
                hint_style: super::WorkspacePaletteHintStyle::Metadata,
                search: format!("{full} directory folder 常用目录 mulu wenjianjia").to_lowercase(),
                action: WorkspacePaletteAction::OpenDirectory(path),
                icon: None,
                icon_glyph: None,
                icon_path: Some(IconName::FolderOpen.path()),
            }
        })
        .collect()
}

fn ssh_rows(language: crate::display::UiLanguage) -> Vec<WorkspacePaletteRow> {
    let group = language.pick("SSH 主机", "SSH hosts");
    let icons = ssh_host_icon_ids();
    crate::gpui_shell::ssh_hosts::SshHostLists::load()
        .merged()
        .into_iter()
        .map(|host| {
            let glyph =
                crate::display::ui::os_icons::resolve(icons.get(&host).map(String::as_str)).glyph;
            WorkspacePaletteRow {
                group_order: 3,
                group: group.to_owned(),
                label: host.clone(),
                hint: "SSH".to_owned(),
                hint_style: super::WorkspacePaletteHintStyle::Metadata,
                search: format!("{host} ssh host remote 远程 连接").to_lowercase(),
                action: WorkspacePaletteAction::LaunchSshHost(host),
                icon: None,
                icon_glyph: Some(glyph),
                icon_path: None,
            }
        })
        .collect()
}
