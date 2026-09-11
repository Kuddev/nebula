from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS_DIR = Path(__file__).resolve().parents[2]
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from conformance.cases import resize
from conformance.harness import ConformanceError


class ResizeContext:
    window_id = 1
    pane_id = 10
    shell = "bash"
    platform_family = "macos"

    def __init__(self, artifact_dir: Path, columns: list[int]) -> None:
        self.artifact_dir = artifact_dir
        self.columns = iter(columns)
        self.cleanup: list[str] = []

    def api(self, method: str, params=None, **kwargs):
        if method == "pane.split":
            return {"action": {"pane_id": 11}}
        if method == "pane.resize":
            return {"action": {"ratio": params["ratio"]}}
        if method == "runtime.snapshot":
            return {"windows": [{"id": self.window_id}]}
        if method == "pane.read":
            return {"text": "NEBULA_COLS_3_116"}
        raise AssertionError(method)

    def measure_columns(self, pane_id: int) -> int:
        return next(self.columns)

    def best_effort_api(self, method: str, params) -> None:
        self.cleanup.append(method)


class ResizeDiagnosticsTests(unittest.TestCase):
    def test_growing_and_shrinking_panes_pass_without_diagnostic_sampling(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            ctx = ResizeContext(Path(directory), [57, 56, 80, 33])
            with patch.object(resize, "_write_failure_diagnostics") as diagnose:
                result = resize.run(ctx)
            self.assertTrue(result["pty_columns_changed"])
            diagnose.assert_not_called()
            self.assertEqual(ctx.cleanup, ["pane.close", "window.focus"])

    def test_unchanged_spawn_width_still_fails_and_preserves_all_measurements(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            ctx = ResizeContext(Path(directory), [116, 116, 116, 116])
            with self.assertRaisesRegex(ConformanceError, "70% pane did not grow: 116 -> 116"):
                resize.run(ctx)
            details = json.loads((Path(directory) / "resize-failure.json").read_text())
            self.assertEqual(details["measurements"], {
                "source_columns_before": 116,
                "sibling_columns_before": 116,
                "source_columns_after": 116,
                "sibling_columns_after": 116,
            })
            self.assertEqual(set(details["panes"]), {"10", "11"})
            self.assertEqual(ctx.cleanup, ["pane.close", "window.focus"])

    def test_sibling_and_total_assertions_remain_strict(self) -> None:
        for columns, message in [
            ([57, 56, 80, 56], "30% pane did not shrink"),
            ([57, 56, 85, 15], "PTY column total changed"),
        ]:
            with self.subTest(columns=columns), tempfile.TemporaryDirectory() as directory:
                ctx = ResizeContext(Path(directory), columns)
                with self.assertRaisesRegex(ConformanceError, message):
                    resize.run(ctx)
                self.assertEqual(ctx.cleanup, ["pane.close", "window.focus"])

    def test_diagnostic_failure_cannot_replace_the_resize_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            ctx = ResizeContext(Path(directory), [116] * 4)
            with patch.object(resize, "_write_failure_diagnostics", side_effect=OSError("read-only")), \
                    patch("builtins.print"):
                with self.assertRaisesRegex(ConformanceError, "70% pane did not grow"):
                    resize.run(ctx)
            self.assertEqual(ctx.cleanup, ["pane.close", "window.focus"])

    @unittest.skipUnless(sys.platform == "linux", "reads the Linux process PTY")
    def test_kernel_probe_follows_pty_geometry_even_with_stale_columns_environment(self) -> None:
        import fcntl
        import struct
        import termios

        master, slave = os.openpty()
        child = None
        try:
            child = subprocess.Popen(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                stdin=slave, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                env={**os.environ, "COLUMNS": "116"},
            )
            for columns in (57, 82):
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, columns, 0, 0))
                self.assertEqual(
                    resize._linux_terminal_size(child.pid),
                    {"rows": 30, "columns": columns},
                )
        finally:
            if child is not None:
                child.terminate()
                child.wait(timeout=5)
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    unittest.main()
