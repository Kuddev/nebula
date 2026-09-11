//! Built-in development recipes. They are templates inserted for review, never auto-executed.

use super::SavedCommand;
use crate::i18n::{Message, UiLanguage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandPlatform {
    Windows,
    Mac,
    Posix,
}

pub(crate) fn commands(language: UiLanguage, platform: CommandPlatform) -> Vec<SavedCommand> {
    use CommandPlatform::*;
    let mut commands = Vec::new();
    if platform == Windows {
        commands.push(SavedCommand {
            id: "builtin:docker_install_windows".into(),
            name: language.text(Message::CommandsBuiltinDockerInstallWindows).into(),
            command: "winget install --exact --id Docker.DockerDesktop".into(),
            append_enter: false,
        });
    }
    if platform == Mac {
        commands.push(SavedCommand {
            id: "builtin:docker_install_mac".into(),
            name: language.text(Message::CommandsBuiltinDockerInstallMac).into(),
            command: "brew install --cask docker".into(),
            append_enter: false,
        });
    }
    if platform == Posix {
        commands.push(SavedCommand {
            id: "builtin:docker_install_ubuntu".into(),
            name: language.text(Message::CommandsBuiltinDockerInstallUbuntu).into(),
            command: "sudo apt-get update && sudo apt-get install docker.io docker-compose-v2"
                .into(),
            append_enter: false,
        });
    }
    if platform == Posix {
        commands.push(SavedCommand {
            id: "builtin:docker_install_fedora".into(),
            name: language.text(Message::CommandsBuiltinDockerInstallFedora).into(),
            command: "sudo dnf install moby-engine docker-compose".into(),
            append_enter: false,
        });
    }
    commands.push(SavedCommand {
        id: "builtin:docker_ps".into(),
        name: language.text(Message::CommandsBuiltinDockerPs).into(),
        command: "docker ps -a".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:docker_images".into(),
        name: language.text(Message::CommandsBuiltinDockerImages).into(),
        command: "docker images".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:docker_build".into(),
        name: language.text(Message::CommandsBuiltinDockerBuild).into(),
        command: "docker build -t my-app:dev .".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:docker_run".into(),
        name: language.text(Message::CommandsBuiltinDockerRun).into(),
        command: "docker run --rm -it -p 8080:8080 my-app:dev".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:docker_logs".into(),
        name: language.text(Message::CommandsBuiltinDockerLogs).into(),
        command: "docker logs --follow --tail 100 container_name".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:docker_exec".into(),
        name: language.text(Message::CommandsBuiltinDockerExec).into(),
        command: "docker exec -it container_name sh".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:compose_up".into(),
        name: language.text(Message::CommandsBuiltinComposeUp).into(),
        command: "docker compose up -d --build".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:compose_logs".into(),
        name: language.text(Message::CommandsBuiltinComposeLogs).into(),
        command: "docker compose logs --follow --tail 100".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:compose_down".into(),
        name: language.text(Message::CommandsBuiltinComposeDown).into(),
        command: "docker compose down".into(),
        append_enter: false,
    });
    if platform == Windows {
        commands.push(SavedCommand {
            id: "builtin:conda_install_windows".into(),
            name: language.text(Message::CommandsBuiltinCondaInstallWindows).into(),
            command: "winget install --exact --id Anaconda.Miniconda3".into(),
            append_enter: false,
        });
    }
    if platform == Mac {
        commands.push(SavedCommand {
            id: "builtin:conda_install_mac".into(),
            name: language.text(Message::CommandsBuiltinCondaInstallMac).into(),
            command: "brew install --cask miniconda".into(),
            append_enter: false,
        });
    }
    if platform == Posix {
        commands.push(SavedCommand {
            id: "builtin:conda_install_linux".into(),
            name: language.text(Message::CommandsBuiltinCondaInstallLinux).into(),
            command: "curl -fL -o miniconda-installer.sh \"https://repo.anaconda.com/miniconda/Miniconda3-latest-Linux-$(uname -m).sh\" && bash miniconda-installer.sh".into(),
            append_enter: false,
        });
    }
    commands.push(SavedCommand {
        id: "builtin:conda_list".into(),
        name: language.text(Message::CommandsBuiltinCondaList).into(),
        command: "conda env list".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_create".into(),
        name: language.text(Message::CommandsBuiltinCondaCreate).into(),
        command: "conda create -n dev python=3.12".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_activate".into(),
        name: language.text(Message::CommandsBuiltinCondaActivate).into(),
        command: "conda activate dev".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_deactivate".into(),
        name: language.text(Message::CommandsBuiltinCondaDeactivate).into(),
        command: "conda deactivate".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_install".into(),
        name: language.text(Message::CommandsBuiltinCondaInstall).into(),
        command: "conda install numpy pandas matplotlib".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_export".into(),
        name: language.text(Message::CommandsBuiltinCondaExport).into(),
        command: "conda env export --from-history > environment.yml".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:conda_restore".into(),
        name: language.text(Message::CommandsBuiltinCondaRestore).into(),
        command: "conda env create -f environment.yml".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:python_venv".into(),
        name: language.text(Message::CommandsBuiltinPythonVenv).into(),
        command: "python -m venv .venv".into(),
        append_enter: false,
    });
    if platform == Windows {
        commands.push(SavedCommand {
            id: "builtin:python_activate_windows".into(),
            name: language.text(Message::CommandsBuiltinPythonActivateWindows).into(),
            command: ".\\.venv\\Scripts\\Activate.ps1".into(),
            append_enter: false,
        });
    }
    if platform != Windows {
        commands.push(SavedCommand {
            id: "builtin:python_activate_posix".into(),
            name: language.text(Message::CommandsBuiltinPythonActivatePosix).into(),
            command: "source .venv/bin/activate".into(),
            append_enter: false,
        });
    }
    commands.push(SavedCommand {
        id: "builtin:pip_install".into(),
        name: language.text(Message::CommandsBuiltinPipInstall).into(),
        command: "python -m pip install -r requirements.txt".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:uv_sync".into(),
        name: language.text(Message::CommandsBuiltinUvSync).into(),
        command: "uv sync".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:npm_install".into(),
        name: language.text(Message::CommandsBuiltinNpmInstall).into(),
        command: "npm ci".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:npm_dev".into(),
        name: language.text(Message::CommandsBuiltinNpmDev).into(),
        command: "npm run dev".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:pnpm_install".into(),
        name: language.text(Message::CommandsBuiltinPnpmInstall).into(),
        command: "pnpm install".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:git_status".into(),
        name: language.text(Message::CommandsBuiltinGitStatus).into(),
        command: "git status --short --branch".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:git_diff".into(),
        name: language.text(Message::CommandsBuiltinGitDiff).into(),
        command: "git diff".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:git_log".into(),
        name: language.text(Message::CommandsBuiltinGitLog).into(),
        command: "git log --oneline --graph --decorate -20".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:git_branch".into(),
        name: language.text(Message::CommandsBuiltinGitBranch).into(),
        command: "git switch -c feature/my-change".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:cargo_test".into(),
        name: language.text(Message::CommandsBuiltinCargoTest).into(),
        command: "cargo test".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:cargo_check".into(),
        name: language.text(Message::CommandsBuiltinCargoCheck).into(),
        command: "cargo check --all-targets".into(),
        append_enter: false,
    });
    commands.push(SavedCommand {
        id: "builtin:ssh_key".into(),
        name: language.text(Message::CommandsBuiltinSshKey).into(),
        command: "ssh-keygen -t ed25519 -C \"developer@example.com\"".into(),
        append_enter: false,
    });
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recipes_are_unique_reviewable_and_platform_specific() {
        for platform in [CommandPlatform::Windows, CommandPlatform::Mac, CommandPlatform::Posix] {
            let rows = commands(UiLanguage::EnUs, platform);
            assert!(rows.len() >= 30);
            let ids: std::collections::HashSet<_> = rows.iter().map(|row| &row.id).collect();
            assert_eq!(ids.len(), rows.len());
            assert!(rows.iter().all(|row| !row.append_enter && !row.command.contains('\n')));
            assert_eq!(
                rows.iter().any(|row| row.command.starts_with("winget ")),
                platform == CommandPlatform::Windows
            );
            assert!(rows.iter().any(|row| row.command == "conda create -n dev python=3.12"));
            assert!(rows.iter().any(|row| row.command == "docker compose up -d --build"));
        }
    }
}
