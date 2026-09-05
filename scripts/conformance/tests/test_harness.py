from __future__ import annotations

import json
import re
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
import zipfile
from pathlib import Path

SCRIPTS_DIR = Path(__file__).resolve().parents[2]
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from conformance.harness import (  # noqa: E402
    ConformanceError,
    ResolvedApp,
    compare_reports,
    find_last_line_match,
    filter_flat,
    flatten,
    normalize_path,
    normalize_text,
    shell_category,
    SkipCase,
)
from conformance.macos_launch import capture_screenshot


class NormalizationTests(unittest.TestCase):
    def test_normalize_path_handles_drive_and_posix_paths(self) -> None:
        self.assertEqual(normalize_path(r"C:\Users\Test\nebula"), "/c/Users/Test/nebula")
        self.assertEqual(normalize_path("/tmp//nebula/"), "/tmp/nebula")
        self.assertEqual(normalize_path("/"), "/")

    def test_normalize_text_unifies_newlines_and_trailing_space(self) -> None:
        self.assertEqual(normalize_text("alpha  \r\nbeta\r\n\r\n"), "alpha\nbeta")

    def test_shell_category_accepts_paths_and_labels(self) -> None:
        self.assertEqual(shell_category(r"C:\Windows\System32\WindowsPowerShell.exe"), "powershell")
        self.assertEqual(shell_category("/bin/zsh"), "zsh")
        self.assertEqual(shell_category("Nushell"), "nu")
        self.assertIsNone(shell_category("unknown-shell"))

    def test_line_match_accepts_output_attached_to_a_wrapped_command(self) -> None:
        text = (
            "❯ Write-Output ('NEBULA_COLS_2_' + "
            "$Host.UI.RawUI.WindowSNEBULA_COLS_2_57\n❯"
        )
        match = find_last_line_match(re.compile(r"NEBULA_COLS_2_(\d+)"), text)
        self.assertIsNotNone(match)
        assert match is not None
        self.assertEqual(match.group(1), "57")


class ComparisonTests(unittest.TestCase):
    def test_new_ssh_capability_cannot_remain_silently_skipped(self) -> None:
        from conformance.cases.ssh_loop import run

        with self.assertRaises(SkipCase):
            run(SimpleNamespace(description={"capabilities": []}))
        with self.assertRaises(ConformanceError):
            run(SimpleNamespace(description={"capabilities": ["ssh.open"]}))

    def test_flatten_and_filter_apply_glob_rules(self) -> None:
        flat = flatten({"cases": {"boot": {"status": "passed", "duration_ms": 4}}})
        self.assertEqual(
            filter_flat(flat, ["cases.*.duration_ms"]),
            {"cases.boot.status": "passed"},
        )

    def test_cross_platform_whitelist_ignores_only_documented_fields(self) -> None:
        first = {
            "schema_version": 1,
            "platform": "windows-x86_64",
            "cases": {
                "boot": {"status": "passed", "shell_category": "powershell"},
                "echo": {"status": "passed", "tail_contains_marker": True},
                "ssh_loop": {"status": "skipped", "reason": "missing"},
            },
            "summary": {"total": 3, "passed": 2, "failed": 0, "skipped": 1},
        }
        second = json.loads(json.dumps(first))
        second["platform"] = "linux-x86_64"
        second["cases"]["boot"]["shell_category"] = "bash"
        second["cases"]["ssh_loop"] = {"status": "passed"}
        second["summary"]["passed"] = 3
        second["summary"]["skipped"] = 0

        with tempfile.TemporaryDirectory() as directory:
            golden = Path(directory)
            (golden / "whitelist.json").write_text(
                json.dumps(
                    {
                        "volatile": {},
                        "cross_platform": {
                            "platform": "identity",
                            "cases.boot.shell_category": "default shell",
                            "cases.ssh_loop.*": "optional",
                            "summary.passed": "optional",
                            "summary.skipped": "optional",
                        },
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(compare_reports([first, second], golden), [])
            second["cases"]["echo"]["tail_contains_marker"] = False
            errors = compare_reports([first, second], golden)
            self.assertTrue(any("tail_contains_marker" in error for error in errors))


class ArchiveSafetyTests(unittest.TestCase):
    def test_zip_member_cannot_escape_extraction_root(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "bad.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("../outside/nebula", "not an executable")
            with self.assertRaisesRegex(ConformanceError, "escapes extraction root"):
                ResolvedApp(archive)
            self.assertFalse((root.parent / "outside" / "nebula").exists())


class LaunchCaptureTests(unittest.TestCase):
    def test_screenshot_requires_success_and_a_nonempty_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / "capture.png"
            with (root / "capture.log").open("wb") as log, patch(
                "conformance.macos_launch.subprocess.run", return_value=SimpleNamespace(returncode=0),
            ):
                self.assertEqual(capture_screenshot(destination, log), "unavailable")
                destination.write_bytes(b"")
                self.assertEqual(capture_screenshot(destination, log), "unavailable")
                destination.write_bytes(b"screenshot")
                self.assertEqual(capture_screenshot(destination, log), "captured")

    def test_screenshot_failure_does_not_fail_the_launch_test(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            destination = root / "capture.png"
            destination.write_bytes(b"partial")
            with (root / "capture.log").open("wb") as log, patch(
                "conformance.macos_launch.subprocess.run", return_value=SimpleNamespace(returncode=1),
            ):
                self.assertEqual(capture_screenshot(destination, log), "unavailable")

    def test_screenshot_errors_do_not_fail_the_launch_test(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (root / "capture.log").open("wb") as log:
                for error in (OSError("capture unavailable"), subprocess.TimeoutExpired("screencapture", 15)):
                    with self.subTest(error=error), patch(
                        "conformance.macos_launch.subprocess.run", side_effect=error,
                    ):
                        self.assertEqual(capture_screenshot(root / "capture.png", log), "unavailable")


if __name__ == "__main__":
    unittest.main()
