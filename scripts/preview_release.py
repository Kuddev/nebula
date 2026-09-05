#!/usr/bin/env python3
"""Validate and describe the complete cross-platform Preview asset set."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys


MIN_ASSET_SIZE = 1024 * 1024
VERSION_PATTERN = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"
    r"(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$"
)
PREVIEW_PATTERN = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.-]{0,31}$")
CHECKSUM_PLACEHOLDER = "<!-- PREVIEW_SHA256 -->"
PACKAGE_VERSION_PATTERN = re.compile(r'^version\s*=\s*"([^"]+)"\s*(?:#.*)?$')
RUNTIME_REPORTS = {
    "windows-report.json": "windows-x86_64",
    "linux-appimage-report.json": "linux-x86_64-appimage",
    "linux-appimage-wayland-report.json": "linux-x86_64-appimage-wayland",
    "linux-deb-report.json": "linux-x86_64-deb",
    "macos-aarch64-report.json": "macos-aarch64",
    "macos-x86_64-report.json": "macos-x86_64",
}


class ManifestError(ValueError):
    """The Preview asset set is incomplete or unsafe to publish."""


def verify_binary_freshness(binary: Path, repo: Path) -> None:
    sources = [repo / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "CHANGELOG.md")]
    for name in ("nebula_app", "nebula_terminal", "nebula_config", "nebula_config_derive",
                 "nebula_settings", "nebula_split", "nebula-completions", "assets", "extra", "third_party"):
        root = repo / name
        if root.is_dir():
            sources.extend(path for path in root.rglob("*") if path.is_file())
    timestamp = binary.stat().st_mtime_ns
    stale = next((path for path in sources if path.is_file() and path.stat().st_mtime_ns > timestamp), None)
    if stale:
        raise ManifestError(f"binary is older than {stale.relative_to(repo)}; rebuild GPUI before packaging")


def validate_evidence(directory: Path, commit: str) -> None:
    root = str(Path(__file__).resolve().parents[1])
    if root not in sys.path:
        sys.path.insert(0, root)
    from scripts.conformance.harness import compare_reports, validate_common

    golden = Path(__file__).resolve().parent / "conformance/golden"
    reports = []
    for name, platform in RUNTIME_REPORTS.items():
        report = json.loads((directory / name).read_text(encoding="utf-8"))
        if report.get("platform") != platform or report.get("build", {}).get("commit") != commit:
            raise ManifestError(f"wrong platform or source commit in {name}")
        cases = report.get("cases", {})
        if (report.get("schema_version") != 1 or len(cases) != report.get("summary", {}).get("total")
                or any(case.get("status") not in {"passed", "skipped"} for case in cases.values())):
            raise ManifestError(f"failed or incomplete conformance report: {name}")
        errors = validate_common(report, golden)
        if errors:
            raise ManifestError(f"{name}: {'; '.join(errors)}")
        reports.append(report)
    errors = compare_reports(reports, golden)
    if errors:
        raise ManifestError("cross-platform mismatch: " + "; ".join(errors))
    for architecture in ("aarch64", "x86_64"):
        name = f"macos-{architecture}-launch.json"
        report = json.loads((directory / name).read_text(encoding="utf-8"))
        if (report.get("status") != "passed" or report.get("commit") != commit
                or report.get("architecture") != architecture
                or report.get("launch_method") != "launchservices"
                or report.get("utf8_locale") is not True or report.get("home_cwd") is not True):
            raise ManifestError(f"installed macOS application did not pass LaunchServices smoke test: {name}")


def verify_release(metadata: dict, directory: Path, version: str, preview_id: str,
                   commit: str, tag_commit: str) -> None:
    expected_title = f"Nebula Terminal {version} Preview {preview_id}"
    expected_tag = f"preview-v{version}-{preview_id}"
    if (metadata.get("name") != expected_title or metadata.get("tag_name") != expected_tag
            or metadata.get("prerelease") is not True or metadata.get("draft") is not False
            or tag_commit != commit):
        raise ManifestError("remote Preview title, tag, commit, or prerelease state differs")
    notes = (directory / "PREVIEW_NOTES.md").read_text(encoding="utf-8")
    if (metadata.get("body") or "").strip() != notes.strip():
        raise ManifestError("remote Preview notes differ from the verified local notes")
    names = set(expected_asset_names(version, preview_id)) | {"SHA256SUMS"}
    assets = metadata.get("assets") or []
    if len(assets) != len(names) or {asset.get("name") for asset in assets} != names:
        raise ManifestError("remote Preview asset names differ")
    for asset in assets:
        path = directory / asset["name"]
        if (asset.get("label") not in (None, "") or asset.get("state") != "uploaded"
                or asset.get("size") != path.stat().st_size
                or asset.get("digest") != f"sha256:{sha256(path)}"):
            raise ManifestError(f"remote asset label, state, size, or SHA256 differs: {path.name}")


def validate_labels(version: str, preview_id: str) -> None:
    if not VERSION_PATTERN.fullmatch(version):
        raise ManifestError(f"invalid Cargo package version: {version!r}")
    if not PREVIEW_PATTERN.fullmatch(preview_id):
        raise ManifestError(f"invalid Preview id: {preview_id!r}")


def read_cargo_package_version(manifest: Path) -> str:
    """Read only the top-level [package].version from one Cargo manifest."""
    in_package = False
    for raw_line in manifest.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line == "[package]":
            in_package = True
            continue
        if in_package and line.startswith("["):
            break
        if in_package:
            match = PACKAGE_VERSION_PATTERN.fullmatch(line)
            if match:
                version = match.group(1)
                if not VERSION_PATTERN.fullmatch(version):
                    raise ManifestError(
                        f"invalid [package].version in {manifest}: {version!r}"
                    )
                return version
    raise ManifestError(f"Cargo manifest has no [package].version: {manifest}")


def asset_version(version: str, preview_id: str) -> str:
    validate_labels(version, preview_id)
    return f"{version}-preview.{preview_id}"


def reviewed_notes(repo: Path, version: str, preview_id: str, macos_signing: str) -> str:
    release = asset_version(version, preview_id)
    source = repo / "docs" / "release-notes" / f"v{release}.md"
    notes = source.read_text(encoding="utf-8").strip()
    if not notes.startswith(f"# Nebula Terminal {version} Preview {preview_id}\n"):
        raise ManifestError("reviewed Preview notes have the wrong release title")
    for heading in ("## English", "## 中文", "## Contributors", "## SHA256"):
        if notes.splitlines().count(heading) != 1:
            raise ManifestError(f"reviewed Preview notes require one {heading} section")
    body, _, checksum_section = notes.rpartition("\n## SHA256\n")
    if checksum_section.strip() != CHECKSUM_PLACEHOLDER or notes.count(CHECKSUM_PLACEHOLDER) != 1:
        raise ManifestError("reviewed notes require the SHA256 placeholder, not stale checksums")
    signing_line = f"macOS signing / macOS 签名: `{macos_signing}`"
    if signing_line not in body.splitlines():
        raise ManifestError("reviewed notes do not match the requested macOS signing mode")
    english_start = body.index("\n## English\n") + 1
    chinese_start = body.index("\n## 中文\n") + 1
    contributors_start = body.index("\n## Contributors\n") + 1
    if not english_start < chinese_start < contributors_start:
        raise ManifestError("reviewed notes must order English, 中文, then Contributors")
    english = body[english_start:chinese_start]
    chinese = body[chinese_start:contributors_start]
    pairs = (("Added", "新增"), ("Fixed", "修复"), ("Improved", "改进"))
    categories = 0
    for first, second in pairs:
        present = f"### {first}" in english.splitlines()
        if present != (f"### {second}" in chinese.splitlines()):
            raise ManifestError("reviewed Preview change categories must be bilingual")
        categories += present
    if not categories:
        raise ManifestError("reviewed Preview notes have no Added, Fixed, or Improved changes")
    changelog = (repo / "CHANGELOG.md").read_text(encoding="utf-8")
    heading = re.search(rf"(?m)^## {re.escape(release)}(?: - [^\n]*)?\n", changelog)
    if heading is None:
        raise ManifestError(f"CHANGELOG.md is missing the {release} entry")
    entry = re.split(r"(?m)^## ", changelog[heading.end():], maxsplit=1)[0].strip()
    entry = re.sub(r"(?m)^#(?=##)", "", entry)
    if entry != body[english_start:contributors_start].strip():
        raise ManifestError("CHANGELOG.md and reviewed Preview notes describe different changes")
    return notes + "\n"


def expected_asset_names(version: str, preview_id: str) -> tuple[str, ...]:
    release = asset_version(version, preview_id)
    return (
        f"NebulaTerminal-v{release}-linux-x86_64.AppImage",
        f"NebulaTerminal-v{release}-linux-x86_64.deb",
        f"NebulaTerminal-v{release}-linux-x86_64.tar.gz",
        f"NebulaTerminal-v{release}-macos-aarch64.dmg",
        f"NebulaTerminal-v{release}-macos-x86_64.dmg",
    )


def _check_magic(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(8)
        if path.suffix == ".dmg":
            stream.seek(-512, os.SEEK_END)
            trailer = stream.read(4)
        else:
            trailer = b""

    if path.name.endswith(".AppImage") and not header.startswith(b"\x7fELF"):
        raise ManifestError(f"AppImage is not an ELF executable: {path.name}")
    if path.suffix == ".deb" and header != b"!<arch>\n":
        raise ManifestError(f"Debian package has an invalid ar header: {path.name}")
    if path.name.endswith(".tar.gz") and not header.startswith(b"\x1f\x8b"):
        raise ManifestError(f"tar.gz has an invalid gzip header: {path.name}")
    if path.suffix == ".dmg" and trailer != b"koly":
        raise ManifestError(f"DMG has no UDIF trailer: {path.name}")


def validate_assets(directory: Path, version: str, preview_id: str) -> list[Path]:
    expected = set(expected_asset_names(version, preview_id))
    generated = {"SHA256SUMS", "PREVIEW_NOTES.md"}
    if not directory.is_dir():
        raise ManifestError(f"asset directory does not exist: {directory}")

    actual = {path.name for path in directory.iterdir() if path.name not in generated}
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        details: list[str] = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        raise ManifestError("Preview asset manifest differs: " + "; ".join(details))

    assets = [directory / name for name in sorted(expected)]
    for path in assets:
        if not path.is_file() or path.is_symlink():
            raise ManifestError(f"asset is not a regular file: {path.name}")
        size = path.stat().st_size
        if size < MIN_ASSET_SIZE:
            raise ManifestError(
                f"asset is unexpectedly small ({size} bytes): {path.name}"
            )
        _check_magic(path)
    return assets


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        temporary.write_text(content, encoding="utf-8", newline="\n")
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def checksum_text(assets: list[Path]) -> str:
    return "".join(f"{sha256(path)}  {path.name}\n" for path in assets)


def preview_notes(
    version: str,
    preview_id: str,
    repository: str,
    commit: str,
    checksums: str,
    macos_signing: str = "adhoc",
) -> str:
    short_commit = commit[:12]
    commit_url = f"https://github.com/{repository}/commit/{commit}"
    title = f"Nebula Terminal {version} Preview {preview_id}"
    signing_en = (
        "The macOS applications use ad-hoc signatures and are not Apple-notarized. Only open a verified, trusted download via System Settings > Privacy & Security > Open Anyway."
        if macos_signing == "adhoc" else
        "The macOS applications use Developer ID signatures; the final DMGs were accepted by Apple notarization and have stapled tickets."
    )
    signing_zh = (
        "macOS 应用仅使用 ad-hoc 签名，尚未经过 Apple 公证；仅对已核验且可信的下载在系统设置 → 隐私与安全性中选择“仍要打开”。"
        if macos_signing == "adhoc" else
        "macOS 应用使用 Developer ID 签名；最终 DMG 已获 Apple 公证接受并附加公证票据。"
    )
    return f"""# {title}

