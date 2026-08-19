//! Transactional Git worktree creation for runtime-managed agents.
//!
//! Git commands run on the runtime client thread. This module never invokes a
//! shell and only rolls back resources that the current transaction proved it
//! created.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::runtime_api::ApiError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeProvenance {
    pub repo_root: PathBuf,
    pub source_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub created: bool,
}

#[derive(Debug)]
pub struct WorktreeRequest {
    pub source_cwd: PathBuf,
    pub agent_name: String,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub path: Option<PathBuf>,
    pub allow_dirty_source: bool,
}

#[derive(Debug)]
pub struct WorktreeTransaction {
    provenance: WorktreeProvenance,
    branch_owned: bool,
    worktree_owned: bool,
}

impl WorktreeTransaction {
    pub fn prepare(request: WorktreeRequest) -> Result<Self, ApiError> {
        if !request.source_cwd.is_absolute() {
            return Err(ApiError::invalid_params(
                "source_cwd must be an absolute path when source_pane_id is not used",
            ));
        }

        ensure_git_available()?;
        let source_root = git_text(
            &request.source_cwd,
            ["rev-parse", "--show-toplevel"],
            "not_a_git_worktree",
            "source_cwd is not inside a Git worktree",
        )?;
        let source_root = PathBuf::from(source_root);
        let common_dir = PathBuf::from(git_text(
            &source_root,
            ["rev-parse", "--path-format=absolute", "--git-common-dir"],
            "git_query_failed",
            "failed to resolve the repository common directory",
        )?);
        let repo_root = common_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
            ApiError::new(
                "unsupported_repository",
                "the Git common directory does not have a parent worktree",
            )
            .details(json!({ "git_common_dir": common_dir }))
        })?;

        if !request.allow_dirty_source {
            let status = git_text(
                &source_root,
                ["status", "--porcelain=v1", "--untracked-files=normal"],
                "git_query_failed",
                "failed to inspect source worktree changes",
            )?;
            if !status.is_empty() {
                return Err(ApiError::new(
                    "dirty_source",
                    "source worktree has uncommitted changes; commit them or explicitly allow a dirty source",
                )
                .details(json!({ "source_root": source_root, "status": status })));
            }
        }

        let base = request.base.as_deref().unwrap_or("HEAD");
        if base.trim().is_empty() || base.chars().any(char::is_control) {
            return Err(ApiError::invalid_params("base must be a non-empty Git revision"));
        }
        let verify = format!("{base}^{{commit}}");
        let base_commit = git_text_os(
            &source_root,
            [
                OsString::from("rev-parse"),
                OsString::from("--verify"),
                OsString::from("--end-of-options"),
                OsString::from(verify),
            ],
            "invalid_base",
            "base does not resolve to a Git commit",
        )?;

        let slug = worktree_slug(&request.agent_name);
        let branch = request.branch.unwrap_or_else(|| format!("nebula/{slug}"));
        validate_branch(&branch)?;
        ensure_branch_absent(&source_root, &branch)?;

        let path = match request.path {
            Some(path) => {
                if !path.is_absolute() {
                    return Err(ApiError::invalid_params("path must be absolute"));
                }
                path
            },
            None => {
                let repo_name = repo_root.file_name().ok_or_else(|| {
                    ApiError::new(
                        "unsupported_repository",
                        "the main repository worktree does not have a directory name",
                    )
                })?;
                let parent = repo_root.parent().ok_or_else(|| {
                    ApiError::new(
                        "unsupported_repository",
                        "the main repository worktree does not have a parent directory",
                    )
                })?;
                let mut container = repo_name.to_os_string();
                container.push("-worktrees");
                parent.join(container).join(&slug)
            },
        };
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(ApiError::new(
                    "worktree_path_conflict",
                    "the target worktree path already exists",
                )
                .details(json!({ "path": path })));
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(ApiError::new(
                    "worktree_path_inspection_failed",
                    "failed to inspect the target worktree path",
                )
                .details(json!({ "path": path, "reason": error.to_string() })));
            },
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ApiError::new(
                    "worktree_parent_create_failed",
                    "failed to create the worktree parent directory",
                )
                .details(json!({ "path": parent, "reason": error.to_string() }))
            })?;
        }

        let provenance =
            WorktreeProvenance { repo_root, source_root, path, branch, base_commit, created: true };
        let mut transaction = Self { provenance, branch_owned: false, worktree_owned: false };

        transaction.create_branch()?;
        if let Err(error) = transaction.create_worktree() {
            // 分支创建是独立成功步骤，因此这里能证明分支归本事务所有；
            // worktree add 未明确成功时绝不直接删除目标目录。
            let rollback = transaction.rollback_branch_only();
            return Err(attach_rollback(error, rollback));
        }
        Ok(transaction)
    }

    pub fn provenance(&self) -> &WorktreeProvenance {
        &self.provenance
    }

    pub fn commit(mut self) -> WorktreeProvenance {
        self.branch_owned = false;
        self.worktree_owned = false;
        self.provenance
    }

    pub fn rollback(mut self) -> Result<(), ApiError> {
        if self.worktree_owned {
            let output = run_git_os(
                &self.provenance.repo_root,
                [
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    OsString::from("--force"),
                    self.provenance.path.as_os_str().to_os_string(),
                ],
            )?;
            if !output.status.success() {
                return Err(git_failure(
                    "worktree_rollback_failed",
                    "failed to remove the worktree created by this request",
                    &output,
                    json!({ "path": self.provenance.path }),
                ));
            }
            self.worktree_owned = false;
        }
        self.rollback_branch_only()
    }

    fn create_branch(&mut self) -> Result<(), ApiError> {
        let output = run_git_os(
            &self.provenance.source_root,
            [
                OsString::from("branch"),
                OsString::from("--no-track"),
                self.provenance.branch.clone().into(),
                self.provenance.base_commit.clone().into(),
            ],
        )?;
        if !output.status.success() {
            return Err(git_failure(
                "branch_create_failed",
                "failed to create the worktree branch",
                &output,
                json!({
                    "branch": self.provenance.branch,
                    "base_commit": self.provenance.base_commit
                }),
            ));
        }
        self.branch_owned = true;
        Ok(())
    }

    fn create_worktree(&mut self) -> Result<(), ApiError> {
        let output = run_git_os(
            &self.provenance.repo_root,
            [
                OsString::from("worktree"),
                OsString::from("add"),
                self.provenance.path.as_os_str().to_os_string(),
                self.provenance.branch.clone().into(),
            ],
        )?;
        if !output.status.success() {
            return Err(git_failure(
                "worktree_create_failed",
                "failed to create the Git worktree",
                &output,
                json!({
                    "path": self.provenance.path,
                    "branch": self.provenance.branch
                }),
            ));
        }
        self.worktree_owned = true;
        Ok(())
    }

    fn rollback_branch_only(&mut self) -> Result<(), ApiError> {
        if !self.branch_owned {
            return Ok(());
        }
        let output = run_git_os(
            &self.provenance.repo_root,
            [
                OsString::from("branch"),
                OsString::from("-D"),
                OsString::from("--"),
                self.provenance.branch.clone().into(),
            ],
        )?;
        if !output.status.success() {
            return Err(git_failure(
                "branch_rollback_failed",
                "failed to remove the branch created by this request",
                &output,
                json!({ "branch": self.provenance.branch }),
            ));
        }
        self.branch_owned = false;
        Ok(())
    }
}

