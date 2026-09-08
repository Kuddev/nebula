"""Select complete affected crate suites; run with `python -m scripts.ci`.

Dependency interpretation is shared with the architecture checker. Unknown inputs
or unavailable diff history select the whole workspace, never an empty test run.
"""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import time

from scripts.architecture.dependencies import dependency_entries, load


ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = (
    ("ubuntu-24.04", "linux-x64"),
    ("windows-2022", "windows-x64"),
    ("macos-15", "macos-arm64"),
    ("macos-15-intel", "macos-x64"),
)
UI_PACKAGES = {"nebula", "nebula-gpui"}


def workspace_graph(root):
    workspace = load(root / "Cargo.toml")["workspace"]
    members = workspace["members"]
    if workspace.get("exclude") or any(set(member) & set("*?[") for member in members):
        raise ValueError("CI requires the explicit workspace members used by architecture checks")
    manifests = {member: load(root / member / "Cargo.toml") for member in members}
    names = {member: manifest["package"]["name"] for member, manifest in manifests.items()}
    paths = {(root / member).resolve(): names[member] for member in members}
    consumers = {name: set() for name in names.values()}
    for member, manifest in manifests.items():
        for _, _, spec, inherited in dependency_entries(manifest, workspace):
            if "path" not in spec:
                continue
            directory = root if inherited else root / member
            dependency = paths.get((directory / spec["path"]).resolve())
            if dependency is None:
                raise ValueError(f"unclassified local dependency in {member}: {spec['path']}")
            # Include optional, build, dev and every target's edges. A change in a
            # derive or a Windows-only dependency also retests its consumers.
            consumers[dependency].add(names[member])
    return names, consumers


def affected_packages(paths, names, consumers):
    affected = set()
    for path in paths:
        file = PurePosixPath(path)
        if file.name in {"Cargo.toml", "Cargo.lock"}:
            return set(names.values()), f"dependency manifest changed: {path}"
        member = next((member for member in names if path.startswith(member + "/")), None)
        if member:
            affected.add(names[member])
        elif file.suffix == ".md" or path.startswith("docs/screenshots/"):
            continue
        else:
            return set(names.values()), f"shared or unclassified input changed: {path}"
    pending = list(affected)
    while pending:
        for consumer in consumers[pending.pop()]:
            if consumer not in affected:
                affected.add(consumer)
                pending.append(consumer)
    return affected, "changed crates and all transitive consumers"


def changed_paths(root, base, head, event):
    if not base or not base.strip("0"):
        raise ValueError("no comparison base (initial push or manual/full run)")
    # Resolve before diffing; never allow a supplied ref to become a Git option.
    def commit(ref):
        return subprocess.check_output(
            ["git", "rev-parse", "--verify", "--end-of-options", ref + "^{commit}"],
            cwd=root, text=True, encoding="utf-8", stderr=subprocess.PIPE,
        ).strip()

    base, head = commit(base), commit(head)
    if event == "pull_request":
        base = subprocess.check_output(
            ["git", "merge-base", base, head], cwd=root, text=True, encoding="utf-8",
        ).strip()
    # No rename collapsing: both old and new owners must be tested after a move.
    output = subprocess.check_output(
        ["git", "diff", "--name-only", "--no-renames", "-z", base, head, "--"], cwd=root,
    )
    return output.decode("utf-8").rstrip("\0").split("\0") if output else []


def plan(root, base, head, event, full=False, windows_only=False):
    names, consumers = workspace_graph(root)
    if full or event == "schedule":
        selected, reason = set(names.values()), "full workspace requested"
    else:
        try:
            paths = changed_paths(root, base, head, event)
        except (ValueError, subprocess.CalledProcessError, UnicodeError) as error:
            selected, reason = set(names.values()), f"full fallback: {error}"
        else:
            selected, reason = affected_packages(paths, names, consumers)
    matrix = []
    for runner, platform in PLATFORMS:
        if windows_only and platform != "windows-x64":
            continue
        for suite, packages in (
            ("core", selected - UI_PACKAGES), ("product", selected & UI_PACKAGES),
        ):
            # Core jobs also run the native Python contracts, including on docs PRs.
            if suite == "core" or packages:
                matrix.append({"runner": runner, "platform": platform, "suite": suite,
                               "packages": sorted(packages)})
    return {"include": matrix}, reason


def cargo_commands(packages, root=ROOT):
    names, _ = workspace_graph(root)
    if not isinstance(packages, list) or any(package not in names.values() for package in packages):
        raise ValueError("test selection contains an unknown workspace package")
    if not packages:
        return []
    options = ["--locked", "--config", ".github/ci-profile.toml", "--profile", "ci"]
    commands = []
    if "nebula" in packages:
        # test-support changes the dependency feature graph. Also compile the
        # actual product configuration, including its native accessibility code.
        commands.append(["cargo", "check", *options, "-p", "nebula", "--bin", "pebrel",
                         "--features", "gpui-shell"])
    test = ["cargo", "test", *options]
    for package in sorted(packages):
        test += ["-p", package]
    if "nebula" in packages:
        test += ["--features", "nebula/gpui-test-support"]
    # All tests of each selected crate, including doctests and GPUI dialogs, run
    # in one invocation. No substring filters or copied test implementations.
    commands.append(test)
    return commands


def append_summary(message):
    print(message, flush=True)
    if destination := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(destination, "a", encoding="utf-8") as summary:
            summary.write(message + "\n")


def run_tests(packages):
    for command in cargo_commands(packages):
        started = time.monotonic()
        result = subprocess.run(command, cwd=ROOT)
        append_summary(f"`{' '.join(command)}`: {time.monotonic() - started:.1f}s "
                       f"(exit {result.returncode})\n")
        if result.returncode:
            return result.returncode
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    select = commands.add_parser("plan")
    select.add_argument("--base", default=os.environ.get("CI_BASE", ""))
    select.add_argument("--head", default="HEAD")
    select.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", "push"))
    select.add_argument("--full", action="store_true", default=os.environ.get("CI_FULL") == "true")
    test = commands.add_parser("test")
    test.add_argument("--packages", default=os.environ.get("CI_PACKAGES", "[]"))
    args = parser.parse_args()
    if args.command == "test":
        return run_tests(json.loads(args.packages))
    matrix, reason = plan(ROOT, args.base, args.head, args.event, args.full,
                          windows_only=os.environ.get("CI_WINDOWS_ONLY") == "true")
    append_summary(f"Test selection: {reason}\n\n```json\n{json.dumps(matrix, indent=2)}\n```\n")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as handle:
            handle.write("matrix=" + json.dumps(matrix, separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
