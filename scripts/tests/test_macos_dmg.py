import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts.macos_dmg import MIB, image_size_mib


class DmgSizingTests(unittest.TestCase):
    def test_sparse_binary_uses_logical_size(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with (root / "pebrel").open("wb") as stream:
                stream.truncate(512 * MIB)
            self.assertGreater(image_size_mib(root), 640)

    @unittest.skipIf(os.name == "nt", "requires unprivileged symlinks")
    def test_application_link_does_not_follow_external_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            stage.mkdir()
            external = root / "Applications"
            external.mkdir()
            with (external / "large").open("wb") as stream:
                stream.truncate(1024 * MIB)
            (stage / "Applications").symlink_to(external, target_is_directory=True)
            self.assertEqual(image_size_mib(stage), 64)

    def test_small_files_include_allocation_overhead(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index in range(100):
                (root / str(index)).write_bytes(b"x")
            self.assertGreaterEqual(image_size_mib(root), 64)

    def test_missing_source_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(ValueError):
                image_size_mib(Path(temporary) / "missing")

    @unittest.skipUnless(sys.platform == "darwin", "requires native hdiutil")
    def test_native_image_contains_complete_sparse_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            stage = root / "stage"
            executable = stage / "Pebrel Preview.app/Contents/MacOS/pebrel"
            executable.parent.mkdir(parents=True)
            with executable.open("wb") as stream:
                stream.write(b"pebrel-start")
                stream.seek(64 * MIB)
                stream.write(b"pebrel-end")
            (stage / "Applications").symlink_to("/Applications")
            image = root / "test.dmg"
            subprocess.run([
                "hdiutil", "create", "-volname", "Pebrel Preview", "-fs", "HFS+",
                "-size", f"{image_size_mib(stage)}m", "-srcfolder", str(stage),
                "-format", "UDZO", str(image),
            ], check=True, capture_output=True)
            mount = root / "mount"
            mount.mkdir()
            subprocess.run([
                "hdiutil", "attach", str(image), "-nobrowse", "-readonly",
                "-mountpoint", str(mount),
            ], check=True, capture_output=True)
            try:
                copied = mount / executable.relative_to(stage)
                self.assertEqual(copied.stat().st_size, executable.stat().st_size)
                with copied.open("rb") as stream:
                    self.assertEqual(stream.read(12), b"pebrel-start")
                    stream.seek(64 * MIB)
                    self.assertEqual(stream.read(), b"pebrel-end")
                self.assertEqual(os.readlink(mount / "Applications"), "/Applications")
            finally:
                subprocess.run(["hdiutil", "detach", str(mount)], check=True,
                               capture_output=True)


if __name__ == "__main__":
    unittest.main()
