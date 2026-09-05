use nebula_terminal::tty;

pub fn prepare(options: &mut tty::Options) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        prepare_unix(options)
    }
    #[cfg(not(unix))]
    {
        let _ = options;
        Ok(())
    }
}

#[cfg(unix)]
fn prepare_unix(options: &mut tty::Options) -> std::io::Result<()> {
    use std::path::Path;

    let program = match &options.shell {
        Some(shell) => shell.program().to_owned(),
        None => tty::default_shell_program()?,
    };
    let args = options.shell.as_ref().map(|shell| shell.args()).unwrap_or_default();
    let name = Path::new(&program).file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if !supports(name, args) {
        return Ok(());
    }
    let root = super::dirs::data_dir().join("shell-integration");
    match name {
        "zsh" => {
            let directory = root.join("zsh");
            std::fs::create_dir_all(&directory)?;
            for (name, content) in [
                (".zshenv", include_str!("../../res/shell/zshenv")),
                (".zprofile", include_str!("../../res/shell/zprofile")),
                (".zshrc", include_str!("../../res/shell/zshrc")),
            ] {
                crate::atomic_file::write(&directory.join(name), content.as_bytes())?;
            }
            if let Some(original) = std::env::var_os("ZDOTDIR") {
                options.env.insert(
                    "NEBULA_ORIGINAL_ZDOTDIR".into(),
                    original.to_string_lossy().into_owned(),
                );
                options.env.insert("NEBULA_ZDOTDIR_WAS_SET".into(), "1".into());
            } else {
                options.env.insert("NEBULA_ZDOTDIR_WAS_SET".into(), "0".into());
            }
            let directory = directory.to_string_lossy().into_owned();
            options.env.insert("NEBULA_ZSH_INTEGRATION".into(), directory.clone());
            options.env.insert("ZDOTDIR".into(), directory);
        },
        "bash" => {
            std::fs::create_dir_all(&root)?;
            let init = root.join("bashrc");
            crate::atomic_file::write(&init, include_bytes!("../../res/shell/bashrc"))?;
            options.shell = Some(tty::Shell::new(
                program,
                vec!["--rcfile".into(), init.to_string_lossy().into_owned(), "-i".into()],
            ));
        },
        _ => {},
    }
    Ok(())
}

#[cfg(unix)]
fn supports(name: &str, args: &[String]) -> bool {
    match name {
        "zsh" => args.iter().all(|arg| matches!(arg.as_str(), "-l" | "--login" | "-i")),
        "bash" => !cfg!(target_os = "macos") && args.is_empty(),
        _ => false,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn custom_commands_and_rcfiles_are_never_rewritten() {
        for name in ["bash", "zsh", "fish"] {
            assert!(!supports(name, &["-c".into(), "echo test".into()]));
            assert!(!supports(name, &["--norc".into()]));
            assert!(!supports(name, &["--rcfile".into(), "custom".into()]));
        }
        assert!(supports("zsh", &["-l".into()]));
        assert_eq!(supports("bash", &[]), !cfg!(target_os = "macos"));
    }
}