fn ensure_git_available() -> Result<(), ApiError> {
    let output = Command::new("git").arg("--version").output().map_err(|error| {
        ApiError::new("git_unavailable", "Git executable is unavailable")
            .details(json!({ "reason": error.to_string() }))
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_failure(
            "git_unavailable",
            "Git executable could not report its version",
            &output,
            json!({}),
        ))
    }
}

fn validate_branch(branch: &str) -> Result<(), ApiError> {
    if branch.trim() != branch || branch.is_empty() || branch.chars().any(char::is_control) {
        return Err(ApiError::invalid_params("branch is not a valid Git branch name"));
    }
    let output = Command::new("git")
        .args(["check-ref-format", "--branch", branch])
        .output()
        .map_err(|error| {
            ApiError::new("git_unavailable", "Git executable is unavailable")
                .details(json!({ "reason": error.to_string() }))
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_failure(
            "invalid_branch",
            "branch is not a valid Git branch name",
            &output,
            json!({ "branch": branch }),
        ))
    }
}

fn ensure_branch_absent(cwd: &Path, branch: &str) -> Result<(), ApiError> {
    let reference = format!("refs/heads/{branch}");
    let output = run_git_os(
        cwd,
        [
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            reference.into(),
        ],
    )?;
    match output.status.code() {
        Some(1) => Ok(()),
        Some(0) => {
            Err(ApiError::new("branch_conflict", "the target worktree branch already exists")
                .details(json!({ "branch": branch })))
        },
        _ => Err(git_failure(
            "git_query_failed",
            "failed to determine whether the target branch exists",
            &output,
            json!({ "branch": branch }),
        )),
    }
}

