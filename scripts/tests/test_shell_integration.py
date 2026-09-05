from __future__ import annotations

import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.parse import quote

SCRIPTS = Path(__file__).resolve().parents[2] / "nebula_app/res/shell"


class ShellSession:
    def __init__(self, program: str, home: Path, args: list[str], env: dict[str, str]) -> None:
        import pty

        self.master, slave = pty.openpty()
        self.output = b""
        self.process = subprocess.Popen(
            [program, *args], stdin=slave, stdout=slave, stderr=slave, cwd=home,
            env={"PATH": os.environ["PATH"], "HOME": str(home), "TERM": "xterm-256color",
                 "LC_ALL": "en_US.UTF-8" if sys.platform == "darwin" else "C.UTF-8", "PS1": "nebula-test> ",
                 **{name: os.environ[name] for name in ("FPATH", "NEBULA_TEST_ZSH_MODULE_DIR") if name in os.environ},
                 **env},
            start_new_session=True,
        )
        os.close(slave)

    def wait(self, marker: bytes) -> bytes:
        collected = b""
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            if select.select([self.master], [], [], 0.1)[0]:
                try:
                    block = os.read(self.master, 65536)
                except OSError:
                    break
                collected += block
                self.output += block
                if marker in collected:
                    return collected
        raise AssertionError(f"shell never emitted {marker!r}: {self.output!r}")

    def command(self, command: str, marker: bytes) -> bytes:
        os.write(self.master, command.encode("utf-8") + b"\n")
        return self.wait(marker)

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        os.close(self.master)


@unittest.skipUnless(os.name == "posix", "requires Unix PTYs")
class ShellIntegrationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="nebula-shell-test-")
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)

    def start(self, shell: str, rc: str = "", original_zdotdir: Path | None = None) -> ShellSession:
        program = shutil.which(shell)
        if not program:
            self.skipTest(f"{shell} is not installed; native CI must run this case")
        if shell == "bash":
            if sys.platform == "darwin":
                self.skipTest("macOS Bash keeps native login startup; this rc wrapper is Linux-only")
            (self.home / ".bashrc").write_text(rc, encoding="utf-8")
            args = ["--noprofile", "--rcfile", str(SCRIPTS / "bashrc"), "-i"]
            env = {}
        else:
            dotfiles = original_zdotdir or self.home
            dotfiles.mkdir(exist_ok=True)
            (dotfiles / ".zshenv").write_text(
                '[[ -n $NEBULA_TEST_ZSH_MODULE_DIR ]] && module_path=("$NEBULA_TEST_ZSH_MODULE_DIR" $module_path)\n'
                "export NEBULA_PROFILE_TEST=env\n", encoding="utf-8")
            (dotfiles / ".zprofile").write_text("NEBULA_PROFILE_TEST+=:profile\n", encoding="utf-8")
            (dotfiles / ".zshrc").write_text("NEBULA_PROFILE_TEST+=:rc\n" + rc, encoding="utf-8")
            (dotfiles / ".zlogin").write_text("NEBULA_PROFILE_TEST+=:login\n", encoding="utf-8")
            wrapper = self.home / "integration"
            wrapper.mkdir()
            for source, target in [("zshenv", ".zshenv"), ("zprofile", ".zprofile"), ("zshrc", ".zshrc")]:
                shutil.copyfile(SCRIPTS / source, wrapper / target)
            args = ["-d", "-l", "-i"]
            env = {"ZDOTDIR": str(wrapper), "NEBULA_ZSH_INTEGRATION": str(wrapper),
                   "NEBULA_ZDOTDIR_WAS_SET": "1" if original_zdotdir else "0"}
            if original_zdotdir:
                env["NEBULA_ORIGINAL_ZDOTDIR"] = str(original_zdotdir)
        session = ShellSession(program, self.home, args, env)
        self.addCleanup(session.close)
        session.wait(b"\x1b]133;A\x07")
        return session

    def check_protocol(self, shell: str) -> None:
        session = self.start(shell)
        output = session.command("(exit 7)", b"\x1b]133;D;7\x07")
        self.assertIn(b"\x1b]133;C\x07", output)
        directory = self.home / "目录 % # space"
        directory.mkdir()
        encoded = quote(str(directory), safe="/._~-").encode("ascii")
        session.command(f"cd '{directory}'", b"\x1b]7;file://localhost" + encoded + b"\x07")

    def test_bash_command_status_and_utf8_cwd(self) -> None:
        self.check_protocol("bash")

    def test_zsh_command_status_and_utf8_cwd(self) -> None:
        self.check_protocol("zsh")

    def test_zsh_loads_user_rcs_without_host_global_rcs(self) -> None:
        session = self.start("zsh")
        session.command('print -r -- "RCS=$options[rcs] GLOBAL_RCS=$options[globalrcs]"',
                        b"RCS=on GLOBAL_RCS=off")

    def test_bash_preserves_prompt_command(self) -> None:
        session = self.start("bash", "PROMPT_COMMAND=\"printf 'USER_PROMPT\\\\n'\"\n")
        session.command("(exit 7)", b"\x1b]133;D;7\x07")
        self.assertIn(b"USER_PROMPT", session.output)

    def test_bash_does_not_replace_user_debug_trap(self) -> None:
        session = self.start("bash", "trap 'printf USER_DEBUG' DEBUG\n")
        session.command("trap -p DEBUG", b"trap -- 'printf USER_DEBUG' DEBUG")
        self.assertIn(b"USER_DEBUG", session.output)

    def test_bash_preserves_array_prompt_command(self) -> None:
        session = self.start("bash", "PROMPT_COMMAND=('printf FIRST_PROMPT' 'printf SECOND_PROMPT')\n")
        session.command("(exit 7)", b"\x1b]133;D;7\x07")
        self.assertIn(b"FIRST_PROMPT", session.output)
        self.assertIn(b"SECOND_PROMPT", session.output)

    def test_zsh_sources_login_files_and_restores_zdotdir(self) -> None:
        session = self.start("zsh")
        session.command('printf "PROFILE=%s ZDOTDIR=%s\\n" "$NEBULA_PROFILE_TEST" "${ZDOTDIR-unset}"',
                        b"PROFILE=env:profile:rc:login ZDOTDIR=unset")

    def test_zsh_preserves_custom_zdotdir(self) -> None:
        directory = self.home / "custom dotfiles"
        session = self.start("zsh", original_zdotdir=directory)
        session.command('printf "PROFILE=%s ZDOTDIR=%s_END\\n" "$NEBULA_PROFILE_TEST" "$ZDOTDIR"',
                        f"PROFILE=env:profile:rc:login ZDOTDIR={directory}_END".encode())

    def test_zsh_preserves_precmd_hooks_after_failure(self) -> None:
        session = self.start("zsh", """
typeset -gi user_prompt_count=0
_user_precmd() {
    local status_code=$?
    (( ++user_prompt_count ))
    printf 'USER_PRECMD_%s_STATUS=%s_END\\n' "$user_prompt_count" "$status_code"
}
precmd_functions=(_user_precmd)
""")
        output = session.command("(exit 7)", b"USER_PRECMD_2_STATUS=7_END")
        self.assertIn(b"\x1b]133;D;7\x07", output)
        session.command("true", b"USER_PRECMD_3_STATUS=0_END")


if __name__ == "__main__":
    unittest.main()
