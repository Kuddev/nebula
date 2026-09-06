use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const POLICY: &str = include_str!("../../architecture/file-budgets.txt");

fn line_count(content: &[u8]) -> usize {
    content.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(!content.is_empty() && content.last() != Some(&b'\n'))
}

fn collect_sources(root: &Path, files: &mut Vec<PathBuf>) {
    assert!(!root.is_symlink(), "source directory must not be a symlink: {}", root.display());
    let cargo_root = root.join("Cargo.toml").is_file();
    for entry in fs::read_dir(root).expect("source directories must be readable") {
        let entry = entry.expect("source directory entries must be readable");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "node_modules" || name == "__pycache__" || (cargo_root && name == "target") {
            continue;
        }
        let path = entry.path();
        assert!(!path.is_symlink(), "source must not be a symlink: {}", path.display());
        if path.is_dir() {
            collect_sources(&path, files);
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| matches!(value, "rs" | "py" | "ps1" | "mjs" | "js" | "sh" | "lua"))
        {
            files.push(path);
        }
    }
}

#[test]
fn every_source_file_respects_its_line_budget() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut limit = None;
    let mut roots = HashSet::new();
    let mut exceptions = HashMap::new();
    for record in POLICY.lines().filter(|record| !record.trim().is_empty()) {
        let fields = record.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 2, "invalid budget record: {record}");
        match fields[0] {
            "limit" => {
                assert!(limit.is_none(), "duplicate default limit");
                limit = Some(fields[1].parse::<usize>().unwrap());
            },
            "root" => assert!(roots.insert(fields[1]), "duplicate source root"),
            name => {
                let count = fields[1].parse::<usize>().unwrap();
                assert!(exceptions.insert(name, count).is_none(), "duplicate exception: {name}");
            },
        }
    }
    let limit = limit.expect("missing default limit");
    assert!(limit > 0 && !roots.is_empty());
    let mut files = Vec::new();
    for root in roots {
        assert!(!Path::new(root).is_absolute() && !root.contains("..") && !root.contains(':'));
        let mut directory = repo_root.to_path_buf();
        for component in Path::new(root).components() {
            directory.push(component);
            assert!(!directory.is_symlink(), "source ancestor must not be a symlink");
        }
        collect_sources(&repo_root.join(root), &mut files);
    }
    assert!(files.len() > 100, "unexpectedly empty source scan: {}", files.len());
    files.sort();
    let mut errors = Vec::new();
    for path in files {
        let name = path.strip_prefix(repo_root).unwrap().to_string_lossy().replace('\\', "/");
        let content = fs::read(&path).expect("source files must be readable");
        let count = line_count(&content);
        let exception = exceptions.remove(name.as_str());
        let budget = exception.unwrap_or(limit);
        if count > budget {
            errors.push(format!("{name}: {count} lines > {budget}; split by responsibility"));
        }
        if exception.is_some() && count <= limit {
            errors.push(format!("{name}: remove the obsolete exception ({count} lines)"));
        }
    }
    for name in exceptions.keys() {
        errors.push(format!("{name}: remove the missing-source exception"));
    }
    assert!(errors.is_empty(), "docs/project-constraints.md:\n{}", errors.join("\n"));
}

#[test]
fn physical_lines_ignore_encoding_and_newline_convention() {
    for (content, expected) in [
        ("", 0),
        ("\n", 1),
        ("a\nb", 2),
        ("a\r\nb\r\n", 2),
        ("中文\n", 1),
        ("vertical\u{000b}tab", 1),
    ] {
        assert_eq!(line_count(content.as_bytes()), expected);
    }
}
