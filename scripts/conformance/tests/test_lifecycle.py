from __future__ import annotations

import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPTS_DIR = Path(__file__).resolve().parents[2]
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from conformance.harness import ConformanceContext, ConformanceError


REAL_POPEN = subprocess.Popen
FIXTURE = """
from pathlib import Path
import os, subprocess, sys, time

root = Path(sys.argv[1])
role = sys.argv[2]
if role == 'grandchild':
    time.sleep(60)
else:
    next_role = 'child' if role == 'parent' else 'grandchild'
    child = subprocess.Popen(
        [sys.executable, '-I', '-S', __file__, str(root), next_role],
        stdin=subprocess.DEVNULL,
    )
    (root / (next_role + '.pid')).write_text(str(child.pid))
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if role == 'parent' and (root / 'exit').exists():
            code = root / 'exit-code'
            raise SystemExit(int(code.read_text()) if code.exists() else 0)
        time.sleep(0.02)
"""


class OwnedProcessHandle:
    """Retain fixture process identity; never terminate by an unowned PID lookup."""

    def __init__(self, pid: int) -> None:
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
        self.api.OpenProcess.restype = wintypes.HANDLE
        self.api.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
        self.api.WaitForSingleObject.restype = wintypes.DWORD
        self.api.TerminateProcess.argtypes = (wintypes.HANDLE, wintypes.UINT)
        self.api.TerminateProcess.restype = wintypes.BOOL
        self.api.CloseHandle.argtypes = (wintypes.HANDLE,)
        self.api.CloseHandle.restype = wintypes.BOOL
        self.handle = self.api.OpenProcess(0x00100001, False, pid)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())

    def exited(self) -> bool:
        return self.api.WaitForSingleObject(self.handle, 0) == 0

    def close(self) -> None:
        if self.handle is None:
            return
        try:
            if not self.exited():
                terminated = self.api.TerminateProcess(self.handle, 1)
                error = ctypes.WinError(ctypes.get_last_error()) if not terminated else None
                # A job may already be terminating this process. ERROR_ACCESS_DENIED
                # is benign only when the retained handle then proves it exited.
                if self.api.WaitForSingleObject(self.handle, 5000) != 0:
                    if error is not None:
                        raise error
                    raise AssertionError("owned fixture did not terminate")
        finally:
            self.api.CloseHandle(self.handle)
            self.handle = None


