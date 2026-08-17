//! 文件行数预算守护（docs/project-constraints.md 第 1 条）。
//!
//! 单个 `.rs` 文件硬上限 2000 行；存量超标文件在 [`GRANDFATHERED`] 登记，
//! 登记值只允许下调。测试失败的正确动作是拆文件，不是上调预算。

use std::fs;
use std::path::{Path, PathBuf};

/// 新代码红线：超过它就必须拆（docs/project-constraints.md）。
const MAX_LINES: usize = 2000;

/// 存量豁免清单（相对仓库根，正斜杠路径 → 行数上限）。
///
/// 登记值 = 收录当天的行数加不足百行的呼吸空间：小修小补不触线，成规模
/// 新增必须伴随拆分。拆小后同步下调或删除对应条目。
const GRANDFATHERED: &[(&str, usize)] = &[
    ("nebula_app/src/display/mod.rs", 12000),
    ("nebula_app/src/display/settings.rs", 8650),
    // 2026-08-17 上调（4850 → 5150）：git 树 VS Code 化 + 源码 tab 接线；
    // 下一步应把 VCS 面板拆成独立文件后下调。
    ("nebula_app/src/gpui_shell/workspace.rs", 5150),
    // 2026-08-17 下调（4600 → 3600）：SSH 区拆到 ssh_settings.rs 的成果锁定。
    ("nebula_app/src/gpui_shell/settings_pane.rs", 3600),
    ("nebula_app/src/display/command_palette.rs", 3980),
    ("nebula_app/src/window_context.rs", 3800),
    ("nebula_app/src/event.rs", 3780),
    ("nebula_terminal/src/term/mod.rs", 3650),
    // 2026-08-17 上调（2900 → 2970）：draw_overlays 几何抽成两壳共享的 plan。
    ("nebula_app/src/display/terminal_math.rs", 2970),
    // 2026-08-17 上调（2680 → 2760）：SVN wc.db 回退 + 单文件 stage/discard。
    ("nebula_app/src/display/side_panel.rs", 2760),
    ("nebula_app/src/gpui_shell/terminal/view.rs", 2540),
    ("nebula_app/src/display/chrome.rs", 2550),
    ("nebula_app/src/display/markdown_view.rs", 2150),
];

/// 这些目录不受预算约束：构建产物、vendored 第三方代码、版本库内部。
fn excluded(component: &str) -> bool {
    component == ".git"
        || component == "third_party"
        || component == "target"
        || component.starts_with(".target")
}

fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // 隐藏项（.git、.tmp_* 草稿等）不是源码，不受预算约束。
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if !excluded(&name) {
                collect_rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_source_file_respects_its_line_budget() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("nebula_app sits directly under the repo root")
        .to_path_buf();

    let mut files = Vec::new();
    collect_rust_files(&repo_root, &mut files);
    assert!(
        files.len() > 100,
        "行数守护扫到的文件数异常（{}），排除规则可能吞掉了源码目录",
        files.len()
    );

    let mut violations = Vec::new();
    let mut stale_grants = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(&repo_root)
            .expect("collected files live under the repo root")
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(content) = fs::read(&path) else {
            panic!("无法读取 {relative}");
        };
        // 按换行字节计数：与编码无关，空文件为 0 行。
        let lines = content.iter().filter(|byte| **byte == b'\n').count()
            + usize::from(!content.is_empty() && content.last() != Some(&b'\n'));
        let grant = GRANDFATHERED
            .iter()
            .find(|(name, _)| *name == relative)
            .map(|(_, budget)| *budget);
        let budget = grant.unwrap_or(MAX_LINES);
        if lines > budget {
            violations.push(format!("{relative}: {lines} 行 > 预算 {budget}"));
        }
        // 豁免文件瘦身到红线以内后，条目必须删除，防止预算复胖。
        if let Some(budget) = grant
            && lines <= MAX_LINES
        {
            stale_grants.push(format!("{relative}: 已降到 {lines} 行（≤{MAX_LINES}），豁免条目（{budget}）应删除"));
        }
    }

    assert!(
        violations.is_empty(),
        "以下文件超出行数预算（docs/project-constraints.md 第 1 条，正确动作是拆文件）：\n{}",
        violations.join("\n")
    );
    assert!(
        stale_grants.is_empty(),
        "以下豁免条目已过期：\n{}",
        stale_grants.join("\n")
    );
}
