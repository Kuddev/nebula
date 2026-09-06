import argparse
from pathlib import Path
import subprocess
import sys

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from scripts.architecture import budgets, dependencies


ROOT = Path(__file__).resolve().parents[1]


class Revision:
    def __init__(self, root: Path, reference: str):
        self.root = root
        result = subprocess.run(
            ["git", "rev-parse", "--verify", "--end-of-options", reference + "^{commit}"],
            cwd=root, capture_output=True, text=True, encoding="utf-8", check=True,
        )
        self.commit = result.stdout.strip()

    def read(self, path: str):
        result = subprocess.run(
            ["git", "--literal-pathspecs", "ls-tree", "--name-only", "-z", self.commit, "--", path],
            cwd=self.root, capture_output=True, check=True,
        )
        if not result.stdout:
            return None
        return subprocess.run(
            ["git", "show", f"{self.commit}:{path}"],
            cwd=self.root, capture_output=True, check=True,
        ).stdout


def run(root: Path, base: str | None = None, report: bool = False) -> int:
    budget = budgets.Budget.parse((root / budgets.BUDGET_PATH).read_text(encoding="utf-8"))
    revision = Revision(root, base) if base else None
    previous_source = revision.read(budgets.BUDGET_PATH) if revision else None
    previous = budgets.Budget.parse(previous_source.decode("utf-8")) if previous_source is not None else None
    errors, notices, counts = budgets.check(root, budget, previous, revision.read if revision else None)
    dependency_errors, members = dependencies.check(
        root, dependencies.load(root / dependencies.POLICY_PATH), budget.roots, set(counts)
    )
    errors.extend(dependency_errors)
    for member in members:
        if member not in budget.roots:
            errors.append(f"{member}: workspace member missing from line-budget roots")
    print(f"Architecture: {len(counts)} source files, {len(members)} crates, {len(budget.exceptions)} legacy exceptions")
    advisory = sorted(((name, count) for name, count in counts.items() if count > 800), key=lambda item: -item[1])
    print(f"Review guidance only: {len(advisory)} files exceed the 800-line soft target")
    if revision and previous is None:
        print("Initial policy adoption: no budget in base; review the initial debt inventory")
    for notice in notices:
        print(f"NOTICE: {notice}")
    if report:
        for name, count in advisory:
            print(f"REVIEW: {name}: {count} lines")
    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    return int(bool(errors))


def main() -> int:
    parser = argparse.ArgumentParser(description="Offline first-party size and dependency contracts (Python 3.11+)")
    parser.add_argument("--base", help="PR base commit; rejects new/increased debt and regrowth")
    parser.add_argument("--report", action="store_true", help="also list advisory-size files")
    options = parser.parse_args()
    try:
        return run(ROOT, options.base, options.report)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Architecture check could not complete: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
