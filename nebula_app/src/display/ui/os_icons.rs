//! 主机图标目录：一台 SSH 主机在侧栏里的那个形状。
//!
//! # 为什么每一项都带着墨迹宽度
//!
//! 这些图标是 Nerd Font 字形，而 Nerd Font 的图标**墨迹比它的步进宽得多**：
//! 步进统一是 0.6 em（一个等宽格），墨迹却在 0.76 em（树莓派）到 1.20 em
//! （Kali）之间。也就是说同一串码位画出来，最窄的占 1.27 格、最宽的占 2.00
//! 格——差了 57%。
//!
//! 侧栏的排版约定是「图标在第 0 列、文字在第 2 列」（"\u{eb51} 设置" 这类
//! 标签就是靠字形 + 空格拼出来的）。把 2.00 格的墨迹塞进这个约定，它的右缘
//! 正好顶到文字上；1.27 格的又在中间空出一大块。逐个手调偏移是治不完的，
//! 因为宽度是**字体给的**，不是我们排的。
//!
//! 所以这里把宽度当数据存起来：`ink_em` 是从 `MapleMonoNormal-NF-CN` 的
//! glyf 轮廓量出来的真实包围盒宽度（`fontTools` 的 BoundsPen，单位 em）。
//! 画的时候按它反解一个缩放系数，让**每个图标的墨迹都等于同一个目标宽度**
//! ——也就是 AI logo 那个 `cell_h * 0.72` 的方框。于是主机行的图标和标签行
//! 的品牌 logo 同宽、同列、同一条中线，文字列也就自然对齐了。
//!
//! 换字体要重新量：这些数字属于那一个字体文件，不属于码位。
//!
//! # 空心优先
//!
//! 非品牌的那几个（机架、云、终端）一律取**空心**字形：codicon / MDI 的
//! outline 一族，而不是 Font Awesome 的实心版。实心图标在侧栏这个尺寸上就是
//! 一块墨，形状只剩外轮廓，还比旁边的文字重；空心的把内部结构留出来，墨量
//! 也压得住。发行版和 macOS/Windows 那些是**品牌标志**，它们本身就是实心
//! 剪影，改成空心反而认不出来了——所以这条只管我们自己挑的那几个。

/// 一个可选图标。`id` 是写进 `ssh_profiles.json` 的稳定标识，不随语言变。
#[derive(Debug, Clone, Copy)]
pub(crate) struct OsIcon {
    pub(crate) id: &'static str,
    pub(crate) glyph: char,
    /// 字形墨迹包围盒的宽度，单位 em（步进是 0.6 em）。
    pub(crate) ink_em: f32,
    pub(crate) zh: &'static str,
    pub(crate) en: &'static str,
}

/// 没选过图标、也还没认出系统时用的那个。
pub(crate) const DEFAULT_ID: &str = "term";

/// 「跟着远端走」：连上以后按 `/etc/os-release` 认，认出来之前用
/// [`DEFAULT_ID`] 的形状顶着。存的是这个 id 而不是认出来的结果，这样换了机
/// 器上的系统它会自己跟着变——手动选过的就不再动。
pub(crate) const AUTO_ID: &str = "auto";

/// 目录里发行版与平台两段的分界（前者是「远端是什么系统」，后者是「它是什么
/// 角色」）。选择器按这个位置分组，侧栏不关心。
pub(crate) const PLATFORM_SPLIT: usize = 13;

