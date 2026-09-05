use std::path::Path;
use std::process::{Command, Stdio};

use super::Platform;

pub fn open(path: &Path) -> std::io::Result<()> {
    open_command(Platform::current(), path).spawn().map(|_| ())
}

pub fn reveal(path: &Path) -> std::io::Result<()> {
    match reveal_command(Platform::current(), path) {
        Some(mut command) => command.spawn().map(|_| ()),
        None => Ok(()),
    }
}

fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

fn open_command(platform: Platform, path: &Path) -> Command {
    let program = match platform {
        Platform::Windows => "explorer.exe",
        Platform::MacOS => "open",
        Platform::Linux => "xdg-open",
    };
    let mut command = command(program);
    command.arg(path);
    command
}

fn reveal_command(platform: Platform, path: &Path) -> Option<Command> {
    let mut command = match platform {
        Platform::Windows => {
            let mut command = command("explorer.exe");
            let mut select = std::ffi::OsString::from("/select,");
            select.push(path.as_os_str());
            command.arg(select);
            return Some(command);
        },
        Platform::MacOS => command("open"),
        Platform::Linux => return path.parent().map(|parent| open_command(platform, parent)),
    };
    command.arg("-R").arg(path);
    Some(command)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn open_keeps_the_path_as_one_argument_on_every_platform() {
        let path = Path::new("project with spaces").join("file.txt");
        for (platform, program) in [
            (Platform::Windows, "explorer.exe"),
            (Platform::MacOS, "open"),
            (Platform::Linux, "xdg-open"),
        ] {
            let command = open_command(platform, &path);
            assert_eq!(command.get_program(), program);
            assert_eq!(command.get_args().collect::<Vec<_>>(), [path.as_os_str()]);
        }
    }

    #[test]
    fn reveal_preserves_platform_selection_semantics() {
        let path = Path::new("project with spaces").join("file.txt");
        let mut select = OsString::from("/select,");
        select.push(&path);
        for (platform, program, arguments) in [
            (Platform::Windows, "explorer.exe", vec![select]),
            (Platform::MacOS, "open", vec![OsString::from("-R"), path.clone().into_os_string()]),
            (Platform::Linux, "xdg-open", vec![path.parent().unwrap().as_os_str().to_owned()]),
        ] {
            let command = reveal_command(platform, &path).unwrap();
            assert_eq!(command.get_program(), program);
            assert_eq!(command.get_args().collect::<Vec<_>>(), arguments);
        }
        assert!(reveal_command(Platform::Linux, Path::new("")).is_none());
    }
}
