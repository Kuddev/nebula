//! Store application notices as outcomes so a cached result follows the UI language.
//! Command output and user content remain verbatim.

use crate::i18n::{Message, UiLanguage};

#[derive(Clone, Debug)]
pub(crate) enum PanelNotice {
    Raw(String),
    DirectoryUnavailable,
    FollowingDirectory,
    CommandFailed(String),
    TaskStartFailed(String),
    PathUnavailable,
    DeleteFailed(String),
    IgnoreAdded(String),
    IgnoreRemoved(String),
    IgnorePresent(String),
    IgnoreVisible(String),
    IgnoreFailed(String),
}

impl Default for PanelNotice {
    fn default() -> Self {
        Self::Raw(String::new())
    }
}

impl From<String> for PanelNotice {
    fn from(value: String) -> Self {
        Self::Raw(value)
    }
}

impl From<&str> for PanelNotice {
    fn from(value: &str) -> Self {
        Self::Raw(value.to_owned())
    }
}

impl PanelNotice {
    pub(super) fn text(&self, language: UiLanguage) -> String {
        match self {
            Self::Raw(text) => text.clone(),
            Self::DirectoryUnavailable => {
                language.text(Message::VcsDirectoryUnavailable).to_owned()
            },
            Self::FollowingDirectory => language.text(Message::VcsFollowingDirectory).to_owned(),
            Self::CommandFailed(program) => {
                language.format(Message::VcsCommandFailed, &[("program", program)])
            },
            Self::TaskStartFailed(error) => {
                language.format(Message::VcsTaskStartFailed, &[("error", error)])
            },
            Self::PathUnavailable => language.text(Message::FilesPathUnavailable).to_owned(),
            Self::DeleteFailed(error) => {
                language.format(Message::FilesDeleteFailed, &[("error", error)])
            },
            Self::IgnoreAdded(entry) => {
                language.format(Message::FilesIgnoreAdded, &[("entry", entry)])
            },
            Self::IgnoreRemoved(entry) => {
                language.format(Message::FilesIgnoreRemoved, &[("entry", entry)])
            },
            Self::IgnorePresent(entry) => {
                language.format(Message::FilesIgnorePresent, &[("entry", entry)])
            },
            Self::IgnoreVisible(entry) => {
                language.format(Message::FilesIgnoreVisible, &[("entry", entry)])
            },
            Self::IgnoreFailed(error) => {
                language.format(Message::FilesIgnoreFailed, &[("error", error)])
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_notices_follow_language_changes_without_repeating_the_operation() {
        let notice = PanelNotice::FollowingDirectory;
        assert_eq!(notice.text(UiLanguage::ZhCn), "所选目录不可用，已跟随当前目录");
        assert_eq!(
            notice.text(UiLanguage::EnUs),
            "The selected directory is unavailable; following the current directory"
        );
        assert_eq!(notice.text(UiLanguage::FrFr), notice.text(UiLanguage::EnUs));
        assert_eq!(
            PanelNotice::DirectoryUnavailable.text(UiLanguage::EnUs),
            "The selected directory is unavailable"
        );
        assert_eq!(PanelNotice::CommandFailed("git".into()).text(UiLanguage::EnUs), "git failed");
        assert_eq!(PanelNotice::CommandFailed("git".into()).text(UiLanguage::ZhCn), "git 失败");
    }

    #[test]
    fn external_errors_and_placeholder_like_arguments_are_preserved() {
        let stderr = "fatal: 无法读取 分支/{error}";
        for language in UiLanguage::ALL {
            assert_eq!(PanelNotice::Raw(stderr.into()).text(*language), stderr);
        }
        let notice = PanelNotice::TaskStartFailed("OS error: {program}".into());
        assert_eq!(
            notice.text(UiLanguage::EnUs),
            "Could not start the version control task: OS error: {program}"
        );
    }

    #[test]
    fn file_notices_can_be_reused_by_the_git_drawer_in_a_new_language() {
        let cases = [
            (PanelNotice::PathUnavailable, "The path no longer exists"),
            (PanelNotice::DeleteFailed("denied".into()), "Could not delete: denied"),
            (PanelNotice::IgnoreAdded("/build/".into()), "Added to .gitignore: /build/"),
            (PanelNotice::IgnoreRemoved("/build/".into()), "No longer ignored: /build/"),
            (PanelNotice::IgnorePresent("/build/".into()), "Already ignored: /build/"),
            (PanelNotice::IgnoreVisible("/build/".into()), "Already visible: /build/"),
            (
                PanelNotice::IgnoreFailed("denied".into()),
                "Could not change Git ignore rules: denied",
            ),
        ];
        for (notice, english) in cases {
            assert_ne!(notice.text(UiLanguage::ZhCn), english);
            assert_eq!(notice.text(UiLanguage::EnUs), english);
        }
    }
}
