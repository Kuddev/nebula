use crate::gpui_shell::ssh_settings::SshValidationError;

#[derive(Clone, Debug, Default)]
pub(super) enum AboutUpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate(String),
    Available(String),
    Failed(String),
}

#[derive(Clone, Debug)]
pub(super) enum ProviderStatus {
    Saved,
    Added,
    AtLeastOneRequired,
    Deleted,
    ApiKeySaved,
    Testing,
    TestResult { outcome: crate::provider_test::ProviderTestOutcome, elapsed_ms: u64 },
    CodexConfirmation,
    AppliedToCodex(std::path::PathBuf),
    Error(String),
}

impl ProviderStatus {
    pub(in crate::gpui_shell) fn is_error(&self) -> bool {
        match self {
            Self::AtLeastOneRequired | Self::Error(_) => true,
            Self::TestResult { outcome, .. } => !outcome.is_success(),
            _ => false,
        }
    }

    pub(in crate::gpui_shell) fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Saved => language.pick("供应商配置已保存", "Provider settings saved").into(),
            Self::Added => language.pick("已添加自定义供应商", "Custom provider added").into(),
            Self::AtLeastOneRequired => {
                language.pick("至少保留一个供应商", "Keep at least one provider").into()
            },
            Self::Deleted => language
                .pick("供应商及其凭据已删除", "Provider and its credentials deleted")
                .into(),
            Self::ApiKeySaved => language
                .pick(
                    "API Key 已保存到系统凭据管理器",
                    "API key saved to the system credential manager",
                )
                .into(),
            Self::Testing => language.pick("正在测试连接…", "Testing connection...").into(),
            Self::TestResult { outcome, elapsed_ms } => {
                format!("{} · {elapsed_ms} ms", language.provider_test_message(outcome))
            },
            Self::CodexConfirmation => language
                .pick(
                    "再次点击确认：API Key 将明文写入 Codex auth.json（原文件会备份）",
                    "Click again to confirm: the API key will be written in plain text to Codex auth.json (the original file will be backed up)",
                )
                .into(),
            Self::AppliedToCodex(path) => format!(
                "{}: {}",
                language.pick("已应用到 Codex", "Applied to Codex"),
                path.display()
            ),
            Self::Error(error) => format!(
                "{}: {error}",
                language.pick("操作失败", "Operation failed")
            ),
        }
    }
}
#[derive(Clone, Debug)]
pub(super) enum BackupCompletion {
    Exported(std::path::PathBuf),
    Restored,
    Pushed(String),
    Pulled(String),
}

#[derive(Clone, Debug)]
pub(super) enum BackupStatus {
    PassphraseTooShort,
    SelectionRequired,
    Processing,
    RemoteConfigSaved,
    CredentialEmpty,
    CredentialUnsupported,
    CredentialSaved,
    Completed(BackupCompletion),
    Error(String),
}

impl BackupStatus {
    pub(in crate::gpui_shell) fn is_error(&self) -> bool {
        matches!(
            self,
            Self::PassphraseTooShort
                | Self::SelectionRequired
                | Self::CredentialEmpty
                | Self::CredentialUnsupported
                | Self::Error(_)
        )
    }

    pub(in crate::gpui_shell) fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::PassphraseTooShort => language
                .pick("备份密码至少 8 位", "The backup password must be at least 8 characters")
                .into(),
            Self::SelectionRequired => language
                .pick("请至少勾选一个备份类别", "Select at least one backup category")
                .into(),
            Self::Processing => language.pick("处理中…", "Processing...").into(),
            Self::RemoteConfigSaved => {
                language.pick("远端配置已保存", "Remote configuration saved").into()
            },
            Self::CredentialEmpty => {
                language.pick("凭据不能为空", "Credentials cannot be empty").into()
            },
            Self::CredentialUnsupported => language
                .pick(
                    "当前协议不需要独立凭据",
                    "The current protocol does not use a separate credential",
                )
                .into(),
            Self::CredentialSaved => language
                .pick(
                    "凭据已写入系统凭据管理器",
                    "Credential saved to the system credential manager",
                )
                .into(),
            Self::Completed(BackupCompletion::Exported(path)) => format!(
                "{}: {}",
                language.pick("已导出加密备份", "Encrypted backup exported"),
                path.display()
            ),
            Self::Completed(BackupCompletion::Restored) => language
                .pick(
                    "已从备份恢复（字体/托盘等部分设置重启后生效）",
                    "Backup restored (some settings, including fonts and tray options, apply after restart)",
                )
                .into(),
            Self::Completed(BackupCompletion::Pushed(location)) => {
                format!("{} {location}", language.pick("已推送到", "Pushed to"))
            },
            Self::Completed(BackupCompletion::Pulled(name)) => format!(
                "{} {name} {}",
                language.pick("已从", "Restored from"),
                language.pick("恢复（部分设置重启后生效）", "(some settings apply after restart)")
            ),
            Self::Error(error) => format!(
                "{}: {error}",
                language.pick("备份操作失败", "Backup operation failed")
            ),
        }
    }
}

#[derive(Debug)]
pub(super) enum TerminalImportError {
    Scan(String),
    NoSupportedTerminal,
    Load(String),
    Import(String),
    Save(String),
}

