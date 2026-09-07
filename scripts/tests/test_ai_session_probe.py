import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHILD = r'''
import ctypes, json, sys
ctypes.CDLL(None).prctl(15, b"codex", 0, 0, 0)
handles = [open(path, "rb") for path in json.loads(sys.argv[1])]
print("ready", flush=True)
sys.stdin.read()
'''


@unittest.skipUnless(sys.platform == "linux", "requires Linux procfs (also runs inside WSL)")
class ActiveRolloutProbeTests(unittest.TestCase):
    def test_probe_binds_custom_home_to_exact_instance_and_pane(self):
        source = (ROOT / "nebula_app/src/platform/ai_session_identity.rs").read_text()
        script = source.split('const PROBE_SCRIPT: &str = r###"', 1)[1].split('"###;', 1)[0]
        with tempfile.TemporaryDirectory(prefix="pebrel-identity-") as temporary:
            codex_directory = Path(temporary) / "custom codex"
            sessions = codex_directory / "sessions/2026/09/07"
            sessions.mkdir(parents=True)
            main_id = "01a079fa-4a9b-7d93-8a4a-4a7a9edaf247"
            guardian_id = "01a079f7-7e36-7232-882b-f06d3bde9df8"
            paths = []
            for identity, origin in ((main_id, "cli"), (guardian_id, {"subagent": "guardian"})):
                path = sessions / f"rollout-x-{identity}.jsonl"
                path.write_text(json.dumps({"type": "session_meta", "payload": {
                    "id": identity, "source": origin,
                }}) + "\n", encoding="utf-8")
                paths.append(str(path))
            instance = f"probe-test-{os.getpid()}"
            environment = dict(os.environ, PEBREL_PROCESS_ID=instance,
                               PEBREL_PANE_ID="713", CODEX_HOME=str(codex_directory))
            child = subprocess.Popen([sys.executable, "-c", CHILD, json.dumps(paths)],
                                     env=environment, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.assertEqual(child.stdout.readline().strip(), "ready")
                result = subprocess.run(["sh", "-c", script, "probe", "713", instance],
                                        capture_output=True, text=True, check=True, timeout=5)
                records = [json.loads(line.split("\t", 3)[3])
                           for line in result.stdout.splitlines()]
                self.assertEqual({item["payload"]["id"] for item in records},
                                 {main_id, guardian_id})
                for pane, owner in (("714", instance), ("713", instance + "-other")):
                    mismatch = subprocess.run(["sh", "-c", script, "probe", pane, owner],
                                              capture_output=True, text=True, check=True, timeout=5)
                    self.assertEqual(mismatch.stdout, "")
            finally:
                child.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