pub(crate) const CATALOG: &[OsIcon] = &[
    OsIcon { id: "linux", glyph: '\u{f17c}', ink_em: 0.76, zh: "通用 Linux", en: "Linux" },
    OsIcon { id: "ubuntu", glyph: '\u{f31b}', ink_em: 0.94, zh: "Ubuntu", en: "Ubuntu" },
    OsIcon { id: "debian", glyph: '\u{f306}', ink_em: 0.80, zh: "Debian", en: "Debian" },
    OsIcon { id: "centos", glyph: '\u{f304}', ink_em: 1.00, zh: "CentOS", en: "CentOS" },
    OsIcon { id: "rhel", glyph: '\u{f316}', ink_em: 1.00, zh: "Red Hat", en: "Red Hat" },
    OsIcon { id: "fedora", glyph: '\u{f30a}', ink_em: 1.00, zh: "Fedora", en: "Fedora" },
    OsIcon { id: "rocky", glyph: '\u{f32b}', ink_em: 1.00, zh: "Rocky", en: "Rocky" },
    OsIcon { id: "alpine", glyph: '\u{f300}', ink_em: 0.95, zh: "Alpine", en: "Alpine" },
    OsIcon { id: "arch", glyph: '\u{f303}', ink_em: 1.00, zh: "Arch", en: "Arch" },
    OsIcon { id: "suse", glyph: '\u{f314}', ink_em: 1.00, zh: "openSUSE", en: "openSUSE" },
    OsIcon { id: "nixos", glyph: '\u{f313}', ink_em: 1.00, zh: "NixOS", en: "NixOS" },
    OsIcon { id: "kali", glyph: '\u{f327}', ink_em: 1.20, zh: "Kali", en: "Kali" },
    OsIcon { id: "freebsd", glyph: '\u{f30c}', ink_em: 0.88, zh: "FreeBSD", en: "FreeBSD" },
    // ↑ 发行版 / ↓ 平台与角色（PLATFORM_SPLIT）
    OsIcon { id: "macos", glyph: '\u{f302}', ink_em: 0.84, zh: "macOS", en: "macOS" },
    OsIcon { id: "windows", glyph: '\u{f17a}', ink_em: 0.81, zh: "Windows", en: "Windows" },
    OsIcon { id: "rpi", glyph: '\u{f315}', ink_em: 0.78, zh: "树莓派", en: "Raspberry Pi" },
    OsIcon { id: "docker", glyph: '\u{f308}', ink_em: 0.99, zh: "容器", en: "Container" },
    OsIcon { id: "server", glyph: '\u{eb50}', ink_em: 0.75, zh: "机架 / 裸金属", en: "Rack" },
    OsIcon { id: "cloud", glyph: '\u{f0163}', ink_em: 1.00, zh: "云主机", en: "Cloud" },
    OsIcon { id: "term", glyph: '\u{f489}', ink_em: 1.00, zh: "通用终端", en: "Terminal" },
];

/// 按 id 取图标。认不出的 id（旧配置、手改坏的）落回默认形状而不是消失：
/// 一行主机突然没了图标，比它戴着一个笼统但正确的图标更让人以为出了错。
pub(crate) fn resolve(id: Option<&str>) -> &'static OsIcon {
    let id = id.unwrap_or(DEFAULT_ID);
    let id = if id == AUTO_ID { DEFAULT_ID } else { id };
    CATALOG.iter().find(|icon| icon.id == id).unwrap_or_else(|| {
        CATALOG.iter().find(|icon| icon.id == DEFAULT_ID).expect("term in catalog")
    })
}

/// 把这个字形画成 `target_px` 宽所需要的缩放系数。
///
/// 墨迹宽 = `ink_em / 0.6` 个格子，所以原生像素宽是 `ink_em / 0.6 * cell_w`。
/// 反解即可。上下限是防呆：字体换了、量错了的时候，宁可图标大小不齐，也不
/// 要一个被压成一条线或者糊出行外的形状。
pub(crate) fn scale_for(icon: &OsIcon, cell_w: f32, target_px: f32) -> f32 {
    const ADVANCE_EM: f32 = 0.6;
    let native = (icon.ink_em / ADVANCE_EM) * cell_w;
    if native <= f32::EPSILON {
        return 1.0;
    }
    (target_px / native).clamp(0.35, 2.0)
}

/// 选择器弹出列表里的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerRow {
    /// 分组标题，不可点。
    Group(&'static str),
    /// 可选项：`None` = 「自动识别」，`Some(i)` = `CATALOG[i]`。
    Option(Option<usize>),
}

