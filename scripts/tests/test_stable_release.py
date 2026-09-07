from __future__ import annotations

from pathlib import Path
import tempfile
import unittest

from scripts.stable_release import (
    STABLE_SHA256_PLACEHOLDER,
    StableReleaseError,
    expected_asset_names,
    render_notes,
    validate_assets,
    validate_changelog,
    validate_notes,
    verify_release,
)
from scripts.preview_release import sha256
from scripts.preview_release import MIN_ASSET_SIZE


VERSION = "1.6.0"


def write_fake_asset(path: Path) -> None:
    if path.suffix == ".exe":
        header = b"MZ" + b"\0" * 6
    elif path.suffix == ".zip":
        header = b"PK\x03\x04" + b"\0" * 4
    elif path.name.endswith(".AppImage"):
        header = b"\x7fELF\x02\x01\x01\0"
    elif path.suffix == ".deb":
        header = b"!<arch>\n"
    elif path.name.endswith(".tar.gz"):
        header = b"\x1f\x8b\x08\0\0\0\0\0"
    elif path.suffix == ".dmg":
        header = b"\0" * 8
    else:
        raise AssertionError(path)
    path.write_bytes(header + b"\0" * (MIN_ASSET_SIZE - len(header)))
    if path.suffix == ".dmg":
        with path.open("r+b") as stream:
            stream.seek(-512, 2)
            stream.write(b"koly")


def notes(checksum_placeholder: bool = True) -> str:
    checksum = STABLE_SHA256_PLACEHOLDER if checksum_placeholder else "0" * 64
    return f"""# Pebrel {VERSION}

## English

### Added
- A stable package set.

## 中文

### 新增
- 稳定版安装包集合。

## Contributors

<a href=\"https://github.com/Kuddev\"><img src=\"https://github.com/Kuddev.png?size=96\" width=\"64\" height=\"64\" alt=\"@Kuddev avatar\"></a>

## SHA256

{checksum}
"""


