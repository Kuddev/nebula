use super::*;

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(directory.path())
            .status()
            .unwrap()
            .success()
    );
    directory
}

fn file(root: &Path, path: &str) -> PathBuf {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "fixture").unwrap();
    path
}

#[test]
fn remove_exact_rule_preserves_comments_crlf_and_other_paths() {
    let repo = repository();
    let root = repo.path();
    let path = file(root, "a[1].txt");
    let other = file(root, "other.txt");
    let ignore = root.join(".gitignore");
    std::fs::write(&ignore, "# keep\r\n/a\\[1\\].txt\r\n/other.txt").unwrap();
    assert!(matching_rule(root, &path).unwrap().is_some());
    assert!(matches!(remove_from_gitignore(&path, false).unwrap(), IgnoreOutcome::Removed { .. }));
    assert_eq!(std::fs::read_to_string(ignore).unwrap(), "# keep\r\n/other.txt");
    assert!(matching_rule(root, &path).unwrap().is_none());
    assert!(matching_rule(root, &other).unwrap().is_some());
    assert!(matches!(
        remove_from_gitignore(&path, false).unwrap(),
        IgnoreOutcome::AlreadyVisible { .. }
    ));
}

#[test]
fn wildcard_exceptions_and_parent_exceptions_keep_siblings_ignored() {
    for pattern in ["*.log\n", "/build/\n", "*\n"] {
        let repo = repository();
        let root = repo.path();
        let target = file(root, "build/deep/keep.log");
        let sibling = file(root, "build/deep/other.log");
        let cousin = file(root, "build/other.log");
        std::fs::write(root.join(".gitignore"), pattern).unwrap();
        remove_from_gitignore(&target, false).unwrap();
        assert!(matching_rule(root, &target).unwrap().is_none(), "{pattern}");
        assert!(matching_rule(root, &sibling).unwrap().is_some(), "{pattern}");
        assert!(matching_rule(root, &cousin).unwrap().is_some(), "{pattern}");
        assert!(std::fs::read_to_string(root.join(".gitignore")).unwrap().starts_with(pattern));
    }
}

#[test]
fn nested_rules_and_repository_excludes_are_handled_in_the_right_scope() {
    for source in ["sub/.gitignore", ".git/info/exclude"] {
        let repo = repository();
        let root = repo.path();
        let target = file(root, "sub/keep.log");
        let sibling = file(root, "sub/other.log");
        std::fs::write(root.join(source), "*.log\n").unwrap();
        remove_from_gitignore(&target, false).unwrap();
        assert!(matching_rule(root, &target).unwrap().is_none());
        assert!(matching_rule(root, &sibling).unwrap().is_some());
        assert!(std::fs::read_to_string(root.join(source)).unwrap().starts_with("*.log\n"));
    }
}

#[test]
fn exact_directory_rule_removal_restores_children() {
    let repo = repository();
    let root = repo.path();
    let child = file(root, "build/result.txt");
    append_to_gitignore(&root.join("build"), true).unwrap();
    remove_from_gitignore(&root.join("build"), true).unwrap();
    assert!(matching_rule(root, &child).unwrap().is_none());
}
