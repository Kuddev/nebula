#!/usr/bin/env python3
"""Run or compare Nebula cross-platform Runtime API conformance reports."""

from __future__ import annotations

import argparse
import importlib
import json
import os
import platform as host_platform
import sys
import tempfile
import time
from pathlib import Path
from types import ModuleType
from typing import Any

# `python scripts/conformance/run.py` puts this file's directory on sys.path,
# while the package itself lives one level higher as `conformance`.
SCRIPT_DIR = Path(__file__).resolve().parent
SCRIPTS_DIR = SCRIPT_DIR.parent
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from conformance.harness import (  # noqa: E402
    ConformanceContext,
    ConformanceError,
    ResolvedApp,
    SkipCase,
    compare_platform_golden,
    compare_reports,
    platform_family,
    stable_flat,
    validate_common,
)

SCHEMA_VERSION = 1
CASE_NAMES = (
    "boot",
    "echo",
    "resize",
    "split",
    "scrollback",
    "session",
    "ssh_loop",
    "paste",
    "cjk_roundtrip",
    "close",
)
RESERVED_CASE_FIELDS = {"status", "duration_ms", "error", "reason"}


def default_platform_name() -> str:
    system = host_platform.system().lower()
    family = {"windows": "windows", "darwin": "macos", "linux": "linux"}.get(
        system, system
    )
    machine = host_platform.machine().lower()
    architecture = {
        "amd64": "x86_64",
        "x64": "x86_64",
        "aarch64": "arm64",
    }.get(machine, machine or "unknown")
    return f"{family}-{architecture}"


def load_case(name: str) -> ModuleType:
    return importlib.import_module(f"conformance.cases.{name}")


def case_error(error: Exception) -> str:
    if isinstance(error, ConformanceError):
        return str(error)
    return f"{type(error).__name__}: {error}"


def execute_case(name: str, ctx: ConformanceContext) -> dict[str, Any]:
    print(f"[{name}] running", flush=True)
    started = time.monotonic()
    try:
        result = load_case(name).run(ctx)
        if not isinstance(result, dict):
            raise ConformanceError(f"case returned {type(result).__name__}, expected an object")
        collision = RESERVED_CASE_FIELDS.intersection(result)
        if collision:
            raise ConformanceError(f"case returned reserved fields: {sorted(collision)}")
        entry: dict[str, Any] = {"status": "passed"}
        entry.update(result)
        print(f"[{name}] passed", flush=True)
    except SkipCase as error:
        entry = {"status": "skipped", "reason": str(error)}
        print(f"[{name}] skipped: {error}", flush=True)
    except Exception as error:
        entry = {"status": "failed", "error": case_error(error)}
        print(f"[{name}] failed: {entry['error']}", flush=True)
    entry["duration_ms"] = round((time.monotonic() - started) * 1000)
    return entry


def summarize(cases: dict[str, dict[str, Any]]) -> dict[str, int]:
    return {
        "total": len(cases),
        "passed": sum(case.get("status") == "passed" for case in cases.values()),
        "failed": sum(case.get("status") == "failed" for case in cases.values()),
        "skipped": sum(case.get("status") == "skipped" for case in cases.values()),
    }


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def record_boot_failure(
    report: dict[str, Any], error: Exception, started: float
) -> None:
    report["cases"]["boot"] = {
        "status": "failed",
        "error": case_error(error),
        "duration_ms": round((time.monotonic() - started) * 1000),
    }
    print(f"[boot] failed: {case_error(error)}", flush=True)
    for name in CASE_NAMES[1:]:
        report["cases"][name] = {
            "status": "skipped",
            "reason": "dependency_boot_failed",
            "duration_ms": 0,
        }


