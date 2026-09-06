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
            Icon::new(IconName::FolderOpen).xsmall().into_any_element()
        } else {
            Icon::new(IconName::SquareTerminal).xsmall().into_any_element()
        };
        h_flex()
            .gap_2()
            .items_center()
            .child(icon)
            .child(div().flex_1().min_w_0().child(self.name.clone()))
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
        self.view(24.0, self.row_image.as_ref())
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn matches(&self, query: &str) -> bool {
        self.name.to_lowercase().contains(&query.to_lowercase())
            || self.id.to_lowercase().contains(&query.to_lowercase())
    }
}