impl TerminalImportError {
    pub(super) fn text(self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Scan(error) => format!(
                "{}: {error}",
                language.pick("无法扫描终端目录", "Could not scan the terminal directory")
            ),
            Self::NoSupportedTerminal => language
                .pick(
                    "目录中未找到受支持的终端程序",
                    "No supported terminal program was found in the directory",
                )
                .into(),
            Self::Load(error) => format!(
                "{}: {error}",
                language.pick("无法读取终端配置", "Could not read terminal profiles")
            ),
            Self::Import(error) => format!(
                "{}: {error}",
                language.pick("无法导入终端", "Could not import the terminal")
            ),
            Self::Save(error) => format!(
                "{}: {error}",
                language.pick("无法保存终端配置", "Could not save terminal profiles")
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::gpui_shell) enum SshStatus {
    Saved(String),
    Pinned,
    Imported(usize),
    Opening(String),
    DeleteCommitted { hidden_config: bool },
    CleanupPartial(String),
    Restored(String),
    Validation(SshValidationError),
    PersistFailed(String),
    DeleteFailed(String),
    UndoFailed(String),
    TestStartFailed(String),
    ProfileLoadFailed(String),
    ProfileSaveFailed(String),
    HostListSaveFailed(String),
    CredentialSaveFailed(String),
    SavedWithCleanupError { destination: String, error: String },
    Error(String),
}

impl SshStatus {
    pub(in crate::gpui_shell) fn is_error(&self) -> bool {
        matches!(
            self,
            Self::CleanupPartial(_)
                | Self::Validation(_)
                | Self::PersistFailed(_)
                | Self::DeleteFailed(_)
                | Self::UndoFailed(_)
                | Self::TestStartFailed(_)
                | Self::ProfileLoadFailed(_)
                | Self::ProfileSaveFailed(_)
                | Self::HostListSaveFailed(_)
                | Self::CredentialSaveFailed(_)
                | Self::SavedWithCleanupError { .. }
                | Self::Error(_)
        )
    }

    pub(in crate::gpui_shell) fn text(&self, language: crate::display::UiLanguage) -> String {
        match self {
            Self::Saved(destination) => {
                format!("{} {destination}", language.pick("已保存", "Saved"))
            },
            Self::Pinned => language.pick("置顶状态已更新", "Pin status updated").into(),
            Self::Imported(count) => format!(
                "{} {count} {}",
                language.pick("已导入，config 源共", "Imported"),
                language.pick("个别名", "config aliases")
            ),
            Self::Opening(host) => {
                format!("{} {host}…", language.pick("正在打开", "Opening"))
            },
            Self::DeleteCommitted { hidden_config: true } => language
                .pick(
                    "已隐藏 config 别名，并清理 Pebrel Profile 与凭据",
                    "Config alias hidden; Pebrel profile and credentials removed",
                )
                .into(),
            Self::DeleteCommitted { hidden_config: false } => language
                .pick(
                    "已删除主机、Profile 与凭据",
                    "Host, profile, and credentials deleted",
                )
                .into(),
            Self::CleanupPartial(details) => format!(
                "{}: {details}",
                language.pick(
                    "主机已从列表移除，但部分清理失败",
                    "Host removed from the list, but some cleanup failed",
                )
            ),
            Self::Restored(host) => {
                format!("{} {host}", language.pick("已恢复", "Restored"))
            },
            Self::Validation(error) => error.text(language).into(),
            Self::PersistFailed(error) => format!(
                "{}: {error}",
                language.pick("写入设置失败", "Failed to write settings")
            ),
            Self::DeleteFailed(error) => format!(
                "{}: {error}",
                language.pick("删除主机失败", "Failed to delete host")
            ),
            Self::UndoFailed(error) => {
                format!("{}: {error}", language.pick("撤销失败", "Undo failed"))
            },
            Self::TestStartFailed(error) => format!(
                "{}: {error}",
                language.pick("无法启动连接测试", "Could not start the connection test")
            ),
            Self::ProfileLoadFailed(error) => format!(
                "{}: {error}",
                language.pick("加载 SSH Profile 失败", "Failed to load the SSH profile")
            ),
            Self::ProfileSaveFailed(error) => format!(
                "{}: {error}",
                language.pick("保存 SSH Profile 失败", "Failed to save the SSH profile")
            ),
            Self::HostListSaveFailed(error) => format!(
                "{}: {error}",
                language.pick("保存主机列表失败", "Failed to save the host list")
            ),
            Self::CredentialSaveFailed(error) => format!(
                "{}: {error}",
                language.pick(
                    "Profile 已保存，但密码写入凭据管理器失败",
                    "The profile was saved, but the password could not be written to the credential manager",
                )
            ),
            Self::SavedWithCleanupError { destination, error } => format!(
                "{} {destination}, {}: {error}",
                language.pick("已保存", "Saved"),
                language.pick(
                    "但旧地址凭据清理失败",
                    "but credentials for the previous address could not be removed",
                )
            ),
            Self::Error(error) => {
                format!("{}: {error}", language.pick("SSH 操作失败", "SSH operation failed"))
            },
        }
    }
}
