use gpui::SharedString;

/// 固定下拉只在这里把稳定 value 映射为显示文案。创建和语言切换刷新共用
/// 同一入口，避免 `SelectState` 留着构造时的旧语言。
pub(super) fn localized_select_labels(
    key: &str,
    values: &[&'static str],
    language: crate::display::UiLanguage,
) -> Vec<SharedString> {
    let labels: Vec<&'static str> = match key {
        "language" => nebula_settings::LanguagePref::ALL
            .iter()
            .map(|preference| {
                if *preference == nebula_settings::LanguagePref::System {
                    language.tr("language.system")
                } else {
                    preference.native_name()
                }
            })
            .collect(),
        "cursor_shape" => vec![
            language.pick("条形（│）", "Bar (│)"),
            language.pick("下划线（_）", "Underscore (_)"),
            language.pick("实心框（█）", "Filled box (█)"),
            language.pick("空心框（□）", "Empty box (□)"),
        ],
        "tabs_position" => {
            vec![language.pick("左侧边栏", "Left sidebar"), language.pick("顶部", "Top")]
        },
        "tab_reveal" => vec![language.pick("滑动", "Slide"), language.pick("立即", "Instant")],
        "density" => vec![language.pick("标准", "Standard"), language.pick("紧凑", "Compact")],
        "new_tab_position" => vec![
            language.pick("当前标签之后", "After current tab"),
            language.pick("列表末尾", "End of list"),
        ],
        "windowing_behavior" => vec![
            language.pick("创建新窗口", "Create a new window"),
            language.pick("附加到最近使用的窗口", "Attach to the most recent window"),
            language.pick(
                "附加到此桌面最近使用的窗口",
                "Attach to the most recent window on this desktop",
            ),
        ],
        "vcs_display" => vec![
            language.pick("自动检测", "Auto detect"),
            language.pick("仅 Git", "Git only"),
            language.pick("仅 SVN", "SVN only"),
        ],
        "cell_width_mode" => {
            vec![language.pick("紧凑", "Compact"), language.pick("宽松", "Relaxed")]
        },
        "bell" => vec![
            language.pick("关", "Off"),
            language.pick("闪烁", "Visual"),
            language.pick("声音", "Sound"),
            language.pick("闪烁 + 声音", "Visual + sound"),
        ],
        "blur" => vec![
            language.pick("无", "None"),
            language.pick("Mica（低开销）", "Mica (low cost)"),
            language.pick("Mica Alt（低开销）", "Mica Alt (low cost)"),
            language.pick("Aero（玻璃）", "Aero (glass)"),
            language.pick("Acrylic（高开销）", "Acrylic (high cost)"),
        ],
        "accept" => vec![
            language.pick("右方向键", "Right arrow"),
            "Tab",
            language.pick("Tab 或右方向键", "Tab or Right arrow"),
        ],
        "completion_style" => {
            vec![language.pick("行内灰字", "Inline ghost"), language.pick("弹窗列表", "Popup list")]
        },
        "background_image_fit" => vec![
            language.pick("拉伸", "Fill"),
            language.pick("适应", "Uniform"),
            language.pick("填充", "Uniform to fill"),
            language.pick("原始尺寸", "Original size"),
        ],
        "background_image_alignment" => vec![
            language.pick("左上", "Top left"),
            language.pick("顶部", "Top"),
            language.pick("右上", "Top right"),
            language.pick("左侧", "Left"),
            language.pick("居中", "Center"),
            language.pick("右侧", "Right"),
            language.pick("左下", "Bottom left"),
            language.pick("底部", "Bottom"),
            language.pick("右下", "Bottom right"),
        ],
        "ssh_proxy_mode" => vec![
            language.tr("settings.network.mode.off"),
            language.tr("settings.network.mode.system"),
            language.tr("settings.network.mode.custom"),
        ],
        _ => values.to_vec(),
    };
    debug_assert_eq!(labels.len(), values.len(), "localized select label/value mismatch: {key}");
    labels.into_iter().map(SharedString::from).collect()
}

pub(super) fn provider_input_placeholders(
    language: crate::display::UiLanguage,
) -> [&'static str; 5] {
    [
        language.pick("供应商名称", "Provider name"),
        language.pick("备注（可包含空格）", "Note (spaces allowed)"),
        language.pick("官方网站", "Official website"),
        language.pick("API 请求地址", "API endpoint"),
        language.pick("默认模型", "Default model"),
    ]
}

pub(super) fn localized_input_placeholder(
    key: &str,
    language: crate::display::UiLanguage,
) -> &'static str {
    match key {
        "ssh_label" => language.pick("例如：开发服务器", "e.g. Development server"),
        "ssh_password" => language.pick("留空则连接时询问", "Leave empty to ask when connecting"),
        "ssh_proxy_username" => language.pick("可选", "Optional"),
        "ssh_proxy_password" => {
            language.pick("留空则保留已存密码", "Leave empty to keep the saved password")
        },
        "ssh_jump_host" => language
            .pick("user@bastion:22 或 SSH config 别名", "user@bastion:22 or SSH config alias"),
        "ssh_icon_filter" => language.pick("搜索图标…", "Search icons..."),
        "font_family" => language.pick("输入字体名称", "Enter a font family"),
        "backup_password" => {
            language.pick("备份密码（至少 8 位）", "Backup password (at least 8 characters)")
        },
        "backup_secret" => language.tr("settings.input.backup_secret"),
        "keymap_search" => language.pick("搜索动作或按键…", "Search actions or keys..."),
        _ => "",
    }
}