/// 按关键词过滤出的列表行（含分组标题）。
///
/// 带搜索是因为**记得名字的人比记得形状的人多**——二十一个剪影摊在一起，
/// 认出 Debian 的漩涡要比想起"debian"慢。关键词同时匹配 id、中文名、英文
/// 名：id 是配置里那个键，有人是照着配置文件找过来的。
///
/// 分组标题只在这一组**真有命中**时才发出去，否则筛完会剩下一串标题下面
/// 空无一物，看起来像列表坏了。
pub(crate) fn picker_rows(needle: &str, zh: bool) -> Vec<PickerRow> {
    let needle = needle.trim().to_lowercase();
    let hit = |haystack: &[&str]| {
        needle.is_empty() || haystack.iter().any(|s| s.to_lowercase().contains(&needle))
    };

    let mut rows = Vec::with_capacity(CATALOG.len() + 3);
    // 「自动识别」不属于任何一组：它不是一个形状，是"别管这个字段"。放在
    // 最顶上，和它作为默认值的身份一致。
    if hit(&["auto", "自动", "自动识别", "automatic"]) {
        rows.push(PickerRow::Option(None));
    }
    for (title, range) in [
        (if zh { "发行版" } else { "DISTRO" }, 0..PLATFORM_SPLIT),
        (if zh { "平台 / 角色" } else { "PLATFORM / ROLE" }, PLATFORM_SPLIT..CATALOG.len()),
    ] {
        let mut header_done = false;
        for index in range {
            let icon = &CATALOG[index];
            if !hit(&[icon.id, icon.zh, icon.en]) {
                continue;
            }
            if !header_done {
                rows.push(PickerRow::Group(title));
                header_done = true;
            }
            rows.push(PickerRow::Option(Some(index)));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_auto_ids_fall_back_to_the_default_shape() {
        assert_eq!(resolve(None).id, DEFAULT_ID);
        assert_eq!(resolve(Some("auto")).id, DEFAULT_ID);
        assert_eq!(resolve(Some("no-such-distro")).id, DEFAULT_ID);
        assert_eq!(resolve(Some("debian")).id, "debian");
    }

    #[test]
    fn every_icon_scales_to_the_same_ink_width() {
        // 这才是整张表存在的理由：同一个目标宽度下，最窄和最宽的字形画出来
        // 必须一样宽。差一像素以内算齐（缩放系数是浮点）。
        let cell_w = 12.0;
        let target = 18.0;
        for icon in CATALOG {
            let drawn = (icon.ink_em / 0.6) * cell_w * scale_for(icon, cell_w, target);
            assert!(
                (drawn - target).abs() < 1.0,
                "{} 画出来是 {drawn:.2}px，目标 {target}px",
                icon.id
            );
        }
    }

    #[test]
    fn ids_are_unique_and_split_lands_between_the_two_halves() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|icon| icon.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "图标 id 重复了——它是写进配置的键");
        assert_eq!(CATALOG[PLATFORM_SPLIT - 1].id, "freebsd");
        assert_eq!(CATALOG[PLATFORM_SPLIT].id, "macos");
    }

    #[test]
    fn picker_lists_every_icon_under_two_group_titles() {
        let rows = picker_rows("", true);
        let options = rows.iter().filter(|r| matches!(r, PickerRow::Option(_))).count();
        let groups = rows.iter().filter(|r| matches!(r, PickerRow::Group(_))).count();
        assert_eq!(options, CATALOG.len() + 1, "目录里每个图标各一行，外加「自动识别」");
        assert_eq!(groups, 2, "发行版 / 平台各一条标题");
        assert_eq!(rows[0], PickerRow::Option(None), "「自动识别」在最顶上");
    }

    #[test]
    fn picker_matches_by_id_and_by_either_language() {
        for needle in ["debian", "Debian", "DEB"] {
            let rows = picker_rows(needle, true);
            let picked: Vec<_> = rows
                .iter()
                .filter_map(|r| match r {
                    PickerRow::Option(Some(i)) => Some(CATALOG[*i].id),
                    _ => None,
                })
                .collect();
            assert_eq!(picked, ["debian"], "「{needle}」应当只命中 debian");
        }
        // 中文名和英文名各自都能搜到——用哪种语言想事的人都不必改语言设置。
        assert!(!picker_rows("树莓", true).is_empty());
        assert!(!picker_rows("raspberry", true).is_empty());
    }

    /// 筛空的组不该留下一条孤零零的标题——那看起来像列表坏了。
    #[test]
    fn picker_drops_group_titles_that_have_no_hits() {
        let rows = picker_rows("ubuntu", true);
        assert_eq!(rows.iter().filter(|r| matches!(r, PickerRow::Group(_))).count(), 1);
        assert!(picker_rows("没有这种系统", true).is_empty(), "全不命中就该是空列表");
    }
}
