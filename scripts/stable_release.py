#!/usr/bin/env python3
"""Validate the complete stable Pebrel asset set before publishing a Release."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys

if __package__:
    from scripts.preview_release import MIN_ASSET_SIZE, VERSION_PATTERN, sha256, write_atomic
else:
    # The release workflow invokes this file directly. In that mode Python puts
    # `scripts/` on sys.path rather than the repository root.
    from preview_release import MIN_ASSET_SIZE, VERSION_PATTERN, sha256, write_atomic


STABLE_SHA256_PLACEHOLDER = "<!-- STABLE_SHA256 -->"
PENDING_SHA256 = "PENDING FINAL BUILD"
STABLE_VERSION_PATTERN = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")


class StableReleaseError(ValueError):
    """The stable asset set or release notes are incomplete or unsafe."""


def validate_version(version: str) -> None:
    if not VERSION_PATTERN.fullmatch(version) or not STABLE_VERSION_PATTERN.fullmatch(version):
        raise StableReleaseError(f"invalid stable version: {version!r}")


def expected_asset_names(version: str) -> tuple[str, ...]:
    validate_version(version)
    return (
        f"Pebrel-v{version}-linux-x64-preview.AppImage",
        f"Pebrel-v{version}-linux-x64-preview.deb",
        f"Pebrel-v{version}-linux-x64-preview.tar.gz",
        f"Pebrel-v{version}-macos-arm64-preview.dmg",
        f"Pebrel-v{version}-macos-x64-preview.dmg",
        f"Pebrel-v{version}-windows-x64.zip",
        f"Pebrel-v{version}-windows-x64-setup.exe",
        f"NebulaTerminal-{version}-windows-x64-setup.exe",
    )


def mark_platform_previews(directory: Path, version: str) -> None:
    for name in expected_asset_names(version):
        if "-preview." not in name:
            continue
        original = directory / name.replace("-preview.", ".", 1)
        target = directory / name
        if original.exists():
            if target.exists() or original.is_symlink() or not original.is_file():
                raise StableReleaseError(f"cannot mark platform preview without replacing a file: {name}")
            original.rename(target)


def _check_magic(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(8)
        trailer = b""
        if path.suffix == ".dmg":
            if path.stat().st_size < 512:
                raise StableReleaseError(f"DMG is too small to contain a UDIF trailer: {path.name}")
            stream.seek(-512, os.SEEK_END)
            trailer = stream.read(4)

    if path.suffix == ".exe" and not header.startswith(b"MZ"):
        raise StableReleaseError(f"Windows installer has no PE header: {path.name}")
    if path.suffix == ".zip" and not header.startswith((b"PK\x03\x04", b"PK\x05\x06")):
        raise StableReleaseError(f"Windows ZIP has an invalid ZIP header: {path.name}")
    if path.name.endswith(".AppImage") and not header.startswith(b"\x7fELF"):
        raise StableReleaseError(f"AppImage is not an ELF executable: {path.name}")
    if path.suffix == ".deb" and header != b"!<arch>\n":
        raise StableReleaseError(f"Debian package has an invalid ar header: {path.name}")
    if path.name.endswith(".tar.gz") and not header.startswith(b"\x1f\x8b"):
        raise StableReleaseError(f"tar.gz has an invalid gzip header: {path.name}")
    if path.suffix == ".dmg" and trailer != b"koly":
        raise StableReleaseError(f"DMG has no UDIF trailer: {path.name}")


def validate_assets(directory: Path, version: str) -> list[Path]:
    expected = set(expected_asset_names(version))
    generated = {"SHA256SUMS", "RELEASE_NOTES.md"}
    if not directory.is_dir():
        raise StableReleaseError(f"asset directory does not exist: {directory}")

    actual = {path.name for path in directory.iterdir() if path.name not in generated}
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        details: list[str] = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        raise StableReleaseError("stable asset manifest differs: " + "; ".join(details))

    assets = [directory / name for name in sorted(expected)]
    for path in assets:
        if not path.is_file() or path.is_symlink():
            raise StableReleaseError(f"asset is not a regular file: {path.name}")
        if path.stat().st_size < MIN_ASSET_SIZE:
            raise StableReleaseError(f"asset is unexpectedly small: {path.name}")
        _check_magic(path)

    installer = directory / f"Pebrel-v{version}-windows-x64-setup.exe"
    legacy = directory / f"NebulaTerminal-{version}-windows-x64-setup.exe"
    if sha256(installer) != sha256(legacy):
        raise StableReleaseError("legacy Windows installer alias is not byte-identical to Pebrel installer")
    return assets


def validate_notes(source: Path, version: str) -> str:
    notes = source.read_text(encoding="utf-8").strip()
    if not notes.startswith(f"# Pebrel {version}\n"):
        raise StableReleaseError("stable release notes have the wrong title")
    for heading in ("## English", "## 中文", "## Contributors"):
        if notes.splitlines().count(heading) != 1:
            raise StableReleaseError(f"stable release notes require one {heading} section")
    checksum_headings = ("\n## SHA256\n", "\n**SHA256**\n")
    present_checksums = [heading for heading in checksum_headings if notes.count(heading) == 1]
    if len(present_checksums) != 1:
        raise StableReleaseError("stable release notes require one SHA256 section")
    english_start = notes.index("\n## English\n")
    chinese_start = notes.index("\n## 中文\n")
    contributors_start = notes.index("\n## Contributors\n")
    checksum_start = notes.index(present_checksums[0])
    if not english_start < chinese_start < contributors_start < checksum_start:
        raise StableReleaseError("stable release notes sections are out of order")
    pairs = (("Added", "新增"), ("Fixed", "修复"), ("Improved", "改进"))
    english = notes[english_start:chinese_start]
    chinese = notes[chinese_start:contributors_start]
    if not any(
        f"### {en}" in english and f"### {zh}" in chinese for en, zh in pairs
    ):
        raise StableReleaseError("stable release notes need matching bilingual change categories")
    contributors = notes[contributors_start:checksum_start]
    if not re.search(r"github\.com/[^)]+", contributors, flags=re.IGNORECASE):
        raise StableReleaseError("stable release notes must identify contributors with GitHub links")
    checksum_body = notes[checksum_start:].strip()
    if STABLE_SHA256_PLACEHOLDER not in checksum_body:
        listed_names = set(re.findall(r"`([^`]+)`:\s*`(?:[0-9a-fA-F]{64}|PENDING FINAL BUILD)`", checksum_body))
        listed_names.update(
            match.group(1)
            for match in re.finditer(r"(?m)^\s*[0-9a-fA-F]{64}\s{2}(\S+)\s*$", checksum_body)
        )
        expected_names = set(expected_asset_names(version))
        if listed_names != expected_names:
            missing = sorted(expected_names - listed_names)
            unexpected = sorted(listed_names - expected_names)
            details = []
            if missing:
                details.append("missing: " + ", ".join(missing))
            if unexpected:
                details.append("unexpected: " + ", ".join(unexpected))
            raise StableReleaseError("stable release notes SHA256 asset names differ: " + "; ".join(details))
    if (
        STABLE_SHA256_PLACEHOLDER not in checksum_body
        and PENDING_SHA256 not in checksum_body
        and not re.search(r"[0-9a-fA-F]{64}", checksum_body)
    ):
        raise StableReleaseError("stable release notes need a SHA256 placeholder or checksums")
    return notes + "\n"


def checksum_text(assets: list[Path]) -> str:
    return "".join(f"{sha256(path)}  {path.name}\n" for path in assets)


def validate_changelog(source: Path, version: str) -> str:
    changelog = source.read_text(encoding="utf-8")
    match = re.search(rf"(?m)^## {re.escape(version)}(?: - [^\n]*)?\n", changelog)
    if match is None:
        raise StableReleaseError(f"CHANGELOG.md is missing the {version} entry")
    entry = re.split(r"(?m)^## ", changelog[match.end() :], maxsplit=1)[0]
    if not entry.strip():
        raise StableReleaseError(f"CHANGELOG.md has an empty {version} entry")
    missing = [name for name in expected_asset_names(version) if name not in entry]
    if missing:
        raise StableReleaseError("CHANGELOG.md is missing stable asset names: " + ", ".join(missing))
    return entry


def verify_release(metadata: dict, directory: Path, version: str, commit: str, tag_commit: str) -> None:
    expected_names = set(expected_asset_names(version)) | {"SHA256SUMS"}
    if (
        metadata.get("name") != f"Pebrel {version}"
        or metadata.get("tag_name") != f"v{version}"
        or metadata.get("draft") is not False
        or metadata.get("prerelease") is not False
        or tag_commit != commit
    ):
        raise StableReleaseError("remote stable title, tag, commit, or publication state differs")
    notes = (directory / "RELEASE_NOTES.md").read_text(encoding="utf-8")
    if (metadata.get("body") or "").strip() != notes.strip():
        raise StableReleaseError("remote stable Release notes differ from the verified local notes")
    assets = metadata.get("assets") or []
    if len(assets) != len(expected_names) or {asset.get("name") for asset in assets} != expected_names:
        raise StableReleaseError("remote stable asset names differ")
    for asset in assets:
        path = directory / asset["name"]
        if (
            asset.get("label") not in (None, "")
            or asset.get("state") != "uploaded"
            or asset.get("size") != path.stat().st_size
            or asset.get("digest") != f"sha256:{sha256(path)}"
        ):
            raise StableReleaseError(f"remote asset label, state, size, or SHA256 differs: {path.name}")


def render_notes(notes: str, checksums: str) -> str:
    if STABLE_SHA256_PLACEHOLDER in notes:
        return notes.replace(STABLE_SHA256_PLACEHOLDER, f"```text\n{checksums.rstrip()}\n```")
    if PENDING_SHA256 in notes:
        rendered = notes
        for line in checksums.splitlines():
            digest, name = line.split("  ", 1)
            rendered = re.sub(
                rf"(`{re.escape(name)}`:\s*`){re.escape(PENDING_SHA256)}(`)",
                rf"\g<1>{digest}\g<2>",
                rendered,
            )
        if PENDING_SHA256 in rendered:
            raise StableReleaseError("stable release notes contain an unknown pending checksum entry")
        return rendered
    else:
        missing = [line for line in checksums.splitlines() if line not in notes]
        if missing:
            raise StableReleaseError(
                "stable release notes have stale or incomplete SHA256 entries: "
                + ", ".join(line.rsplit("  ", 1)[-1] for line in missing)
            )
        return notes


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--mark-platform-previews", action="store_true")
    parser.add_argument("--notes", type=Path)
    parser.add_argument("--changelog", type=Path)
    parser.add_argument("--write-checksums", type=Path)
    parser.add_argument("--write-notes", type=Path)
    parser.add_argument("--verify-release", type=Path)
    parser.add_argument("--commit")
    parser.add_argument("--tag-commit")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.mark_platform_previews:
            mark_platform_previews(args.directory.resolve(), args.version)
        assets = validate_assets(args.directory.resolve(), args.version)
        notes = None
        if args.notes:
            notes = validate_notes(args.notes.resolve(), args.version)
        if args.changelog:
            validate_changelog(args.changelog.resolve(), args.version)
        checksums = checksum_text(assets)
        if args.write_checksums:
            write_atomic(args.write_checksums.resolve(), checksums)
        if args.write_notes:
            if notes is None:
                raise StableReleaseError("--write-notes requires --notes")
            write_atomic(args.write_notes.resolve(), render_notes(notes, checksums))
        if args.verify_release:
            if not args.commit or not args.tag_commit:
                raise StableReleaseError("--verify-release requires --commit and --tag-commit")
            verify_release(
                json.loads(args.verify_release.read_text(encoding="utf-8")),
                args.directory.resolve(),
                args.version,
                args.commit,
                args.tag_commit,
            )
        for path in assets:
            print(f"{sha256(path)}  {path.name}")
    except (StableReleaseError, OSError, json.JSONDecodeError) as error:
        print(f"stable release error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
