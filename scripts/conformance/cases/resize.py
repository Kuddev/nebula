from __future__ import annotations

import json
import os

from conformance.harness import ConformanceContext, require


def _linux_terminal_size(pid: int) -> dict[str, int]:
    import fcntl
    import struct
    import termios

    # Read the shell's actual PTY geometry without relying on COLUMNS or the
    # command-substitution stdout used by tput. O_NOCTTY preserves ownership.
    fd = os.open(f"/proc/{pid}/fd/0", os.O_RDONLY | os.O_NOCTTY | os.O_CLOEXEC)
    try:
        packed = fcntl.ioctl(fd, termios.TIOCGWINSZ, bytes(8))
        rows, columns, _, _ = struct.unpack("HHHH", packed)
        return {"rows": rows, "columns": columns}
    finally:
        os.close(fd)


def _write_failure_diagnostics(
    ctx: ConformanceContext,
    source: int,
    sibling: int,
    measurements: dict[str, int],
    error: Exception,
) -> None:
    details: dict[str, object] = {
        "error": str(error),
        "shell": ctx.shell,
        "measurements": measurements,
        "panes": {},
    }
    try:
        details["snapshot"] = ctx.api("runtime.snapshot", timeout=2.0)
    except Exception as diagnostic_error:
        details["snapshot_error"] = str(diagnostic_error)
    panes: dict[str, object] = {}
    for pane_id in (source, sibling):
        params = {"window_id": ctx.window_id, "pane_id": pane_id}
        pane: dict[str, object] = {}
        try:
            pane["read"] = ctx.api("pane.read", {**params, "lines": 40}, timeout=2.0)
        except Exception as diagnostic_error:
            pane["read_error"] = str(diagnostic_error)
        if ctx.platform_family == "linux":
            try:
                processes = ctx.api("pane.procs", params, timeout=2.0)
                pane["kernel_terminal_size"] = _linux_terminal_size(int(processes["root_pid"]))
            except Exception as diagnostic_error:
                pane["kernel_terminal_size_error"] = str(diagnostic_error)
        panes[str(pane_id)] = pane
    details["panes"] = panes
    (ctx.artifact_dir / "resize-failure.json").write_text(
        json.dumps(details, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )


def run(ctx: ConformanceContext) -> dict[str, object]:
    source = ctx.pane_id
    split = ctx.api(
        "pane.split",
        {
            "window_id": ctx.window_id,
            "pane_id": source,
            "direction": "left_right",
        },
    )
    sibling = int(split["action"]["pane_id"])
    measurements: dict[str, int] = {}
    try:
        source_before = ctx.measure_columns(source)
        measurements["source_columns_before"] = source_before
        sibling_before = ctx.measure_columns(sibling)
        measurements["sibling_columns_before"] = sibling_before
        resized = ctx.api(
            "pane.resize",
            {"window_id": ctx.window_id, "pane_id": source, "ratio": 0.70},
        )
        ratio = float(resized["action"]["ratio"])
        source_after = ctx.measure_columns(source)
        measurements["source_columns_after"] = source_after
        sibling_after = ctx.measure_columns(sibling)
        measurements["sibling_columns_after"] = sibling_after
        require(abs(ratio - 0.70) < 0.001, f"runtime reported wrong split ratio: {ratio}")
        require(
            source_after > source_before,
            f"70% pane did not grow: {source_before} -> {source_after}",
        )
        require(
            sibling_after < sibling_before,
            f"30% pane did not shrink: {sibling_before} -> {sibling_after}",
        )
        require(
            abs((source_before + sibling_before) - (source_after + sibling_after)) <= 4,
            "PTY column total changed beyond divider rounding tolerance",
        )
        return {
            "layout_ratio_applied": True,
            "pty_columns_changed": True,
            "source_columns_before": source_before,
            "source_columns_after": source_after,
            "sibling_columns_before": sibling_before,
            "sibling_columns_after": sibling_after,
        }
    except Exception as error:
        # Capture only after a strict assertion fails. Extra observations must
        # not turn into an implicit wait that makes a resize race disappear.
        try:
            _write_failure_diagnostics(ctx, source, sibling, measurements, error)
        except Exception as diagnostic_error:
            print(f"[resize] could not retain diagnostics: {diagnostic_error}", flush=True)
        raise
    finally:
        ctx.best_effort_api("pane.close", {"window_id": ctx.window_id, "pane_id": sibling})
        ctx.best_effort_api("window.focus", {"window_id": ctx.window_id, "pane_id": source})
