from __future__ import annotations

import os
from pathlib import Path
import select
import signal
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
if role != 'grandchild':
    next_role = 'child' if role == 'parent' else 'grandchild'
    subprocess.Popen([sys.executable, '-I', '-S', __file__, str(root), next_role])
pending_pid = root / (role + '.pid.pending')
pending_pid.write_text(str(os.getpid()))
pending_pid.replace(root / (role + '.pid'))
while True:
    if role == 'parent' and (root / 'exit').exists():
        code = root / 'exit-code'
        raise SystemExit(int(code.read_text()) if code.exists() else 0)
    time.sleep(0.01)
"""


@unittest.skipUnless(os.name == "posix", "POSIX launch process groups")
class PosixLifecycleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="nebula-posix-lifecycle-")
        self.root = Path(self.temporary.name)
        self.fixture = self.root / "fixture.py"
        self.fixture.write_text(FIXTURE, encoding="utf-8")
        self.app = SimpleNamespace(executable=self.root / "fixture-app")
        self.context = ConformanceContext(
            self.app, "linux-test", self.root / "config", self.root / "work",
            self.root / "artifacts", startup_timeout=2,
        )
        self.context.prepare()
        self.context.port_file.write_text("1 fixture-token", encoding="utf-8")
        self.parents: list[subprocess.Popen] = []
        self.launches: list[Path] = []
        self.descendants: list[int] = []
        self.unrelated: subprocess.Popen | None = None
        self.addCleanup(self.cleanup_owned_processes)

        def launch_fixture(command, **kwargs):
            launch = self.root / f"launch-{len(self.launches) + 1}"
            launch.mkdir()
            self.launches.append(launch)
            # All fixture descendants inherit this pipe. EOF proves they have
            # actually exited even when their former parent is already reaped.
            kwargs["stdout"] = subprocess.PIPE
            process = REAL_POPEN([
                sys.executable, "-I", "-S", os.fspath(self.fixture),
                os.fspath(launch), "parent",
            ], **kwargs)
            self.parents.append(process)
            return process

        self.client = SimpleNamespace(request=lambda *args, **kwargs: {
            "result": {
                "process_id": self.parents[-1].pid,
                "text": "fixture> ",
                "windows": [{"id": 1, "tabs": [{"kind": "shell", "panes": [{"id": 1}]}]}],
            },
        })
        self.launch_patch = patch("conformance.harness.subprocess.Popen", side_effect=launch_fixture)
        self.client_patch = patch(
            "conformance.harness.RuntimeClient.from_port_file", return_value=self.client,
        )
        self.launch_patch.start()
        self.client_patch.start()
        self.addCleanup(self.launch_patch.stop)
        self.addCleanup(self.client_patch.stop)

    def capture_descendants(self) -> None:
        for role in ("child", "grandchild"):
            path = self.launches[-1] / f"{role}.pid"
            deadline = time.monotonic() + 5
            while not path.is_file() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(path.is_file(), f"fixture never created {role}")
            self.descendants.append(int(path.read_text()))

    def assert_tree_exited(self, process: subprocess.Popen) -> None:
        self.assertTrue(select.select([process.stdout], [], [], 2)[0], "launch descendants survived")
        self.assertEqual(os.read(process.stdout.fileno(), 65536), b"")

    def cleanup_owned_processes(self) -> None:
        self.context.stop(force=True)
        # Keep the negative fixture bounded when running against the old harness.
        for launch in self.launches:
            for role in ("child", "grandchild"):
                path = launch / f"{role}.pid"
                if path.is_file() and path.read_text():
                    self.descendants.append(int(path.read_text()))
        for pid in set(self.descendants):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        for process in [*self.parents, self.unrelated]:
            if process is not None:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=5)
                if process.stdout is not None:
                    process.stdout.close()
        self.temporary.cleanup()

    def test_force_stop_terminates_the_launch_but_not_an_unrelated_process(self) -> None:
        self.unrelated = REAL_POPEN(
            [sys.executable, "-I", "-S", "-c", "import time; time.sleep(60)"],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        self.context.start()
        self.capture_descendants()
        self.context.stop(force=True)
        self.assert_tree_exited(self.parents[-1])
        self.assertIsNone(self.unrelated.poll())

    def test_stop_terminates_children_after_the_wrapper_has_exited(self) -> None:
        self.context.start()
        self.capture_descendants()
        (self.launches[-1] / "exit").touch()
        self.context.process.wait(timeout=5)
        self.context.stop(force=True)
        self.assert_tree_exited(self.parents[-1])

    def test_restart_terminates_the_previous_launch(self) -> None:
        self.context.start()
        self.capture_descendants()
        self.context.restart()
        self.capture_descendants()
        self.assert_tree_exited(self.parents[0])
        self.assertFalse(select.select([self.parents[1].stdout], [], [], 0)[0])

    def test_normal_close_terminates_leftover_children_and_preserves_exit_code(self) -> None:
        for code in (0, 7):
            with self.subTest(code=code):
                self.context.start()
                self.capture_descendants()
                (self.launches[-1] / "exit-code").write_text(str(code))
                with patch.object(
                    self.context, "api",
                    side_effect=lambda *args, **kwargs: (self.launches[-1] / "exit").touch(),
                ):
                    if code:
                        with self.assertRaisesRegex(ConformanceError, "Nebula exited with code 7"):
                            self.context.close_and_wait()
                    else:
                        self.context.close_and_wait()
                self.assert_tree_exited(self.parents[-1])

    def test_start_rejects_an_endpoint_from_another_launch(self) -> None:
        self.context.startup_timeout = 0.15
        self.client.request = lambda *args, **kwargs: {
            "result": {
                "process_id": os.getpid(),
                "windows": [{"tabs": [{"kind": "shell", "panes": [{}]}]}],
            },
        }
        with self.assertRaisesRegex(ConformanceError, "runtime process does not belong to this launch"):
            self.context.start()
        self.assert_tree_exited(self.parents[-1])

    def test_start_accepts_the_application_beneath_a_wrapper(self) -> None:
        self.client.request = lambda *args, **kwargs: {
            "result": {
                "process_id": int((self.launches[-1] / "child.pid").read_text()),
                "text": "fixture> ",
                "windows": [{"id": 1, "tabs": [{"kind": "shell", "panes": [{"id": 1}]}]}],
            },
        }
        self.context.start()
        self.capture_descendants()
        self.context.stop(force=True)
        self.assert_tree_exited(self.parents[-1])

    def test_cleanup_failure_retains_the_owned_group_for_retry(self) -> None:
        self.context.start()
        self.capture_descendants()
        process = self.context.process
        with patch("conformance.harness.os.killpg", side_effect=OSError("cleanup unavailable")):
            with self.assertRaisesRegex(OSError, "cleanup unavailable"):
                self.context.stop(force=True)
        self.assertIs(self.context.process, process)
        self.context.stop(force=True)
        self.assert_tree_exited(process)


if __name__ == "__main__":
    unittest.main()
