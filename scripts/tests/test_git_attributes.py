import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "nebula_terminal/tests/ref"


class GitAttributeTests(unittest.TestCase):
    def test_git_preserves_raw_terminal_streams(self):
        paths = sorted(str(path.relative_to(ROOT)).replace("\\", "/") for path in FIXTURES.rglob("*.recording"))
        self.assertTrue(paths)
        result = subprocess.check_output(
            ["git", "-C", str(ROOT), "check-attr", "-z", "text", "--", *paths]
        ).decode("utf-8").split("\0")[:-1]
        for path, attribute, value in zip(result[0::3], result[1::3], result[2::3]):
            with self.subTest(path=path):
                self.assertEqual((attribute, value), ("text", "unset"))

    def test_known_crlf_streams_keep_carriage_returns(self):
        for name in (
            "clear_underline", "colored_reset", "delete_lines", "row_reset",
            "saved_cursor", "saved_cursor_alt", "selective_erasure", "sgr",
        ):
            with self.subTest(name=name):
                self.assertIn(b"\r\n", (FIXTURES / name / "nebula.recording").read_bytes())

    def test_completion_snapshots_use_lf_on_every_checkout(self):
        paths = sorted(
            str(path.relative_to(ROOT)).replace("\\", "/")
            for path in (ROOT / "extra/completions").rglob("*")
            if path.is_file() and path.name != "README.md"
        )
        self.assertTrue(paths)
        result = subprocess.check_output(
            ["git", "-C", str(ROOT), "check-attr", "-z", "eol", "--", *paths]
        ).decode("utf-8").split("\0")[:-1]
        for path, attribute, value in zip(result[0::3], result[1::3], result[2::3]):
            with self.subTest(path=path):
                self.assertEqual((attribute, value), ("eol", "lf"))