@unittest.skipUnless(os.name == "nt", "Windows Job Object process containment")
class WindowsLifecycleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="nebula-lifecycle-test-")
        self.root = Path(self.temporary.name)
        self.fixture = self.root / "fixture.py"
        self.fixture.write_text(FIXTURE, encoding="utf-8")
        self.app = SimpleNamespace(executable=self.root / "fixture-app.exe")
        self.context = ConformanceContext(
            self.app, "windows-test", self.root / "config", self.root / "work",
            self.root / "artifacts", startup_timeout=5,
        )
        self.context.prepare()
        self.context.port_file.write_text("1 fixture-token", encoding="utf-8")
        self.parents: list[subprocess.Popen] = []
        self.handles: list[OwnedProcessHandle] = []
        self.launches: list[Path] = []
        self.unrelated: subprocess.Popen | None = None
        self.addCleanup(self.cleanup_owned_processes)

        def launch_fixture(command, **kwargs):
            # Keep any platform launch gate intact; replace only the app under
            # test with an inert private Python tree, never a GUI executable.
            command = list(command)
            index = command.index(os.fspath(self.app.executable))
            launch = self.root / f"launch-{len(self.launches) + 1}"
            launch.mkdir()
            self.launches.append(launch)
            command[index:] = [
                sys.executable, "-I", "-S", os.fspath(self.fixture),
                os.fspath(launch), "parent",
            ]
            process = REAL_POPEN(command, **kwargs)
            self.parents.append(process)
            return process

        client = SimpleNamespace(request=lambda *args, **kwargs: {
            "result": {"windows": [{"tabs": [{"kind": "shell", "panes": [{}]}]}]},
        })
        self.launch_patch = patch("conformance.harness.subprocess.Popen", side_effect=launch_fixture)
        self.client_patch = patch("conformance.harness.RuntimeClient.from_port_file", return_value=client)
        self.launch_patch.start()
        self.client_patch.start()
        self.addCleanup(self.launch_patch.stop)
        self.addCleanup(self.client_patch.stop)

    def cleanup_owned_processes(self) -> None:
        failures = []
        try:
            self.context.stop(force=True)
        except Exception as error:
            failures.append(error)
        # Regressions must not leave even their failing fixtures running, and
        # cleanup errors must remain visible without skipping later fixtures.
        for handle in self.handles:
            try:
                handle.close()
            except Exception as error:
                failures.append(error)
        for process in [*self.parents, self.unrelated]:
            try:
                if process is not None:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)
            except Exception as error:
                failures.append(error)
        try:
            self.temporary.cleanup()
        except Exception as error:
            failures.append(error)
        if failures:
            raise ExceptionGroup("owned fixture cleanup failed", failures)

    def capture_descendants(self) -> list[OwnedProcessHandle]:
        captured = []
        for name in ("child", "grandchild"):
            path = self.launches[-1] / f"{name}.pid"
            deadline = time.monotonic() + 5
            while not path.is_file() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(path.is_file(), f"fixture never created {name}")
            handle = OwnedProcessHandle(int(path.read_text()))
            self.handles.append(handle)
            captured.append(handle)
        return captured

    def test_force_stop_reaps_descendants_but_not_an_unrelated_process(self) -> None:
        self.unrelated = REAL_POPEN(
            [sys.executable, "-I", "-S", "-c", "import time; time.sleep(60)"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.context.start()
        descendants = self.capture_descendants()
        self.context.stop(force=True)
        self.assertTrue(all(process.exited() for process in descendants))
        self.assertIsNone(self.unrelated.poll())
        self.context.work_dir.rmdir()

    def test_stop_still_reaps_children_after_the_parent_has_exited(self) -> None:
        self.context.start()
        descendants = self.capture_descendants()
        (self.launches[-1] / "exit").touch()
        self.context.process.wait(timeout=5)
        self.context.stop(force=True)
        self.assertTrue(all(process.exited() for process in descendants))

    def test_restart_reaps_the_previous_launch_before_starting_another(self) -> None:
        self.context.start()
        previous = self.capture_descendants()
        self.context.restart()
        current = self.capture_descendants()
        self.assertTrue(all(process.exited() for process in previous))
        self.assertTrue(all(not process.exited() for process in current))

    def test_normal_window_close_also_reaps_leftover_descendants(self) -> None:
        self.context.start()
        descendants = self.capture_descendants()
        with patch.object(
            self.context, "api", side_effect=lambda *args, **kwargs: (self.launches[-1] / "exit").touch()
        ):
            self.context.close_and_wait()
        self.assertTrue(all(process.exited() for process in descendants))

    def test_window_close_preserves_the_application_exit_code(self) -> None:
        self.context.start()
        descendants = self.capture_descendants()
        (self.launches[-1] / "exit-code").write_text("7")
        with patch.object(
            self.context, "api",
            side_effect=lambda *args, **kwargs: (self.launches[-1] / "exit").touch(),
        ):
            with self.assertRaisesRegex(ConformanceError, "Nebula exited with code 7"):
                self.context.close_and_wait()
        self.assertTrue(all(process.exited() for process in descendants))

    def test_assignment_failure_does_not_start_an_uncontained_application(self) -> None:
        with patch("conformance.harness._WindowsJob.assign", side_effect=OSError("cannot assign")):
            with self.assertRaisesRegex(ConformanceError, "cannot assign"):
                self.context.start()
        self.assertIsNone(self.context.process)
        self.assertTrue(all(process.poll() is not None for process in self.parents))
        self.assertFalse((self.launches[-1] / "child.pid").exists())

    def test_cleanup_errors_propagate_and_retain_ownership_for_retry(self) -> None:
        self.context.start()
        descendants = self.capture_descendants()
        process, job = self.context.process, self.context._windows_job
        with patch.object(job, "terminate", side_effect=OSError("cleanup unavailable")):
            with self.assertRaisesRegex(OSError, "cleanup unavailable"):
                self.context.stop(force=True)
        self.assertIs(self.context.process, process)
        self.assertIs(self.context._windows_job, job)
        self.context.stop(force=True)
        self.assertTrue(all(process.exited() for process in descendants))


if __name__ == "__main__":
    unittest.main()
