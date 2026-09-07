use std::path::PathBuf;

use super::*;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir()
            .join(format!("pebrel-migration-test-{}-{timestamp}-{sequence}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn migration_preserves_all_source_data_and_absolute_configuration_imports() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    fs::create_dir_all(old.join("modules")).unwrap();
    fs::write(old.join("nebula_settings.txt"), "theme=Paper\n").unwrap();
    fs::write(old.join("history.db-wal"), b"pending transactions").unwrap();
    let import = old.join("modules/theme.lua");
    fs::write(&import, "return {}").unwrap();
    let config = format!("return dofile([[{}]])", import.display());
    fs::write(old.join("nebula.lua"), &config).unwrap();
    migrate_data_at(&old, &new).unwrap();
    migrate_data_at(&old, &new).unwrap();
    assert_eq!(fs::read_to_string(new.join("pebrel_settings.txt")).unwrap(), "theme=Paper\n");
    assert_eq!(fs::read(new.join("history.db-wal")).unwrap(), b"pending transactions");
    assert_eq!(fs::read_to_string(new.join("pebrel.lua")).unwrap(), config);
    assert_eq!(fs::read_to_string(&import).unwrap(), "return {}");
    assert_eq!(fs::read_to_string(new.join("modules/theme.lua")).unwrap(), "return {}");
    assert!(old.join("nebula_settings.txt").is_file());
}

#[test]
fn existing_destination_is_filled_without_overwriting_new_preferences() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    for dir in [&old, &new] {
        fs::create_dir(dir).unwrap();
    }
    fs::write(old.join("nebula_settings.txt"), "old").unwrap();
    fs::write(old.join("ssh_profiles.json"), "saved hosts").unwrap();
    fs::write(new.join("pebrel_settings.txt"), "new").unwrap();
    migrate_data_at(&old, &new).unwrap();
    assert_eq!(fs::read_to_string(new.join("pebrel_settings.txt")).unwrap(), "new");
    assert_eq!(fs::read_to_string(new.join("ssh_profiles.json")).unwrap(), "saved hosts");
    fs::remove_file(new.join("ssh_profiles.json")).unwrap();
    migrate_data_at(&old, &new).unwrap();
    assert!(!new.join("ssh_profiles.json").exists());
}

#[test]
fn portable_override_converts_legacy_names_once_and_prefers_existing_pebrel_files() {
    let temp = TestDir::new();
    fs::write(temp.join("nebula_settings.txt"), "old").unwrap();
    fs::write(temp.join("pebrel_settings.txt"), "new").unwrap();
    fs::write(temp.join("nebula.toml"), "import = []").unwrap();
    migrate_data_at(&temp.0, &temp.0).unwrap();
    assert_eq!(fs::read_to_string(temp.join("pebrel_settings.txt")).unwrap(), "new");
    assert_eq!(fs::read_to_string(temp.join("pebrel.toml")).unwrap(), "import = []");
    assert!(temp.join("nebula.toml").is_file());
}

#[test]
fn existing_pebrel_toml_cannot_be_shadowed_by_migrating_legacy_lua() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    for dir in [&old, &new] {
        fs::create_dir(dir).unwrap();
    }
    fs::write(old.join("nebula.lua"), "return {font = {size = 50}}").unwrap();
    fs::write(old.join("pebrel.lua"), "return {font = {size = 60}}").unwrap();
    fs::write(new.join("pebrel.toml"), "[font]\nsize=12").unwrap();
    migrate_data_at(&old, &new).unwrap();
    assert!(!new.join("pebrel.lua").exists());
    assert_eq!(fs::read_to_string(new.join("pebrel.toml")).unwrap(), "[font]\nsize=12");
    assert!(old.join("nebula.lua").is_file());
}

#[test]
fn simultaneous_initialization_observes_only_complete_files() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    fs::create_dir(&old).unwrap();
    let contents = "theme=Paper\n".repeat(65536);
    fs::write(old.join("nebula_settings.txt"), &contents).unwrap();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    migrate_data_at(&old, &new).unwrap();
                    assert_eq!(
                        fs::read_to_string(new.join("pebrel_settings.txt")).unwrap(),
                        contents
                    );
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    assert!(new.join(COMPLETE_FILE).is_file());
}

#[test]
fn failed_file_copy_does_not_publish_a_partial_file_and_can_be_retried() {
    let temp = TestDir::new();
    let path = temp.join("pebrel_settings.txt");
    assert!(
        write_if_absent(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("simulated interruption"))
        })
        .is_err()
    );
    assert!(!path.exists());
    write_if_absent(&path, |file| file.write_all(b"complete")).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "complete");
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn concurrent_destination_creation_is_never_overwritten() {
    let temp = TestDir::new();
    let path = temp.join("pebrel_settings.txt");
    write_if_absent(&path, |file| {
        fs::write(&path, b"new preference")?;
        file.write_all(b"legacy preference")
    })
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new preference");
}

#[cfg(windows)]
#[test]
fn windows_publication_renames_without_hard_links_and_refuses_existing_destinations() {
    let temp = TestDir::new();
    let source = temp.join("temporary");
    let target = temp.join("pebrel_settings.txt");
    fs::write(&source, "complete").unwrap();
    publish_file(&source, &target).unwrap();
    assert!(!source.exists(), "publication must move the temporary file, not create a hard link");
    assert_eq!(fs::read_to_string(&target).unwrap(), "complete");
    fs::write(&source, "different").unwrap();
    assert!(publish_file(&source, &target).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "complete");
    assert_eq!(fs::read_to_string(&source).unwrap(), "different");
}

#[test]
fn failure_is_reported_and_does_not_mark_the_migration_complete() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("nebula.lua"), "return {}").unwrap();
    fs::write(&new, "not a directory").unwrap();
    assert!(migrate_data_at(&old, &new).is_err());
    assert_eq!(fs::read_to_string(old.join("nebula.lua")).unwrap(), "return {}");
    assert!(!new.join(COMPLETE_FILE).exists());
}

#[cfg(unix)]
#[test]
fn destination_symlinks_are_never_followed_or_overwritten() {
    let temp = TestDir::new();
    let old = temp.join("Nebula");
    let new = temp.join("Pebrel");
    let outside = temp.join("unrelated");
    for dir in [&old, &new, &outside] {
        fs::create_dir(dir).unwrap();
    }
    fs::create_dir(old.join("fonts")).unwrap();
    fs::write(old.join("fonts/font.ttf"), b"font").unwrap();
    std::os::unix::fs::symlink(&outside, new.join("fonts")).unwrap();
    std::os::unix::fs::symlink(outside.join("missing"), new.join("pebrel_settings.txt")).unwrap();
    fs::write(old.join("nebula_settings.txt"), "old").unwrap();
    migrate_data_at(&old, &new).unwrap();
    assert!(!outside.join("font.ttf").exists());
    assert!(!outside.join("missing").exists());
    assert!(fs::symlink_metadata(new.join("pebrel_settings.txt")).unwrap().is_symlink());
}
