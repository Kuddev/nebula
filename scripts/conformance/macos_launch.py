#!/usr/bin/env python3
"""Exercise a private installed .app through LaunchServices, not its executable."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import tempfile
import time
from typing import BinaryIO

SCRIPTS = Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

from conformance.harness import ConformanceContext, ResolvedApp, RuntimeClient, require


def capture_screenshot(destination: Path, log: BinaryIO) -> str:
    try:
        capture = subprocess.run(["/usr/sbin/screencapture", "-x", str(destination)],
                                 stdout=log, stderr=subprocess.STDOUT, timeout=15, check=False)
        if capture.returncode == 0 and destination.is_file() and destination.stat().st_size > 0:
            return "captured"
    except (OSError, subprocess.TimeoutExpired):
        pass
    return "unavailable"


def owned_processes(executable: Path) -> dict[int, str]:
    listing = subprocess.check_output(["/bin/ps", "-axo", "pid=,comm="], text=True)
    result = {}
    for line in listing.splitlines():
        fields = line.strip().split(None, 1)
        if len(fields) == 2 and fields[1] == str(executable):
            process_id = int(fields[0])
            birth = subprocess.run(["/bin/ps", "-p", str(process_id), "-o", "lstart="],
                                   capture_output=True, text=True, check=False).stdout.strip()
            if birth:
                result[process_id] = birth
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = {"status": "failed", "launch_method": "launchservices",
              "commit": os.environ.get("GITHUB_SHA", "local"),
              "architecture": "aarch64" if platform.machine() == "arm64" else platform.machine(),
              "os_version": platform.mac_ver()[0], "quarantine_assessment": "not_tested"}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        require(sys.platform == "darwin", "LaunchServices requires native macOS")
        with tempfile.TemporaryDirectory(prefix="nebula-installed-smoke-") as temporary:
            root = Path(temporary).resolve()
            installed = root / "Applications" / "Nebula Terminal Preview.app"
            subprocess.run(["/usr/bin/ditto", str(args.app.resolve()), str(installed)], check=True)
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(installed)], check=True)
            app = ResolvedApp(installed)
            logs = args.output.parent / f"{args.output.stem}-artifacts"
            ctx = ConformanceContext(app, "macos", root / "config", root / "work", logs)
            ctx.prepare()
            launcher = None
            with (logs / "launchservices.log").open("wb") as log:
                try:
                    launcher = subprocess.Popen(
                        ["/usr/bin/open", "-n", "-W", "-a", str(installed),
                         "--env", f"NEBULA_CONFIG_DIR={ctx.config_dir}",
                         "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                         "--env", "LANG=", "--env", "LC_ALL=", "--env", "LC_CTYPE="],
                        cwd="/", stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                    )
                    deadline = time.monotonic() + 45
                    while time.monotonic() < deadline:
                        if ctx.port_file.is_file():
                            try:
                                client = RuntimeClient.from_port_file(ctx.port_file, timeout=2)
                                snapshot = client.request("runtime.snapshot")["result"]
                                require(snapshot["process_id"] in owned_processes(app.executable),
                                        "runtime endpoint is not owned by this installed copy")
                                ctx.client = client
                                ctx.description = client.request("runtime.describe")["result"]
                                ctx.refresh_targets(snapshot)
                                break
                            except (OSError, ValueError, KeyError, RuntimeError):
                                pass
                        require(launcher.poll() is None, "LaunchServices exited before the app was ready")
                        time.sleep(0.1)
                    else:
                        raise RuntimeError("installed application did not become ready in 45 seconds")
                    ctx.detect_shell()
                    ctx.prompt('printf "NEBULA_GUI_LOCALE=%s_END\\n" "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}"')
                    ctx.wait_for_line(re.compile(r"NEBULA_GUI_LOCALE=[^\r\n]*UTF-?8_END", re.IGNORECASE))
                    ctx.prompt('printf "NEBULA_GUI_CWD=%s_END\\n" "$PWD"')
                    ctx.wait_for_line(re.compile(r"NEBULA_GUI_CWD=" + re.escape(str(Path.home())) + r"_END"))
                    report["screenshot"] = capture_screenshot(logs / "installed-app.png", log)
                    for window in ctx.snapshot().get("windows", []):
                        ctx.api("window.close", {"window_id": window["id"]})
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline and owned_processes(app.executable):
                        time.sleep(0.1)
                    require(not owned_processes(app.executable), "installed application did not quit after closing its windows")
                    launcher.wait(timeout=5)
                    report.update(status="passed", utf8_locale=True, home_cwd=True)
                finally:
                    for process_id, birth in owned_processes(app.executable).items():
                        if owned_processes(app.executable).get(process_id) == birth:
                            os.kill(process_id, signal.SIGKILL)
                    if launcher is not None:
                        try:
                            launcher.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            launcher.kill()
                            launcher.wait(timeout=5)
                    app.close()
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
        print(report["error"], file=sys.stderr)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
