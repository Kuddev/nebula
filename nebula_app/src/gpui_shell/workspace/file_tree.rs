//! 侧栏文件树渲染（从 workspace.rs 拆出以守行数预算）。
//!
//! 几何与密度以旧 OpenGL 壳的 `display::side_panel::panel_layout`
//! （side_panel.rs:1695）和它的行水洗（同文件 2712-2800）为基准，不自创数字。

use std::ops::Range;
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, Focusable as _,
    InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent, ParentElement as _,
    Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, anchored, deferred, div, px, uniform_list,
};
use gpui_component::menu::PopupMenuItem;

use crate::gpui_shell::prelude::*;

use super::NebulaWorkspace;

/// 行距（旧壳 `PanelLayout::row_h`）。
const ROW_PITCH: f32 = 34.0;
/// 行水洗高度（旧壳 `row_h - 4`）。行与行之间那条缝来自水洗比行距矮，
/// **不是** flex gap——用 gap 会把每行推成一颗独立药丸，连续列表的读感就散了。
const ROW_WASH_H: f32 = 30.0;
/// 水洗相对抽屉内缘的额外内缩：旧壳的水洗从面板边缘缩 10px，而抽屉自身的
/// `p_2` 已经占掉 8px。
const ROW_WASH_INSET: f32 = 2.0;
/// 抽屉内容左右留白（旧壳 `px + 12`：比水洗再多 2px）。
const DRAWER_TEXT_INSET: f32 = 4.0;

/// 文件树右键菜单的宿主：必须挂在 workspace 根上，不能当抽屉行的 child。
///
/// `ContextMenuExt` 会把 `deferred(anchored(PopupMenu))` 挂回触发行；行在带
/// `shadow` 的抽屉里面。菜单翻到抽屉左缘时，抽屉投影会垫在菜单周围，看起来
/// 比侧栏 Tab 右键厚一截。
pub(super) struct FileTreeContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

/// `--cd` 只指定来宾工作目录，不指定要执行的程序；这样 WSL 仍会读取
/// `/etc/passwd` 中由 `chsh` 配置的默认 shell。
pub(super) fn wsl_terminal_launch_at(
    name: String,
    program: String,
    distro: String,
    guest_path: String,
) -> crate::session::LaunchSession {
    crate::session::LaunchSession::Shell {
        name,
        program,
        args: vec!["-d".to_owned(), distro, "--cd".to_owned(), guest_path],
    }
}

