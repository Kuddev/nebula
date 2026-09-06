from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unittest
from unittest.mock import patch

SCRIPTS_DIR = Path(__file__).resolve().parents[2]
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from conformance.harness import ConformanceContext


SHELLS = ("powershell", "pwsh", "cmd", "fish", "nu", "sh", "bash", "dash", "zsh")
MARKER = "NEBULA_SCROLL_DONE"


def context_for(shell: str) -> ConformanceContext:
    context = object.__new__(ConformanceContext)
    context.shell = shell
    context.pane_id = 1
    return context


def available_shells() -> list[tuple[str, list[str]]]:
    commands = {
        "powershell": ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
        "pwsh": ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"],
        "cmd": ["cmd.exe", "/d", "/s", "/c"],
        "fish": ["fish", "--no-config", "-c"],
        "nu": ["nu", "--no-config-file", "-c"],
        "sh": ["sh", "-c"],
        "bash": ["bash", "--noprofile", "--norc", "-c"],
        "dash": ["dash", "-c"],
        "zsh": ["zsh", "-f", "-c"],
    }
    result = []
    for shell, command in commands.items():
        # Do not discover Windows interoperability launchers from a POSIX run.
        if os.name != "nt" and shell in {"powershell", "cmd"}:
            continue
        executable = shutil.which(command[0])
        if (
            executable and os.name == "nt" and shell == "bash"
            and Path(executable).parent.name.lower() in {"system32", "sysnative", "windowsapps"}
        ):
            # Windows' bash.exe is the WSL launcher, with different command
            # argument parsing. The POSIX run tests Bash directly.
            continue
        if executable:
            result.append((shell, [executable, *command[1:]]))
    return result


class ScrollbackCompletionTests(unittest.TestCase):
    def test_complete_marker_is_absent_from_every_shell_command(self) -> None:
        for shell in SHELLS:
            with self.subTest(shell=shell):
                command = context_for(shell).scrollback_command(600, MARKER)
                self.assertNotIn(MARKER, command)

    def test_command_echo_cannot_complete_the_output_wait(self) -> None:
        for shell in SHELLS:
            with self.subTest(shell=shell):
                context = context_for(shell)
                command = context.scrollback_command(600, MARKER)
                observations = [
                    {"text": f"prompt> {command}\nNEBULA_SCROLL_1"},
                    {"text": "NEBULA_SCROLL_310\nNEBULA_SCROLL_311"},
                    {"text": f"NEBULA_SCROLL_600\n{MARKER}"},
                ]
                with patch.object(context, "read", side_effect=observations), patch(
                    "conformance.harness.time.sleep"
                ):
                    _, completed = context.wait_for_line(re.compile(MARKER), timeout=1)
                self.assertEqual(completed, observations[-1])

    def test_real_shell_emits_completion_once_after_all_generated_rows(self) -> None:
        shells = available_shells()
        if not shells:
            self.skipTest("No supported shell is installed")
        for shell, invocation in shells:
            with self.subTest(shell=shell):
                command = context_for(shell).scrollback_command(6, MARKER)
                result = subprocess.run(
                    [*invocation, command],
                    stdin=subprocess.DEVNULL,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    timeout=10,
                    check=True,
                )
                self.assertEqual(
                    result.stdout.splitlines(),
                    [*(f"NEBULA_SCROLL_{index}" for index in range(1, 7)), MARKER],
                )


if __name__ == "__main__":
    unittest.main()