class StableReleaseTests(unittest.TestCase):
    def test_native_packagers_expose_stable_channel_without_preview_id(self) -> None:
        root = Path(__file__).resolve().parents[2]
        linux = (root / "scripts/package-linux.sh").read_text(encoding="utf-8")
        macos = (root / "scripts/package-macos.sh").read_text(encoding="utf-8")
        for source in (linux, macos):
            self.assertIn('--channel', source)
            self.assertIn('channel" == "stable"', source)
            self.assertIn('stable packages must not include --preview-id', source)
        self.assertIn('asset_architecture="x64"', linux)
        self.assertIn('asset_architecture="x64"', macos)
        self.assertIn('asset_architecture="arm64"', macos)

    def test_expected_assets_use_stable_public_names(self) -> None:
        names = expected_asset_names(VERSION)
        self.assertIn("Pebrel-v1.6.0-linux-x64-preview.AppImage", names)
        self.assertIn("Pebrel-v1.6.0-macos-arm64-preview.dmg", names)
        self.assertIn("Pebrel-v1.6.0-macos-x64-preview.dmg", names)
        self.assertIn("Pebrel-v1.6.0-windows-x64-setup.exe", names)
        self.assertIn("NebulaTerminal-1.6.0-windows-x64-setup.exe", names)
        self.assertEqual(len(names), 8)

    def test_stable_manifest_rejects_prerelease_versions(self) -> None:
        with self.assertRaisesRegex(StableReleaseError, "invalid stable version"):
            expected_asset_names("1.6.0-rc.1")

    def test_complete_assets_require_identical_legacy_installer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in expected_asset_names(VERSION):
                write_fake_asset(root / name)
            self.assertEqual(len(validate_assets(root, VERSION)), 8)
            legacy = root / "NebulaTerminal-1.6.0-windows-x64-setup.exe"
            legacy.write_bytes(legacy.read_bytes()[:-1] + b"x")
            with self.assertRaisesRegex(StableReleaseError, "byte-identical"):
                validate_assets(root, VERSION)

    def test_asset_magic_is_checked(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in expected_asset_names(VERSION):
                write_fake_asset(root / name)
            (root / "Pebrel-v1.6.0-linux-x64-preview.AppImage").write_bytes(b"bad" + b"\0" * MIN_ASSET_SIZE)
            with self.assertRaisesRegex(StableReleaseError, "ELF"):
                validate_assets(root, VERSION)

    def test_notes_require_bilingual_categories_and_contributor_link(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "v1.6.0.md"
            source.write_text(notes(), encoding="utf-8")
            self.assertEqual(validate_notes(source, VERSION), notes())
            source.write_text(notes().replace("https://github.com/Kuddev", "https://example.invalid"), encoding="utf-8")
            with self.assertRaisesRegex(StableReleaseError, "GitHub links"):
                validate_notes(source, VERSION)

    def test_changelog_must_list_the_same_stable_asset_names(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "CHANGELOG.md"
            source.write_text(
                "## 1.6.0 - 2026-09-07\n\n"
                + "\n".join(f"- `{name}`: `PENDING FINAL BUILD`" for name in expected_asset_names(VERSION))
                + "\n",
                encoding="utf-8",
            )
            self.assertTrue(validate_changelog(source, VERSION))
            source.write_text(source.read_text(encoding="utf-8").replace("Pebrel-v1.6.0", "Pebrel-1.6.0"), encoding="utf-8")
            with self.assertRaisesRegex(StableReleaseError, "missing stable asset"):
                validate_changelog(source, VERSION)

    def test_render_notes_replaces_placeholder_with_all_hashes(self) -> None:
        rendered = render_notes(notes(), "a" * 64 + "  Pebrel-v1.6.0-windows-x64.zip\n")
        self.assertNotIn(STABLE_SHA256_PLACEHOLDER, rendered)
        self.assertIn("a" * 64 + "  Pebrel-v1.6.0-windows-x64.zip", rendered)

    def test_render_notes_replaces_pending_named_entries(self) -> None:
        source = notes().replace(STABLE_SHA256_PLACEHOLDER, "- `Pebrel-v1.6.0-windows-x64.zip`: `PENDING FINAL BUILD`")
        rendered = render_notes(source, "a" * 64 + "  Pebrel-v1.6.0-windows-x64.zip\n")
        self.assertIn("`Pebrel-v1.6.0-windows-x64.zip`: `" + "a" * 64, rendered)

    def test_render_notes_rejects_stale_explicit_hashes(self) -> None:
        with self.assertRaisesRegex(StableReleaseError, "stale or incomplete"):
            render_notes(notes(False), "a" * 64 + "  Pebrel-v1.6.0-windows-x64.zip\n")

    def test_remote_release_verification_checks_all_assets_and_notes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in expected_asset_names(VERSION):
                write_fake_asset(root / name)
            (root / "SHA256SUMS").write_text("checksums\n", encoding="utf-8")
            release_notes = notes().replace(STABLE_SHA256_PLACEHOLDER, "rendered checksums")
            (root / "RELEASE_NOTES.md").write_text(release_notes, encoding="utf-8")
            names = [*expected_asset_names(VERSION), "SHA256SUMS"]
            metadata = {
                "name": "Pebrel 1.6.0",
                "tag_name": "v1.6.0",
                "draft": False,
                "prerelease": False,
                "body": release_notes,
                "assets": [
                    {
                        "name": name,
                        "label": "",
                        "state": "uploaded",
                        "size": (root / name).stat().st_size,
                        "digest": f"sha256:{sha256(root / name)}",
                    }
                    for name in names
                ],
            }
            verify_release(metadata, root, VERSION, "a" * 40, "a" * 40)
            metadata["body"] = release_notes.replace("\n", "\r\n")
            verify_release(metadata, root, VERSION, "a" * 40, "a" * 40)
            metadata["assets"][0]["label"] = "wrong"
            with self.assertRaisesRegex(StableReleaseError, "label"):
                verify_release(metadata, root, VERSION, "a" * 40, "a" * 40)


if __name__ == "__main__":
    unittest.main()
