use super::*;

#[derive(Clone)]
pub(super) struct ShellSelectItem {
    pub(super) id: String,
    pub(super) name: SharedString,
    pub(super) closed_image: Option<Arc<RenderImage>>,
    pub(super) row_image: Option<Arc<RenderImage>>,
}

/// 下拉首行那条「导入终端目录」动作行的哨兵 id。
///
/// 它不是任何真实 shell：选中只触发目录扫描，绝不会写进 `shell` 设置。
/// 取这个形状是因为 `shell_detect` 的 id 全是普通标识符（`pwsh`、`cmd`、
/// `wsl:<distro>`、`profile:<家族>|<id>`），双下划线包裹不可能与之相撞。
pub(super) const SHELL_IMPORT_ACTION_ID: &str = "__nebula_import_terminal_dir__";

/// 菜单行的逻辑边长（品牌贴图与图标槽同用）。
pub(super) const SHELL_ROW_ICON_SIZE: f32 = 24.0;

/// 回落字形的字号系数：见 `shell_detect::FALLBACK_ICON_SCALE`。
const FALLBACK_ICON_SCALE: f32 = crate::shell_detect::FALLBACK_ICON_SCALE;

/// 真实 shell 行之间的垂直间距。
///
/// `gpui-component` 的虚拟列表不支持条目 `gap_y`（`list.rs` 里明确写了），
/// 每行背景按同一槽高首尾相接。可用的唯一着力点：它只量**首行**的槽高再拿
/// 这个高度排所有行。给置顶的「导入终端目录」动作行加一段下内边距，槽高就
/// 比普通行多出这一段，余下每一行之间便空出同样的间隔；动作行自身也在文字
/// 下方留出这段空白，选中高亮不再贴着它。
pub(super) const SHELL_ROW_GAP: f32 = 4.0;

impl ShellSelectItem {
    pub(super) fn new(id: String, name: String, scale_factor: f32) -> Self {
        // Select 的闭态和菜单行尺寸不同。分别生成与物理像素一一对应的纹理，
        // 避免把 128px 原图交给 GPUI 在每帧缩小而产生模糊边缘。
        let closed_image = crate::gpui_shell::widgets::shell_brand_image(&id, 20.0, scale_factor);
        let row_image = crate::gpui_shell::widgets::shell_brand_image(&id, 24.0, scale_factor);
        Self { id, name: name.into(), closed_image, row_image }
    }

    /// 置顶的导入行。没有品牌贴图，[`Self::view`] 会给它文件夹图标。
    pub(super) fn import_action(language: crate::display::UiLanguage) -> Self {
        Self {
            id: SHELL_IMPORT_ACTION_ID.to_owned(),
            name: language.pick("导入终端目录…", "Import terminal directory...").into(),
            closed_image: None,
            row_image: None,
        }
    }

    pub(super) fn is_import_action(&self) -> bool {
        self.id == SHELL_IMPORT_ACTION_ID
    }

    pub(super) fn view(&self, size: f32, image: Option<&Arc<RenderImage>>) -> gpui::AnyElement {
        let icon: gpui::AnyElement = if let Some(image) = image {
            gpui::StyledImage::object_fit(
                img(image.clone()).size(px(size)).flex_shrink_0(),
                gpui::ObjectFit::Contain,
            )
            .into_any_element()
        } else if self.is_import_action() {
            // 动作行与真实 shell 行必须一眼分得开：文件夹口 = 「去别处拿」。
            Icon::new(IconName::FolderOpen).size(px(size * FALLBACK_ICON_SCALE)).into_any_element()
        } else {
            // 没有品牌贴图的 shell 沿用旧壳那张按 id 取字的 Nerd Font 表
            // （`icon_for_id`，设置行/命令面板同一口径）。字号按回落字形的
            // 墨迹比例配平：品牌 PNG 的可见部分是自己边长的约 0.77（12%
            // 安全边距），codicon terminal 的墨迹约 0.88 em——乘 0.88 之后
            // 两种图标才真的同尺寸。
            div()
                .font_family(crate::font_install::REQUIRED_FONT_FAMILY)
                .text_size(px(size * FALLBACK_ICON_SCALE))
                .child(crate::shell_detect::icon_for_id(&self.id))
                .into_any_element()
        };
        h_flex()
            .gap_2()
            .items_center()
            // 图标槽固定成同一个边长，绝不随图标种类伸缩：gpui-component 的
            // `List` 只量首行（这里是「导入终端目录」动作行）的高度，再拿它
            // 排所有行；品牌 PNG 有 24px、回落图标只有 xsmall，混排时高行会
            // 溢出自己的槽位，压住上下相邻行（选中高亮错位就是这么来的）。
            .child(
                div()
                    .size(px(size))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon),
            )
            .child(div().flex_1().min_w_0().truncate().child(self.name.clone()))
            .into_any_element()
    }
}