def run_suite(args: argparse.Namespace) -> int:
    output = args.output.resolve()
    golden_dir = args.golden_dir.resolve()
    artifact_dir = (args.artifacts or output.parent / f"{output.stem}-artifacts").resolve()
    report: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "platform": args.platform,
        "build": {"commit": os.environ.get("GITHUB_SHA", "local")},
        "cases": {},
        "summary": {"total": 0, "passed": 0, "failed": 0, "skipped": 0},
    }
    app: ResolvedApp | None = None
    ctx: ConformanceContext | None = None
    isolated: tempfile.TemporaryDirectory[str] | None = None
    setup_started = time.monotonic()

    try:
        family = platform_family(args.platform)
        if family not in {"windows", "linux", "macos"}:
            raise ConformanceError(
                "--platform must start with windows, linux, or macos"
            )
        app = ResolvedApp(args.app)
        isolated = tempfile.TemporaryDirectory(prefix="nebula-conformance-")
        root = Path(isolated.name)
        ctx = ConformanceContext(
            app,
            args.platform,
            root / "config",
            root / "work",
            artifact_dir,
            startup_timeout=args.startup_timeout,
        )
        ctx.prepare()
        ctx.start()
    except Exception as error:
        record_boot_failure(report, error, setup_started)
    else:
        for index, name in enumerate(CASE_NAMES):
            report["cases"][name] = execute_case(name, ctx)
            if name == "boot" and report["cases"][name]["status"] != "passed":
                for remaining in CASE_NAMES[index + 1 :]:
                    report["cases"][remaining] = {
                        "status": "skipped",
                        "reason": "dependency_boot_failed",
                        "duration_ms": 0,
                    }
                break
    finally:
        if ctx is not None:
            ctx.stop(force=True)
        if app is not None:
            app.close()
        if isolated is not None:
            isolated.cleanup()
        report["summary"] = summarize(report["cases"])
        write_json(output, report)
        print(f"report: {output}", flush=True)

    errors = validate_common(report, golden_dir)
    failed = report["summary"]["failed"]
    if failed:
        errors.append(f"{failed} conformance case(s) failed")

    golden_path = golden_dir / f"{platform_family(args.platform)}.json"
    if args.update_golden:
        if errors:
            errors.append("platform golden was not updated because the run did not conform")
        else:
            write_json(golden_path, stable_flat(report, golden_dir))
            print(f"updated platform golden: {golden_path}", flush=True)
    elif args.no_platform_golden:
        print("platform golden comparison disabled", flush=True)
    elif golden_path.is_file():
        errors.extend(compare_platform_golden(report, golden_dir))
    else:
        print(
            f"platform golden absent; bootstrap with --update-golden after review: {golden_path}",
            flush=True,
        )

    if errors:
        print("conformance errors:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    summary = report["summary"]
    print(
        f"summary: {summary['passed']} passed, {summary['skipped']} skipped, "
        f"{summary['failed']} failed",
        flush=True,
    )
    return 0


def read_report(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ConformanceError(f"could not read report {path}: {error}") from error
    if not isinstance(value, dict):
        raise ConformanceError(f"report is not a JSON object: {path}")
    return value


def compare_mode(args: argparse.Namespace) -> int:
    if len(args.compare) < 2:
        raise ConformanceError("--compare requires at least two reports")
    reports = [read_report(path.resolve()) for path in args.compare]
    errors: list[str] = []
    for path, report in zip(args.compare, reports):
        errors.extend(
            f"{path}: {error}" for error in validate_common(report, args.golden_dir.resolve())
        )
    errors.extend(compare_reports(reports, args.golden_dir.resolve()))
    if errors:
        print("comparison errors:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print(f"reports conform across {len(reports)} platforms")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--app", help="Nebula executable, archive, app bundle, or package directory")
    mode.add_argument(
        "--compare",
        nargs="+",
        type=Path,
        metavar="REPORT",
        help="compare two or more existing reports",
    )
    parser.add_argument("--platform", default=default_platform_name())
    parser.add_argument("--output", type=Path, default=Path("report.json"))
    parser.add_argument("--artifacts", type=Path, help="directory for retained Nebula logs")
    parser.add_argument(
        "--golden-dir", type=Path, default=SCRIPT_DIR / "golden"
    )
    parser.add_argument("--startup-timeout", type=float, default=20.0)
    parser.add_argument("--update-golden", action="store_true")
    parser.add_argument("--no-platform-golden", action="store_true")
    return parser


def main() -> int:
    # Windows Python commonly inherits a legacy console code page. Diagnostic
    # terminal text is UTF-8 and may contain prompt glyphs or CJK; reporting a
    # product failure must never crash the runner with UnicodeEncodeError.
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="backslashreplace")

    parser = build_parser()
    args = parser.parse_args()
    if args.compare:
        if args.update_golden or args.no_platform_golden:
            parser.error("golden update options cannot be used with --compare")
        try:
            return compare_mode(args)
        except ConformanceError as error:
            parser.error(str(error))
    if args.update_golden and args.no_platform_golden:
        parser.error("--update-golden conflicts with --no-platform-golden")
    if args.startup_timeout <= 0:
        parser.error("--startup-timeout must be positive")
    return run_suite(args)


if __name__ == "__main__":
    raise SystemExit(main())