## English

### Added

- Added automated Preview packages for Linux x86_64: AppImage, Debian package, and portable tar archive.
- Added native macOS Preview disk images for Apple Silicon and Intel.

### Verification

- Every package was built on a native GitHub-hosted runner from commit [`{short_commit}`]({commit_url}).
- Linux conformance ran against both the final AppImage and the installed Debian package.
- macOS conformance ran against the application from each final disk image, including a separate installed-copy LaunchServices startup check.
- Runtime reports were compared with a Windows build of the same commit. This does not certify native IME, display scaling, or interactive SSH authentication.

### Preview limitations

- These packages are for cross-platform testing and are not a stable release.
- {signing_en}
- Linux/macOS automatic update installation and platform-specific integrations remain outside this Preview.

## 中文

### 新增

- 新增由 CI 自动构建的 Linux x86_64 Preview 包：AppImage、Debian 安装包和便携 tar 归档。
- 新增 Apple Silicon 和 Intel 两种原生 macOS Preview 磁盘映像。

### 验证

- 所有安装包均由 GitHub 原生 runner 从提交 [`{short_commit}`]({commit_url}) 构建。
- Linux conformance 分别针对最终 AppImage 和已安装的 Debian 包运行。
- macOS conformance 针对每个最终 DMG 中的应用运行，并单独检查安装副本通过 LaunchServices 启动。
- Runtime 报告与同一提交的 Windows 构建进行了比较；这不代表原生输入法、显示缩放或交互式 SSH 认证已通过验收。

