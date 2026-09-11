"""Exercise metadata-check isolation; the Cargo recorder performs no compilation."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
MAC_TARGET = "aarch64-apple-darwin"


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires a POSIX Bash host")
class XplatIsolationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="nebula-xplat-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo with spaces"
        self.scripts = self.repo / "scripts/xplat"
        shutil.copytree(ROOT / "scripts/xplat", self.scripts)
        self.output = self.repo / ".target-xplat/metadata-only-v1"
        self.native = self.root / "native target"
        self.native.mkdir()
        self.native_sentinel = self.native / "keep.txt"
        self.native_sentinel.write_text("native cache", encoding="utf-8")
        self.calls = self.root / "cargo-calls.jsonl"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        rustc = self.bin / "rustc"
        rustc.write_text(
            '#!/usr/bin/env bash\nprintf "rustc test\\nhost: %s\\n" "$XPLAT_TEST_HOST"\n',
            encoding="utf-8",
        )
        cargo = self.bin / "cargo"
        cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "from pathlib import Path\n"
            "record = {'args': sys.argv[1:], 'target': os.environ['CARGO_TARGET_DIR'], "
            "'build_dir': os.environ['CARGO_BUILD_BUILD_DIR'], "
            "'cc': os.environ.get('CC_aarch64_apple_darwin')}\n"
            "with Path(os.environ['XPLAT_TEST_LOG']).open('a', encoding='utf-8') as log:\n"
            "    log.write(json.dumps(record) + '\\n')\n"
            "raise SystemExit(int(os.environ.get('XPLAT_TEST_EXIT', '0')))\n",
            encoding="utf-8",
        )
        rustc.chmod(0o755)
        cargo.chmod(0o755)

    def run_check(self, *args, host="x86_64-pc-windows-msvc", exit_code=0):
        env = {
            **os.environ,
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "RUSTC": str(self.bin / "rustc"),
            "CARGO_TARGET_DIR": str(self.native),
            "CARGO_BUILD_BUILD_DIR": str(self.native / "intermediate"),
            "XPLAT_TEST_HOST": host,
            "XPLAT_TEST_LOG": str(self.calls),
            "XPLAT_TEST_EXIT": str(exit_code),
        }
        return subprocess.run(
            [str(self.scripts / "check.sh"), *(args or ("macos",))],
            cwd=self.root, env=env, text=True, capture_output=True, timeout=10,
        )

    def recordings(self):
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text(encoding="utf-8").splitlines()]

    def build_out(self, base, package):
        result = base / MAC_TARGET / "debug/build" / package / "out"
        result.mkdir(parents=True)
        return result

    def test_inherited_native_output_is_preserved_and_cargo_is_locked(self):
        native_out = self.build_out(self.native, "media-native")
        result = self.run_check("macos", "--features", "gpui-shell", "-p", "nebula")
        self.assertEqual(result.returncode, 0, result.stderr)
        call, = self.recordings()
        self.assertEqual(call["target"], str(self.output))
        self.assertEqual(call["build_dir"], str(self.output))
        args = call["args"]
        self.assertIn("--locked", args)
        self.assertEqual(args[args.index("--target-dir") + 1], str(self.output))
        self.assertEqual(args[-4:], ["--features", "gpui-shell", "-p", "nebula"])
        self.assertEqual(list(native_out.iterdir()), [])
        self.assertEqual(self.native_sentinel.read_text(encoding="utf-8"), "native cache")
        self.assertIn("not a native build/test", result.stdout)

    def test_native_target_is_rejected_before_any_output_or_cargo_call(self):
        for selector, host in [("macos", MAC_TARGET), ("linux", "x86_64-unknown-linux-gnu"), ("all", MAC_TARGET)]:
            with self.subTest(selector=selector):
                result = self.run_check(selector, host=host)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("refusing native target", result.stderr)
                self.assertEqual(self.recordings(), [])
                self.assertFalse(self.output.exists())

    def test_cargo_target_and_configuration_overrides_are_rejected(self):
        for options in [
            ["--target-dir", str(self.native)], [f"--target-dir={self.native}"],
            ["--target-d", str(self.native)], ["--config", "build.target-dir='target'"],
            ["--config=build.target-dir='target'"], ["--target", "x86_64-unknown-linux-gnu"],
            ["--manifest-path", "elsewhere/Cargo.toml"], ["--release"],
        ]:
            with self.subTest(options=options):
                result = self.run_check("macos", *options)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(self.recordings(), [])
                self.assertFalse(self.output.exists())

    def test_unmarked_existing_metadata_directory_is_not_repurposed(self):
        self.output.mkdir(parents=True)
        sentinel = self.output / "user-data"
        sentinel.write_text("keep", encoding="utf-8")
        result = self.run_check()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")
        self.assertEqual(self.recordings(), [])

    def test_stubs_are_seeded_only_in_owned_metadata_output(self):
        self.assertEqual(self.run_check().returncode, 0)
        media = self.build_out(self.output, "media-check")
        shaders = self.build_out(self.output, "gpui_macos-check")
        native_media = self.build_out(self.native, "media-native")
        native_shaders = self.build_out(self.native, "gpui_macos-native")
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((media / "bindings.rs").read_bytes(), (self.scripts / "media-bindings-stub.rs").read_bytes())
        self.assertEqual((shaders / "shaders.metallib").read_bytes(), b"")
        self.assertEqual(list(native_media.iterdir()), [])
        self.assertEqual(list(native_shaders.iterdir()), [])

    def test_symlinked_output_root_is_rejected(self):
        (self.repo / ".target-xplat").symlink_to(self.native, target_is_directory=True)
        result = self.run_check()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.recordings(), [])
        self.assertEqual([path.name for path in self.native.iterdir()], ["keep.txt"])

    def test_symlinked_stub_directory_cannot_escape_owned_output(self):
        self.assertEqual(self.run_check().returncode, 0)
        package = self.output / MAC_TARGET / "debug/build/media-escape"
        package.mkdir(parents=True)
        (package / "out").symlink_to(self.native, target_is_directory=True)
        result = self.run_check()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("escapes metadata directory", result.stderr)
        self.assertEqual(len(self.recordings()), 1)
        self.assertFalse((self.native / "bindings.rs").exists())

    def test_symlinked_stub_file_cannot_overwrite_native_file(self):
        self.assertEqual(self.run_check().returncode, 0)
        media = self.build_out(self.output, "media-escape")
        (media / "bindings.rs").symlink_to(self.native_sentinel)
        result = self.run_check()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(self.native_sentinel.read_text(encoding="utf-8"), "native cache")

    def test_cargo_failure_is_reported_unchanged(self):
        result = self.run_check(exit_code=17)
        self.assertEqual(result.returncode, 17)
        self.assertEqual(len(self.recordings()), 1)

    def test_posix_compiler_can_be_invoked_directly(self):
        result = subprocess.run(
            [str(self.scripts / "fake-cc.sh"), "--version"],
            text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cross-check stub", result.stdout)
