"""Exercise the cache identity code used by the composite action itself."""

import os
from pathlib import Path
import re
import tempfile
import textwrap
import unittest
from unittest.mock import patch


ACTION = Path(__file__).resolve().parents[2] / ".github/actions/rust-cache/action.yml"


class CacheIdentityTests(unittest.TestCase):
    def identity(self, *, compiler="rustc 1.97.1", sdk="15.0", **changes):
        source = ACTION.read_text(encoding="utf-8")
        body = re.search(r"python - <<'PY'\n(.*?)\n        PY", source, re.S)
        self.assertIsNotNone(body, "cache identity must remain executable from this action")
        code = textwrap.dedent(body.group(1))
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "outputs"
            environment = {
                "CACHE_OS": "Linux", "CACHE_ARCH": "X64", "CACHE_WORKLOAD": "ci-product",
                "CACHE_MANIFESTS": "pinned-manifests", "GITHUB_OUTPUT": str(output),
                "CARGO_HOME": str(Path(temporary) / "cargo"), **changes,
            }

            def command(arguments, **kwargs):
                if arguments == ["rustc", "-vV"]:
                    return compiler
                self.assertEqual(arguments, ["xcrun", "--sdk", "macosx", "--show-sdk-version"])
                return sdk

            with patch.dict(os.environ, environment), patch("subprocess.check_output", side_effect=command):
                exec(compile(code, str(ACTION), "exec"), {})
            return dict(line.split("=", 1) for line in output.read_text(encoding="utf-8").splitlines())

    def test_compiler_flags_and_workload_isolate_compiled_targets(self):
        baseline = self.identity()["key"]
        for change in (
            {"compiler": "rustc 1.98.0"}, {"RUSTFLAGS": "-C opt-level=1"},
            {"CARGO_PROFILE_RELEASE_LTO": "false"}, {"CACHE_WORKLOAD": "release"},
            {"CACHE_ARCH": "ARM64"}, {"ImageOS": "a-different-runner-image"},
        ):
            with self.subTest(change=change):
                self.assertNotEqual(baseline, self.identity(**change)["key"])

    def test_macos_sdk_and_deployment_floor_are_part_of_identity(self):
        baseline = self.identity(CACHE_OS="macOS", sdk="15.0")["key"]
        self.assertNotEqual(baseline, self.identity(CACHE_OS="macOS", sdk="16.0")["key"])
        self.assertNotEqual(baseline, self.identity(CACHE_OS="macOS", MACOSX_DEPLOYMENT_TARGET="13.0")["key"])

    def test_dependency_change_can_restore_compatible_previous_targets(self):
        before = self.identity(CACHE_MANIFESTS="before")
        after = self.identity(CACHE_MANIFESTS="after")
        self.assertNotEqual(before["key"], after["key"])
        self.assertEqual(before["restore-key"], after["restore-key"])
        self.assertTrue(after["key"].startswith(after["restore-key"]))

    def test_source_only_revision_and_package_labels_do_not_bust_dependencies(self):
        before = self.identity(GITHUB_SHA="one", PREVIEW_ID="first")
        after = self.identity(GITHUB_SHA="two", PREVIEW_ID="second")
        self.assertEqual(before["key"], after["key"])


if __name__ == "__main__":
    unittest.main()