### Preview 限制

- 这些产物用于跨平台测试，不是稳定版本。
- {signing_zh}
- Linux/macOS 自动安装更新和平台专属集成功能不在本次 Preview 范围内。

## Contributors

- See the commit history for this Preview build / 本 Preview 的贡献者以提交历史为准。

---

## SHA256

```text
{checksums.rstrip()}
```
"""


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", nargs="?", type=Path)
    parser.add_argument("--version")
    parser.add_argument("--preview-id")
    parser.add_argument("--write-checksums", type=Path)
    parser.add_argument("--write-notes", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--commit")
    parser.add_argument("--print-package-version", type=Path)
    parser.add_argument("--check-binary", type=Path)
    parser.add_argument("--check-reviewed-notes", action="store_true")
    parser.add_argument("--use-reviewed-notes", action="store_true")
    parser.add_argument("--evidence-dir", type=Path)
    parser.add_argument("--macos-signing", choices=("adhoc", "developer-id"), default="adhoc")
    parser.add_argument("--verify-release", type=Path)
    parser.add_argument("--tag-commit")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.print_package_version:
            print(read_cargo_package_version(args.print_package_version.resolve()))
            return 0
        if args.check_binary:
            verify_binary_freshness(args.check_binary.resolve(), Path(__file__).resolve().parents[1])
            return 0
        source_notes = None
        if args.check_reviewed_notes or args.use_reviewed_notes:
            if not args.version or not args.preview_id:
                raise ManifestError("reviewed notes require --version and --preview-id")
            source_notes = reviewed_notes(Path(__file__).resolve().parents[1], args.version,
                                          args.preview_id, args.macos_signing)
            if args.check_reviewed_notes:
                return 0
        if args.evidence_dir:
            if not args.commit:
                raise ManifestError("--evidence-dir requires --commit")
            validate_evidence(args.evidence_dir.resolve(), args.commit)
        if args.directory is None or not args.version or not args.preview_id:
            raise ManifestError(
                "asset validation requires directory, --version, and --preview-id"
            )
        assets = validate_assets(args.directory.resolve(), args.version, args.preview_id)
        if args.verify_release:
            if not args.commit or not args.tag_commit:
                raise ManifestError("--verify-release requires --commit and --tag-commit")
            verify_release(json.loads(args.verify_release.read_text(encoding="utf-8")),
                           args.directory.resolve(), args.version, args.preview_id, args.commit, args.tag_commit)
        checksums = checksum_text(assets)
        if args.write_checksums:
            write_atomic(args.write_checksums.resolve(), checksums)
        if args.write_notes:
            if not args.repository or not args.commit or not args.evidence_dir:
                raise ManifestError(
                    "--write-notes requires --repository, --commit, and validated --evidence-dir"
                )
            if source_notes is not None:
                notes = source_notes.replace(CHECKSUM_PLACEHOLDER, f"```text\n{checksums.rstrip()}\n```")
            else:
                notes = preview_notes(
                    args.version,
                    args.preview_id,
                    args.repository,
                    args.commit,
                    checksums,
                    args.macos_signing,
                )
            write_atomic(args.write_notes.resolve(), notes)
    except (ManifestError, OSError, json.JSONDecodeError) as error:
        print(f"preview manifest error: {error}", file=sys.stderr)
        return 1

    for path in assets:
        print(f"{sha256(path)}  {path.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