impl NebulaWorkspace {
    /// 单行：34px 行距里画一张 30px 高的水洗（旧壳 side_panel.rs:2712-2800）。
    fn render_file_tree_row(&self, visible_ix: usize, cx: &Context<Self>) -> gpui::AnyElement {
        let Some(row) = self.side_panel.file_rows().get(visible_ix).cloned() else {
            return div().h(px(ROW_PITCH)).into_any_element();
        };
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let hover = theme.list_hover;
        // 选中行穿「浮动药丸」那套语言（accent_soft），与左侧栏活动 tab、抽屉
        // 页签同源——旧壳 side_panel.rs:2766 明确写了这一点。`list_active` 是
        // 中性的 hover_strong，穿上去只像"悬停重了一点"，选中读不出来。
        let selected_bg = theme.tab_active;
        let selected_ring = theme.ring.opacity(0.16);
        // 文件树字位是内置 Nerd Font 图标；固定 Maple，不能让终端主字体
        // 改变折叠箭头和文件图标的 advance。
        let symbol_family: SharedString = crate::font_install::REQUIRED_FONT_FAMILY.into();
        let selected = self.side_panel.selected.as_ref() == Some(&row.path);
        let path = row.path.clone();
        let open_path = path.clone();
        let open_guest_path = row.guest_path.clone();
        let menu_path = path.clone();
        let guest_path = row.guest_path.clone();
        let menu_guest_path = guest_path.clone();
        let drag_guest_path = guest_path.clone();
        let drag_path = path.clone();
        let drag_name = row.name.clone();
        let is_dir = row.is_dir;
        let is_parent = row.is_parent;
        let fg = if row.ignored { muted } else { theme.foreground };
        let _chevron = if row.is_dir && !row.is_parent {
            if row.expanded { "⌄" } else { "›" }
        } else {
            ""
        };
        let file_glyph =
            (!row.is_dir).then(|| crate::display::side_panel::file_type_icon(&row.name));
        let legacy_chevron = row
            .is_dir
            .then(|| {
                (!row.is_parent).then(|| crate::display::side_panel::chevron_icon(row.expanded))
            })
            .flatten();

        // 外层只占行距（34px），内层那张 30px 的水洗由它垂直居中——旧壳
        // 就是这么分的：行距决定密度，水洗决定"被点中的那块"有多大。
        h_flex()
            .h(px(ROW_PITCH))
            .w_full()
            .px(px(ROW_WASH_INSET))
            .child(
                h_flex()
                .id(SharedString::from(format!("file-tree-row-{visible_ix}")))
                .h(px(ROW_WASH_H))
                .flex_1()
                .min_w_0()
                .items_center()
                .pr_2()
                .pl(px(8.0 + row.depth as f32 * 16.0))
                .gap_1()
                .rounded(px(crate::display::UI_CORNER_RADIUS_LOGICAL))
                // 每行都带 1px 边（未选中时透明）：只在选中时加边会让行内文字
                // 横跳 1px。
                .border_1()
                .border_color(gpui::transparent_black())
                .text_color(fg)
                .when(selected, |item| item.bg(selected_bg).border_color(selected_ring))
                .hover(|item| item.bg(hover))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_shrink_0()
                        .font_family(symbol_family.clone())
                        .text_sm()
                        .text_color(muted)
                        .child(legacy_chevron.unwrap_or("")),
                )
                .when(is_dir, |item| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(symbol_family.clone())
                            .text_sm()
                            .text_color(if row.ignored { muted } else { theme.foreground })
                            .child(crate::display::side_panel::folder_icon(row.expanded)),
                    )
                })
                .when_some(file_glyph, |item, glyph| {
                    item.child(
                        div()
                            .w(px(16.0))
                            .flex_shrink_0()
                            .font_family(symbol_family.clone())
                            .text_sm()
                            .child(glyph),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(row.name),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_dir {
                        if this.side_panel.click_row(visible_ix) {
                            cx.notify();
                        }
                    } else {
                        this.side_panel.selected = Some(path.clone());
                        cx.notify();
                    }
                }))
                .when(!is_dir && !is_parent, |item| {
                    item.on_double_click(cx.listener(move |this, _, window, cx| {
                        // 与旧壳 chrome 同合同：应用内能读的（图片/可读文本）
                        // 开查看 tab，其余交系统处理器。WSL 行先映射为官方 UNC
                        // 形式，不能把 `/home/...` 直接交给 Windows 文件 API。
                        if let Some(guest) = open_guest_path.clone() {
                            this.open_wsl_document_path(guest, window, cx);
                        } else {
                            this.open_document_path(open_path.clone(), window, cx);
                        }
                    }))
                })
                .when(!is_parent, |item| {
                    // 拖到终端 = 把路径粘进 shell（旧壳 `FileDrag` 同一合同）。
                    // WSL 行拖的是**来宾**路径：宿主那份拼写只是展开用的键，
                    // 在来宾的 shell 里不存在。
                    let drag = crate::gpui_shell::file_drop::FileTreeDrag {
                        path_text: drag_guest_path
                            .clone()
                            .unwrap_or_else(|| drag_path.display().to_string()),
                        name: drag_name.clone(),
                    };
                    item.on_drag(drag, |drag, _offset, _window, cx| {
                        cx.new(|_| {
                            crate::gpui_shell::file_drop::FileDragGhost::new(drag.name.clone())
                        })
                    })
                })
                .when(!is_parent, |item| {
                    // 不在抽屉行上挂 `.context_menu()`：那会把 PopupMenu 挂回
                    // 行的 child，菜单仍是带投影抽屉的子孙。右键只记锚点，
                    // 菜单由 workspace 根上的 `deferred(anchored)` 画。
                    item.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_file_tree_context_menu(
                                menu_path.clone(),
                                menu_guest_path.clone(),
                                is_dir,
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                }),
            )
            .into_any_element()
    }

    /// 抽屉。贴满右侧整条竖带，四边不留卡缝（用户 08-26 裁定"无缝"）；行几何
    /// 仍以旧壳 `panel_layout`（side_panel.rs:1695-1729）为基准。
    pub(super) fn render_file_tree(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        // 滚动只由 uniform_list 承担。旧壳那套行粒度 `scroll` 不再参与，否则
        // `click_row` 的 `scroll + index` 会把点击算到别的行上。
        self.side_panel.scroll = 0;
        let view_switch = self.render_side_panel_switch(cx).into_any_element();
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let root_dir = self.side_panel.root().map(std::path::Path::to_path_buf);
        let root = root_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "等待终端上报工作目录…".to_owned());
        let row_count = self.side_panel.file_rows().len();
        let empty = self.file_tree_empty_state();
        let scroll_handle = self.file_tree_scroll.clone();

        v_flex()
            .h_full()
            .w(px(320.0))
            .flex_shrink_0()
            .p_2()
            .gap_2()
            // 抽屉圆角只给左侧两角（用户 08-26 裁定「最右侧去掉圆角，左侧保留」）：
            // 右缘贴住窗口边框，倒角只会在那里啃出两个壳色缺口；左缘隔着终端卡的
            // 8px 卡缝、下缘隔着槽位的 8px，圆角落在壳色上才有浮板感。左侧也不画
            // 发丝线——有卡缝就不需要线分界，只留一条线反而像把面板压在终端上。
            .rounded_tl(crate::gpui_shell::theme::card_radius(cx))
            .rounded_bl(crate::gpui_shell::theme::card_radius(cx))
            .bg(theme.popover)
            // 抽屉在窗口右侧；右键菜单常翻到左缘。Tailwind `shadow_lg`
            //（10px 下偏移 + 15px 模糊，再加一层）会垫在菜单周围，比 Tab
            // 右键的 `popover_shadow` 厚一截。抽屉本身也是 popover 面，跟
            // 菜单用同一套紧凑投影。
            .shadow(gpui_component::popover_shadow(theme.is_dark()))
            .occlude()
            .child(view_switch)
            .child(
                h_flex()
                    .h(px(30.0))
                    .px(px(DRAWER_TEXT_INSET))
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(root),
                    )
                    .child(
                        Button::new("file-tree-terminal-here")
                            .icon(IconName::SquareTerminal)
                            .ghost()
                            .xsmall()
                            .disabled(root_dir.is_none())
                            .tooltip("在此新建终端")
                            .on_click(cx.listener(|this, _, window, cx| {
                                // WSL 树根要开的是**来宾**终端：宿主那份拼写只是
                                // 展开用的键，在来宾的 shell 里不存在。
                                let wsl = this
                                    .side_panel
                                    .file_wsl_root()
                                    .map(|root| (root.distro.clone(), root.guest.clone()));
                                match wsl {
                                    Some((distro, guest)) => {
                                        this.add_wsl_terminal_at(distro, guest, window, cx)
                                    },
                                    None => {
                                        let Some(dir) = this.side_panel.root().map(Into::into) else {
                                            return;
                                        };
                                        this.add_terminal_at(Some(dir), None, window, cx);
                                    },
                                }
                            })),
                    )
                    .child(
                        Button::new("file-tree-reveal")
                            .icon(IconName::FolderOpen)
                            .ghost()
                            .xsmall()
                            .disabled(root_dir.is_none())
                            .tooltip("在资源管理器中打开")
                            .on_click(cx.listener(|this, _, _, _cx| {
                                let wsl = this
                                    .side_panel
                                    .file_wsl_root()
                                    .map(|root| (root.distro.clone(), root.guest.clone()));
                                let path = match wsl {
                                    Some((distro, guest)) => {
                                        crate::shell_detect::wsl_unc_path(&distro, &guest)
                                    },
                                    None => match this.side_panel.root() {
                                        Some(root) => root.to_path_buf(),
                                        None => return,
                                    },
                                };
                                // 这颗钮是「打开这个目录」，不是「在父目录里选中它」。
                                // 之前走 reveal（`explorer /select,<root>`）＝让资源
                                // 管理器打开**父目录**并高亮该项：只要父目录的窗口已经
                                // 开着（实测 `D:\` 常驻），explorer 就复用那个后台窗口
                                // 重设选中项、既不新开也不前置，点击看上去毫无反应。
                                // 行级右键的「在资源管理器中显示」才该用 reveal。
                                super::open_in_file_manager(&path);
                            })),
                    )
                    .child(
                        Button::new("file-tree-refresh")
                            .icon(IconName::Redo2)
                            .ghost()
                            .xsmall()
                            .tooltip("跟随当前终端并刷新 (Alt+R)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.side_panel.request_refresh();
                                // 刷新同时恢复“跟随当前 pane”；否则点过 `..` 后
                                // custom root 会继续压住当前终端，按钮只会刷新旧目录。
                                this.sync_side_panel_to_active(true, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("file-tree-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("关闭目录树 (Ctrl+Shift+F)")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_file_tree(cx);
                            })),
                    ),
            )
            .when_some(self.side_panel.root_notice(), |panel, notice| {
                panel.child(div().text_xs().text_color(theme.warning).child(notice.to_owned()))
            })
            .child(
                // 一套滚动模型。此前是 `file_rows().skip(scroll)` 先砍掉上面的行、
                // 再套 `overflow_y_scrollbar` 滚剩下的，两套模型打架：滚动条滑块
                // 按剩余行算长度，滚轮又同时动两边。改成 uniform_list 虚拟化
                // ——它自己就是滚动容器，滑块交给组件库 Scrollbar 读同一个 handle。
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    // 必须裁：`uniform_list` 会把行画到自己的 bounds 之外，没有这层
                    // 末行就压过抽屉的下内边距、一直画到抽屉底边，把 8px 留白和下两
                    // 角的圆角都盖掉（用户报的"圆角没了"）。右键菜单走 deferred 顶层
                    // 绘制，不受这里的裁剪影响。
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "file-tree-rows",
                            row_count,
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                range.map(|ix| this.render_file_tree_row(ix, cx)).collect()
                            }),
                        )
                        .w_full()
                        .flex_grow_1()
                        // 空态时让列表收缩到内容高度（通常只剩 `..` 一行），空态
                        // 文案接在它下面——旧壳也是把文案画在 `..` 行之后。
                        .when(empty.is_some(), |list| {
                            list.with_sizing_behavior(gpui::ListSizingBehavior::Infer)
                        })
                        .when(empty.is_none(), |list| {
                            list.size_full()
                                .with_sizing_behavior(gpui::ListSizingBehavior::Auto)
                        })
                        .track_scroll(&scroll_handle),
                    )
                    // 滑块住在一个显式绝对定位的宿主里，和组件库自己的 Table 同构
                    // （table/state.rs:2182）：直接当普通 child 塞进来它会参与常规
                    // 布局、排在列表下面占掉一条高度，而不是浮在列表右缘。
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            // 组件库的 `Scrollbar::width()` 是 crate 私有的，值为
                            // 轨道 8px + 两侧 4px 内缩（scroll/scrollbar.rs:17）。
                            .w(px(16.0))
                            .child(gpui_component::scroll::Scrollbar::vertical(&scroll_handle)),
                    )
                    .when_some(empty, |list, empty| {
                        list.child(
                            v_flex()
                                .w_full()
                                .px(px(DRAWER_TEXT_INSET + ROW_WASH_INSET))
                                .py_2()
                                .gap_1()
                                .child(div().text_xs().text_color(theme.foreground).child(empty.title))
                                .child(div().text_xs().text_color(muted).child(empty.reason))
                                .child(div().text_xs().text_color(muted).child(empty.action)),
                        )
                    }),
            )
            .into_any_element()
    }

    /// 空态三段（标题 + 原因 + 可执行动作），文案与旧壳
    /// side_panel.rs:3112-3131 同源。判据也照旧壳：`..` 不算内容，只剩它时这个
    /// 目录仍然是空的；"读不到"和"确实是空的"必须分开说，否则用户没法判断该
    /// 重试还是该换目录。
    fn file_tree_empty_state(&self) -> Option<crate::ux::EmptyState> {
        if self.side_panel.file_rows().iter().any(|row| !row.is_parent) {
            return None;
        }
        Some(if self.side_panel.snapshot_pending() {
            crate::ux::EmptyState::new(
                "正在读取目录",
                "还在枚举当前工作目录的内容。",
                "稍等一下，或点右上角重新跟随。",
            )
        } else if self.side_panel.enumeration_failed() {
            // 读不到 ≠ 目录是空的。WSL 冷启动可能耗尽预算，那时必须给可重试的提示。
            crate::ux::EmptyState::new(
                "读取目录失败",
                "枚举这个目录时被系统拒绝，或超出了单次预算。",
                "点右上角重新跟随重试，或换一个目录。",
            )
        } else if self.side_panel.root().is_none() {
            crate::ux::EmptyState::new(
                "没有可浏览的目录",
                "当前终端尚未报告工作目录。",
                "在终端中进入一个目录后点击右上角跟随。",
            )
        } else {
            crate::ux::EmptyState::new(
                "此目录为空",
                "当前工作目录中没有可显示的文件。",
                "在终端创建文件，或选择其他目录。",
            )
        })
    }

    fn open_file_tree_context_menu(
        &mut self,
        path: PathBuf,
        guest_path: Option<String>,
        is_dir: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.side_panel.selected = Some(path.clone());
        let wsl_distro = guest_path
            .as_ref()
            .and_then(|_| self.side_panel.file_wsl_root().map(|root| root.distro.clone()));
        let workspace = cx.entity().downgrade();
        let menu = PopupMenu::build(window, cx, move |menu, window, _cx| {
            file_tree_popup_menu(
                menu.external_link_icon(false),
                workspace,
                path,
                guest_path,
                wsl_distro,
                is_dir,
                window,
            )
        });
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.file_tree_menu = None;
            cx.notify();
        });
        self.file_tree_menu =
            Some(FileTreeContextMenu { menu, position, _subscription: subscription });
        cx.notify();
    }

    pub(super) fn render_file_tree_context_menu(&self) -> Option<gpui::AnyElement> {
        let state = self.file_tree_menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(state.position)
                    .snap_to_window_with_margin(px(8.0))
                    .anchor(Anchor::TopLeft)
                    .child(state.menu.clone()),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// WSL 文件树行只携带来宾路径；发行版身份来自当前树根。通过
    /// `\\wsl.localhost` 交给现有文档/系统打开路由，保持文件类型行为一致。
    fn open_wsl_document_path(
        &mut self,
        guest_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.side_panel.file_wsl_root() else { return };
        let path = crate::shell_detect::wsl_unc_path(&root.distro, &guest_path);
        self.open_document_path(path, window, cx);
    }

    /// `wsl.exe --cd` 不执行指定命令，因此仍由来宾 `/etc/passwd` 选择 zsh/fish；
    /// 这与普通 WSL 新标签相同，并避免重引入 v1.1.0 的 `--exec bash` 回归。
    fn add_wsl_terminal_at(
        &mut self,
        distro: String,
        guest_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detected = crate::shell_detect::detect_shells().into_iter().find(|shell| {
            crate::shell_detect::wsl_launch_distro(&shell.program, &shell.args)
                .is_some_and(|name| name.eq_ignore_ascii_case(&distro))
        });
        let (name, program) = detected
            .map(|shell| (shell.name, shell.program))
            .or_else(|| match self.meta(self.active).launch {
                Some(crate::session::LaunchSession::Shell { name, program, args })
                    if crate::shell_detect::wsl_launch_distro(&program, &args)
                        .is_some_and(|name| name.eq_ignore_ascii_case(&distro)) =>
                {
                    Some((name, program))
                },
                _ => None,
            })
            .unwrap_or_else(|| (format!("WSL · {distro}"), "wsl.exe".to_owned()));
        let launch = wsl_terminal_launch_at(name, program, distro, guest_path);
        self.add_terminal_with(launch, None, None, window, cx);
    }

    fn request_delete_file_tree_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !path.exists() {
            self.side_panel.set_notice("路径已不存在".to_owned());
            cx.notify();
            return;
        }
        let is_dir = path.is_dir();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let title: SharedString =
            format!("删除 {}？", crate::display::truncate_tab_label(&name, 28)).into();
        let body: SharedString = if is_dir {
            "文件夹及其全部内容会移入回收站。".into()
        } else {
            "文件会移入回收站。".into()
        };
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let workspace = workspace.clone();
            let path = path.clone();
            confirm_dialog(
                dialog,
                window,
                title.clone(),
                body.clone(),
                "删除",
                "取消",
                ButtonVariant::Danger,
            )
            .on_ok(move |_, _, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    match crate::display::send_to_recycle_bin(&path) {
                        Ok(()) => {
                            this.side_panel.request_refresh();
                            this.sync_side_panel_to_active(false, cx);
                        },
                        Err(error) => this.side_panel.set_notice(format!("删除失败：{error}")),
                    }
                    cx.notify();
                });
                true
            })
        });
    }
    /// 把一条路径写进仓库根的 `.gitignore`。
    ///
    /// 不弹确认框：这是追加一行、随时能删掉的动作，代价远低于多一次点击的
    /// 摩擦。但**必须回报写了什么**——忽略规则的语义（锚定到根、目录带尾斜杠、
    /// 元字符转义过）和用户点的那个文件名并不一字不差，看不到写进去的那一行
    /// 就没法判断范围对不对。
    fn ignore_file_tree_path(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        use crate::display::side_panel::IgnoreOutcome;

        match crate::display::side_panel::append_to_gitignore(&path, is_dir) {
            Ok(IgnoreOutcome::Added { entry, .. }) => {
                self.side_panel.set_notice(format!("已写入 .gitignore：{entry}"));
                // 忽略状态由 `git check-ignore` 现算，所以重跑一次快照这一行
                // 就会转成灰色——用户能立刻看到规则生效了。
                self.side_panel.request_refresh();
                self.sync_side_panel_to_active(false, cx);
            },
            Ok(IgnoreOutcome::AlreadyPresent { entry }) => {
                self.side_panel.set_notice(format!(".gitignore 已有这条规则：{entry}"));
            },
            Err(error) => self.side_panel.set_notice(error),
        }
        cx.notify();
    }
}