/// 默认 Shell 下拉的全部候选，以及应当选中的行号。
///
/// 顺序 = 置顶导入行 → 已安装 shell（`detect_shells` 菜单序）→ 用户导入的
/// 终端 profile。导入项以 `profile:<家族>|<id>` 作设置值（[`Profile::settings_id`]
/// 同一形状），品牌图标因此仍能按家族查到。
pub(super) fn shell_select_items(
    current: &str,
    scale_factor: f32,
    language: crate::display::UiLanguage,
) -> (Vec<ShellSelectItem>, usize) {
    let mut items: Vec<ShellSelectItem> = crate::shell_detect::detect_shells()
        .into_iter()
        .map(|shell| ShellSelectItem::new(shell.id, shell.name, scale_factor))
        .collect();
    if items.is_empty() {
        // 非 Windows 构建不做安装探测，但历史配置仍支持这两个由 PTY
        // 集成层负责启动的稳定 id，设置页不能因此变成空下拉。
        items = vec![
            ShellSelectItem::new("powershell".into(), "PowerShell".into(), scale_factor),
            ShellSelectItem::new("bash".into(), "Git Bash".into(), scale_factor),
        ];
    }
    // 导入的终端目录：`merge_terminal_profiles` 已把它们并进配置的 profile
    // 列表，这里让设置页也能直接选为默认 Shell——否则导入完看不见结果。
    if let Ok(store) = crate::terminal_profiles::TerminalProfiles::load() {
        for profile in store.as_config_profiles() {
            let Some(id) = profile.settings_id() else { continue };
            if items.iter().any(|item| item.id == id) {
                continue;
            }
            items.push(ShellSelectItem::new(id, profile.name, scale_factor));
        }
    }
    if !items.iter().any(|item| item.id == current) {
        // 检测结果可能暂时找不到已保存的 WSL/profile id；先把它保留在首位，
        // 用户仍可看到并重新选择，下一次检测恢复后不会丢失持久化值。
        items.insert(
            0,
            ShellSelectItem::new(
                current.to_owned(),
                crate::shell_detect::display_name_for_id(current).to_owned(),
                scale_factor,
            ),
        );
    }
    let selected = items.iter().position(|item| item.id == current).unwrap_or(0);
    // 导入行最后才插到首位：它不参与选中判定，所以选中行号整体后移一位。
    items.insert(0, ShellSelectItem::import_action(language));
    (items, selected + 1)
}

/// 扫描目录并落盘（阻塞 IO，调用方须放后台执行器）。
/// 逻辑与旧壳 `Display::import_terminal_directory` 逐句对齐，只是把 toast
/// 换成 `Result`，由 UI 线程决定怎么呈现。
pub(super) fn import_terminal_directory_blocking(
    directory: &std::path::Path,
) -> Result<usize, TerminalImportError> {
    let found = crate::terminal_profiles::scan_directory(directory)
        .map_err(|error| TerminalImportError::Scan(error.to_string()))?;
    if found.is_empty() {
        return Err(TerminalImportError::NoSupportedTerminal);
    }
    let mut profiles = crate::terminal_profiles::TerminalProfiles::load()
        .map_err(|error| TerminalImportError::Load(error.to_string()))?;
    let count = found.len();
    for profile in found {
        profiles.upsert(profile).map_err(|error| TerminalImportError::Import(error.to_string()))?;
    }
    profiles.save().map_err(|error| TerminalImportError::Save(error.to_string()))?;
    Ok(count)
}

/// 原生模态对话框不能在 GPUI update 借用中运行：它自己的消息泵会重入
/// wndproc，造成 AppCell 二次可变借用。与 SSH 私钥选择器一样，先在 UI
/// 线程捕获 HWND，再让专用线程运行旧壳的 IFileOpenDialog。
#[cfg(windows)]
pub(super) fn pick_folder_with_wsl_places(
    window: &Window,
    title: &'static str,
) -> futures::channel::oneshot::Receiver<Option<std::path::PathBuf>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let owner = HasWindowHandle::window_handle(window)
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as usize),
            _ => None,
        })
        .unwrap_or(0);
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let selected = crate::display::file_dialog::pick_folder_with_hwnd(owner as _, title);
        let _ = tx.send(selected);
    });
    rx
}

impl SelectItem for ShellSelectItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn display_title(&self) -> Option<gpui::AnyElement> {
        // 闭态留出 chevron 与上下内边距；20px 在 32px Select 内不会挤字。
        Some(self.view(20.0, self.closed_image.as_ref()))
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // 旧壳 ShellPickerRow 的品牌图标是 24×24 逻辑像素。
        let row = div().child(self.view(SHELL_ROW_ICON_SIZE, self.row_image.as_ref()));
        if self.is_import_action() {
            // 见 SHELL_ROW_GAP：首行独自垫高，成为所有行的槽高，才谈得上
            // 「行与行之间有间隔」。
            row.pb(px(SHELL_ROW_GAP))
        } else {
            row
        }
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(&query.to_lowercase())
            || self.id.to_lowercase().contains(&query.to_lowercase())
    }
}
