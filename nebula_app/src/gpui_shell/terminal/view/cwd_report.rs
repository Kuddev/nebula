//! Apply directory metadata without changing the reported filesystem path.

use nebula_terminal::event::Event;

pub(super) fn apply(current: &mut String, event: &Event) -> bool {
    let reported = match event {
        Event::CwdReport(path) => Some(path.as_str()),
        Event::Title(title) => {
            title.strip_prefix("NEBULA|").and_then(|rest| rest.split('|').next())
        },
        _ => None,
    };
    let Some(path) = reported else { return false };
    if path.chars().any(char::is_control)
        || !(path.starts_with('/') || std::path::Path::new(path).is_absolute())
        || current.as_str() == path
    {
        return false;
    }
    current.clear();
    current.push_str(path);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_events_preserve_absolute_paths_verbatim() {
        for path in ["/", "/srv/Team's \"App\" ", "/home/guest/new project"] {
            let mut current = "/original".to_owned();
            let report = Event::CwdReport(path.to_owned());
            assert!(apply(&mut current, &report));
            assert_eq!(current, path);
            assert!(!apply(&mut current, &report));
        }
    }

    #[test]
    fn shell_title_events_preserve_the_directory_field() {
        let mut current = "/original".to_owned();
        let path = "/srv/Team's \"App\" ";
        assert!(apply(&mut current, &Event::Title(format!("NEBULA|{path}| main | hx "))));
        assert_eq!(current, path);
        assert!(!apply(&mut current, &Event::Title("editor title".to_owned())));
        assert_eq!(current, path);
    }

    #[test]
    fn invalid_directory_events_keep_the_last_known_directory() {
        for path in ["", "relative", " /srv/app", "/srv/app\r", "\n/srv/app", "/srv/\x1bapp"] {
            for event in
                [Event::CwdReport(path.to_owned()), Event::Title(format!("NEBULA|{path}||"))]
            {
                let mut current = "/last known".to_owned();
                assert!(!apply(&mut current, &event), "{path:?}");
                assert_eq!(current, "/last known");
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn local_windows_directory_events_keep_drive_and_unc_paths() {
        for path in [r"C:\Users\Test User", r"\\server\share\project"] {
            let mut current = String::new();
            assert!(apply(&mut current, &Event::CwdReport(path.to_owned())));
            assert_eq!(current, path);
        }
    }
}