fn git_text<const N: usize>(
    cwd: &Path,
    args: [&str; N],
    code: &str,
    message: &str,
) -> Result<String, ApiError> {
    git_text_os(cwd, args.map(OsString::from), code, message)
}

fn git_text_os<const N: usize>(
    cwd: &Path,
    args: [OsString; N],
    code: &str,
    message: &str,
) -> Result<String, ApiError> {
    let output = run_git_os(cwd, args)?;
    if !output.status.success() {
        return Err(git_failure(code, message, &output, json!({ "cwd": cwd })));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run_git_os<I, S>(cwd: &Path, args: I) -> Result<Output, ApiError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git").arg("-C").arg(cwd).args(args).output().map_err(|error| {
        ApiError::new("git_unavailable", "failed to execute Git")
            .details(json!({ "cwd": cwd, "reason": error.to_string() }))
    })
}

fn git_failure(code: &str, message: &str, output: &Output, context: serde_json::Value) -> ApiError {
    ApiError::new(code, message).details(json!({
        "status": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout).trim(),
        "stderr": String::from_utf8_lossy(&output.stderr).trim(),
        "context": context
    }))
}

fn attach_rollback(mut error: ApiError, rollback: Result<(), ApiError>) -> ApiError {
    if let Err(rollback_error) = rollback {
        error.details = Some(json!({
            "operation": error.details,
            "rollback": rollback_error
        }));
    }
    error
}

fn worktree_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in name.chars() {
        let accepted = character.is_alphanumeric() || matches!(character, '-' | '_' | '.');
        if accepted {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            if slug.len() + character.len_utf8() > 48 {
                break;
            }
            slug.extend(character.to_lowercase());
        } else {
            separator = true;
        }
    }
    let trimmed = slug.trim_matches(['-', '_', '.']).to_owned();
    if trimmed.is_empty() { format!("agent-{:08x}", stable_hash(name)) } else { trimmed }
}

fn stable_hash(value: &str) -> u32 {
    value
        .as_bytes()
        .iter()
        .fold(2_166_136_261_u32, |hash, byte| (hash ^ u32::from(*byte)).wrapping_mul(16_777_619))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_path_and_branch_safe() {
        assert_eq!(worktree_slug("Fix Login / Windows"), "fix-login-windows");
        assert_eq!(worktree_slug("  审查 API  "), "审查-api");
        assert!(worktree_slug("///").starts_with("agent-"));
    }

    #[test]
    fn transaction_creates_and_rolls_back_owned_resources() {
        let repository = test_repository();
        let target = repository.path().join("worktrees").join("reviewer");
        let transaction = WorktreeTransaction::prepare(WorktreeRequest {
            source_cwd: repository.path().to_path_buf(),
            agent_name: "reviewer".into(),
            branch: Some("nebula/test-reviewer".into()),
            base: None,
            path: Some(target.clone()),
            allow_dirty_source: false,
        })
        .expect("worktree should be created");
        assert!(target.join("tracked.txt").is_file());
        transaction.rollback().expect("owned worktree should roll back");
        assert!(!target.exists());
        assert!(
            !git_output(
                repository.path(),
                ["show-ref", "--verify", "--quiet", "refs/heads/nebula/test-reviewer"]
            )
            .status
            .success()
        );
    }

    #[test]
    fn dirty_source_requires_explicit_opt_in() {
        let repository = test_repository();
        std::fs::write(repository.path().join("untracked.txt"), "dirty")
            .expect("write untracked file");
        let error = WorktreeTransaction::prepare(WorktreeRequest {
            source_cwd: repository.path().to_path_buf(),
            agent_name: "reviewer".into(),
            branch: Some("nebula/dirty-reviewer".into()),
            base: None,
            path: Some(repository.path().join("worktrees").join("dirty")),
            allow_dirty_source: false,
        })
        .expect_err("dirty source must be rejected");
        assert_eq!(error.code, "dirty_source");
    }

    fn test_repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("create repository directory");
        assert!(git_output(directory.path(), ["init", "--initial-branch=main"]).status.success());
        std::fs::write(directory.path().join("tracked.txt"), "tracked")
            .expect("write tracked file");
        assert!(git_output(directory.path(), ["add", "tracked.txt"]).status.success());
        assert!(
            git_output(
                directory.path(),
                [
                    "-c",
                    "user.name=Nebula Test",
                    "-c",
                    "user.email=nebula@example.invalid",
                    "commit",
                    "-m",
                    "initial"
                ]
            )
            .status
            .success()
        );
        directory
    }

    fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
        Command::new("git").arg("-C").arg(cwd).args(args).output().expect("run git")
    }
}
