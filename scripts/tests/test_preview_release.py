from __future__ import annotations

from pathlib import Path
import json
import os
import tempfile
import unittest

from scripts.preview_release import (
    MIN_ASSET_SIZE,
    ManifestError,
    asset_version,
    expected_asset_names,
    preview_notes,
    read_cargo_package_version,
    reviewed_notes,
    validate_assets,
    validate_evidence,
    verify_binary_freshness,
    verify_release,
    sha256,
    RUNTIME_REPORTS,
)


def write_fake_asset(path: Path) -> None:
    if path.name.endswith(".AppImage"):
        header = b"\x7fELF\x02\x01\x01\x00"
    elif path.suffix == ".deb":
        header = b"!<arch>\n"
    elif path.name.endswith(".tar.gz"):
        header = b"\x1f\x8b\x08\x00\x00\x00\x00\x00"
    else:
        header = b"\x00" * 8

    with path.open("wb") as stream:
        stream.write(header)
        stream.truncate(MIN_ASSET_SIZE)
        if path.suffix == ".dmg":
            stream.seek(-512, 2)
            stream.write(b"koly")


class PreviewReleaseTests(unittest.TestCase):
    def write_reviewed_notes(self, root: Path) -> Path:
        changes = "## English\n\n### Added\n\n- Preview packages.\n\n## 中文\n\n### 新增\n\n- 预览安装包。"
        source = root / "docs/release-notes/v1.5.0-preview.42.md"
        source.parent.mkdir(parents=True)
        source.write_text(
            "# Nebula Terminal 1.5.0 Preview 42\n\n" + changes +
            "\n\n## Contributors\n\n- Contributors\n\n"
            "macOS signing / macOS 签名: `adhoc`\n\n---\n\n## SHA256\n\n<!-- PREVIEW_SHA256 -->\n",
            encoding="utf-8",
        )
        changelog_changes = "\n".join("#" + line if line.startswith("#") else line for line in changes.splitlines())
        (root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## Unreleased\n\nNone.\n\n## 1.5.0-preview.42 - 2026-09-05\n\n" +
            changelog_changes + "\n\n## 1.5.0 - 2026-09-01\n\nOld notes.\n", encoding="utf-8",
        )
        return source

    def test_public_preview_requires_committed_synchronized_notes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = self.write_reviewed_notes(root)
            self.assertEqual(reviewed_notes(root, "1.5.0", "42", "adhoc"), source.read_text(encoding="utf-8"))
            changelog = root / "CHANGELOG.md"
            changelog.write_text(changelog.read_text(encoding="utf-8").replace("Preview packages.", "Other changes."), encoding="utf-8")
            with self.assertRaisesRegex(ManifestError, "different changes"):
                reviewed_notes(root, "1.5.0", "42", "adhoc")

    def test_public_preview_rejects_missing_notes_or_wrong_signing_mode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(OSError):
                reviewed_notes(root, "1.5.0", "42", "adhoc")
            self.write_reviewed_notes(root)
            with self.assertRaisesRegex(ManifestError, "signing mode"):
                reviewed_notes(root, "1.5.0", "42", "developer-id")
            with self.assertRaises(OSError):
                reviewed_notes(root, "1.5.0", "43", "adhoc")

    def test_public_preview_rejects_stale_checksums(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = self.write_reviewed_notes(root)
            source.write_text(source.read_text(encoding="utf-8").replace("<!-- PREVIEW_SHA256 -->", "old-checksum"), encoding="utf-8")
            with self.assertRaisesRegex(ManifestError, "stale checksums"):
                reviewed_notes(root, "1.5.0", "42", "adhoc")

    def test_public_preview_requires_matching_bilingual_categories(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = self.write_reviewed_notes(root)
            source.write_text(source.read_text(encoding="utf-8").replace("### 新增", "### 修复"), encoding="utf-8")
            with self.assertRaisesRegex(ManifestError, "bilingual"):
                reviewed_notes(root, "1.5.0", "42", "adhoc")

    def test_packaging_rejects_stale_binary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "nebula_app/res/shell/zshrc"
            source.parent.mkdir(parents=True)
            source.write_text("updated script", encoding="utf-8")
            binary = root / "nebula"
            binary.write_bytes(b"binary")
            os.utime(binary, (10, 10))
            os.utime(source, (20, 20))
            with self.assertRaisesRegex(ManifestError, "rebuild GPUI"):
                verify_binary_freshness(binary, root)
            os.utime(binary, (30, 30))
            verify_binary_freshness(binary, root)

    def test_reads_only_top_level_cargo_package_version(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            manifest = Path(raw_directory) / "Cargo.toml"
            manifest.write_text(
                '[package]\nname = "nebula"\nversion = "1.5.0"\n\n'
                '[dependencies]\nversion = "99.0.0"\n',
                encoding="utf-8",
            )
            self.assertEqual(read_cargo_package_version(manifest), "1.5.0")

    def test_asset_version_rejects_path_like_preview_id(self) -> None:
        self.assertEqual(asset_version("1.5.0", "42.1"), "1.5.0-preview.42.1")
        with self.assertRaises(ManifestError):
            asset_version("1.5.0", "../../release")

    def test_complete_manifest_passes(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            for name in expected_asset_names("1.5.0", "42"):
                write_fake_asset(directory / name)
            assets = validate_assets(directory, "1.5.0", "42")
            self.assertEqual(len(assets), 5)

    def test_unexpected_asset_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            for name in expected_asset_names("1.5.0", "42"):
                write_fake_asset(directory / name)
            (directory / "old-build.zip").write_bytes(b"old")
            with self.assertRaisesRegex(ManifestError, "unexpected: old-build.zip"):
                validate_assets(directory, "1.5.0", "42")

    def test_bad_magic_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            names = expected_asset_names("1.5.0", "42")
            for name in names:
                write_fake_asset(directory / name)
            (directory / names[0]).write_bytes(b"not an appimage" + b"\0" * MIN_ASSET_SIZE)
            with self.assertRaisesRegex(ManifestError, "not an ELF"):
                validate_assets(directory, "1.5.0", "42")

    def test_notes_are_bilingual_and_disclose_preview_limits(self) -> None:
        notes = preview_notes(
            "1.5.0",
            "42",
            "Kuddev/nebula",
            "a" * 40,
            "deadbeef  package\n",
        )
        self.assertIn("## English", notes)
        self.assertIn("## 中文", notes)
        self.assertIn("ad-hoc", notes)
        self.assertIn("不是稳定版本", notes)
        self.assertIn("deadbeef  package", notes)

    def test_signed_notes_never_claim_adhoc(self) -> None:
        notes = preview_notes("1.5.0", "42", "Kuddev/nebula", "a" * 40, "hash  file", "developer-id")
        self.assertIn("Developer ID", notes)
        self.assertIn("公证票据", notes)
        self.assertNotIn("ad-hoc", notes)

    def test_missing_native_evidence_cannot_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(OSError):
                validate_evidence(Path(directory), "a" * 40)

    def test_complete_evidence_requires_same_commit_and_installed_launch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            common_path = Path(__file__).resolve().parents[1] / "conformance/golden/common.json"
            required = json.loads(common_path.read_text(encoding="utf-8"))["required"]
            report = {"schema_version": 1, "build": {"commit": "a" * 40}}
            for path, value in required.items():
                cursor = report
                parts = path.split(".")
                for part in parts[:-1]:
                    cursor = cursor.setdefault(part, {})
                cursor[parts[-1]] = value
            report["cases"]["ssh_loop"] = {"status": "skipped", "reason": "no_interactive_ssh_automation"}
            report["summary"].update(passed=9, skipped=1)
            for filename, platform in RUNTIME_REPORTS.items():
                report["platform"] = platform
                (root / filename).write_text(json.dumps(report), encoding="utf-8")
            for architecture in ("aarch64", "x86_64"):
                (root / f"macos-{architecture}-launch.json").write_text(json.dumps({
                    "status": "passed", "commit": "a" * 40, "architecture": architecture,
                    "launch_method": "launchservices", "utf8_locale": True, "home_cwd": True,
                }), encoding="utf-8")
            validate_evidence(root, "a" * 40)
            with self.assertRaisesRegex(ManifestError, "source commit"):
                validate_evidence(root, "b" * 40)
            (root / "macos-aarch64-launch.json").write_text('{}', encoding="utf-8")
            with self.assertRaisesRegex(ManifestError, "LaunchServices"):
                validate_evidence(root, "a" * 40)

    def test_remote_release_checks_assets_notes_labels_and_tag_commit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = [*expected_asset_names("1.5.0", "42"), "SHA256SUMS"]
            (root / "PREVIEW_NOTES.md").write_text("verified notes", encoding="utf-8")
            for name in names:
                (root / name).write_bytes(b"fixture")
            metadata = {
                "name": "Nebula Terminal 1.5.0 Preview 42", "tag_name": "preview-v1.5.0-42",
                "prerelease": True, "draft": False, "body": "verified notes",
                "assets": [{"name": name, "label": "", "state": "uploaded", "size": 7,
                            "digest": f"sha256:{sha256(root / name)}"} for name in names],
            }
            verify_release(metadata, root, "1.5.0", "42", "a" * 40, "a" * 40)
            with self.assertRaisesRegex(ManifestError, "commit"):
                verify_release(metadata, root, "1.5.0", "42", "a" * 40, "b" * 40)
            metadata["assets"][0]["label"] = "wrong label"
            with self.assertRaisesRegex(ManifestError, "label"):
                verify_release(metadata, root, "1.5.0", "42", "a" * 40, "a" * 40)


if __name__ == "__main__":
    unittest.main()
