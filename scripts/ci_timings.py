"""Report workflow wall time (including queue/setup) and per-job cache/build time."""

from datetime import datetime, timezone
import argparse
import json
from pathlib import Path

from scripts.ci import append_summary


def seconds(start, end):
    return (datetime.fromisoformat(end.replace("Z", "+00:00"))
            - datetime.fromisoformat(start.replace("Z", "+00:00"))).total_seconds()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-json", type=Path, required=True)
    parser.add_argument("--jobs-json", type=Path, required=True)
    parser.add_argument("--label", default="All-platform packages and tests")
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    run = json.loads(args.run_json.read_text(encoding="utf-8"))
    jobs = json.loads(args.jobs_json.read_text(encoding="utf-8"))["jobs"]
    # run_started_at resets on reruns; created_at would count time between attempts.
    elapsed = seconds(run["run_started_at"], datetime.now(timezone.utc).isoformat())
    rows = [f"{args.label}: **{elapsed / 60:.2f} min**; objective: **<10 min**.",
            "Includes queue/setup and completed cache uploads; this reporting job is still running.",
            "", "| Job | Result | Total | Cache restore/save | Cargo builds/tests |",
            "| --- | --- | ---: | ---: | ---: |"]
    for job in jobs:
        if not job.get("completed_at"):
            continue
        total = seconds(job["started_at"], job["completed_at"])
        cache = cargo = 0.0
        for step in job.get("steps", []):
            if not step.get("started_at") or not step.get("completed_at"):
                continue
            duration = seconds(step["started_at"], step["completed_at"])
            if "rust-cache" in step["name"]:
                cache += duration
            if (step["name"] == "Compile the product and run complete affected crate suites"
                    or step["name"].startswith("Build GPUI")):
                cargo += duration
        rows.append(f"| {job['name']} | {job['conclusion']} | {total:.0f}s | {cache:.0f}s | {cargo:.0f}s |")
    append_summary("\n".join(rows) + "\n")
    if args.artifacts:
        rows = ["| Package | MiB | Bytes |", "| --- | ---: | ---: |"]
        for artifact in sorted(args.artifacts.iterdir()):
            if artifact.is_file() and artifact.suffix not in {".md", ".json"} and artifact.name != "SHA256SUMS":
                size = artifact.stat().st_size
                rows.append(f"| {artifact.name} | {size / 1024**2:.2f} | {size} |")
        append_summary("\n".join(rows) + "\n")
    if elapsed >= 600:
        print("::warning::All-platform feedback exceeded 10 minutes. Inspect cache misses, "
              "compilation and runner queue time in the timing summary.")


if __name__ == "__main__":
    main()