fn file_tree_popup_menu(
    menu: PopupMenu,
    workspace: gpui::WeakEntity<NebulaWorkspace>,
    path: PathBuf,
    guest_path: Option<String>,
    wsl_distro: Option<String>,
    is_dir: bool,
    _window: &mut Window,
) -> PopupMenu {
    if let Some(guest_path) = guest_path {
        let copy_guest = guest_path.clone();
        let copy =
            PopupMenuItem::new("复制 Linux 路径").icon(IconName::Copy).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_guest.clone()));
            });
        let Some(distro) = wsl_distro else { return menu.item(copy) };
        let first = if is_dir {
            let open_here = workspace.clone();
            let open_distro = distro.clone();
            let open_guest = guest_path.clone();
            PopupMenuItem::new("在此处打开终端").icon(IconName::SquareTerminal).on_click(
                move |_, window, cx| {
                    if let Some(workspace) = open_here.upgrade() {
                        workspace.update(cx, |this, cx| {
                            this.add_wsl_terminal_at(
                                open_distro.clone(),
                                open_guest.clone(),
                                window,
                                cx,
                            );
                        });
                    }
                },
            )
        } else {
            let open = workspace.clone();
            let open_guest = guest_path.clone();
            PopupMenuItem::new("打开").icon(IconName::File).on_click(move |_, window, cx| {
                if let Some(workspace) = open.upgrade() {
                    workspace.update(cx, |this, cx| {
                        this.open_wsl_document_path(open_guest.clone(), window, cx);
                    });
                }
            })
        };
        let reveal_path = crate::shell_detect::wsl_unc_path(&distro, &guest_path);
        return menu
            .item(first)
            .item(PopupMenuItem::new("在资源管理器中显示").icon(IconName::FolderOpen).on_click(
                move |_, _, _| {
                    super::reveal_in_file_manager(&reveal_path);
                },
            ))
            .item(copy);
    }
    let first = if is_dir {
        let open_here = workspace.clone();
        let dir = path.clone();
        PopupMenuItem::new("在此处打开终端").icon(IconName::SquareTerminal).on_click(
            move |_, window, cx| {
                if let Some(workspace) = open_here.upgrade() {
                    workspace.update(cx, |this, cx| {
                        this.add_terminal_at(Some(dir.clone()), None, window, cx);
                    });
                }
            },
        )
    } else {
        let open = workspace.clone();
        let file = path.clone();
        PopupMenuItem::new("打开").icon(IconName::File).on_click(move |_, window, cx| {
            if let Some(workspace) = open.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.open_document_path(file.clone(), window, cx);
                });
            }
        })
    };
    let reveal_path = path.clone();
    let copy_path = path.clone();
    // 只有真在 Git 仓库里才给这一项。判据不花钱（沿祖先看 `.git` 在不在，见
    // `git_repository_root` 里为什么不 spawn git），而在 SVN 工作副本或普通
    // 目录上摆一个"加入 .gitignore"是纯误导——SVN 的忽略入口在 VCS 面板的
    // 行内菜单里。
    let ignore_item = crate::display::side_panel::git_repository_root(&path).map(|_| {
        let ignore = workspace.clone();
        let ignore_path = path.clone();
        PopupMenuItem::new("加入 .gitignore").icon(IconName::EyeOff).on_click(move |_, _, cx| {
            if let Some(workspace) = ignore.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.ignore_file_tree_path(ignore_path.clone(), is_dir, cx);
                });
            }
        })
    });
    let delete = workspace;
    menu.item(first)
        .item(PopupMenuItem::new("在资源管理器中显示").icon(IconName::FolderOpen).on_click(
            move |_, _, _| {
                super::reveal_in_file_manager(&reveal_path);
            },
        ))
        .item(PopupMenuItem::new("复制路径").icon(IconName::Copy).on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_path.display().to_string()));
        }))
        .separator()
        // 忽略在删除上面：两者都改工作区，但删除不可逆，危险的排最后。
        .when_some(ignore_item, |menu, item| menu.item(item))
        .item(PopupMenuItem::new("删除").icon(IconName::Delete).on_click(move |_, window, cx| {
            if let Some(workspace) = delete.upgrade() {
                workspace.update(cx, |this, cx| {
                    this.request_delete_file_tree_path(path.clone(), window, cx);
                });
            }
        }))
}
