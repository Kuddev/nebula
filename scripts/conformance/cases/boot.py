from __future__ import annotations

from conformance.harness import ConformanceContext, PROTOCOL_NAME, PROTOCOL_VERSION, path_shape, require


def run(ctx: ConformanceContext) -> dict[str, object]:
    snapshot = ctx.snapshot()
    ctx.refresh_targets(snapshot)
    windows = snapshot.get("windows") or []
    require(len(windows) == 1, f"isolated boot created {len(windows)} windows instead of one")
    window = windows[0]
    require(window.get("tabs"), "boot window has no tab")
    require(snapshot.get("protocol_version") == PROTOCOL_VERSION, "snapshot protocol mismatch")
    require(ctx.description.get("protocol") == PROTOCOL_NAME, "describe protocol mismatch")
    require(
        ctx.description.get("protocol_version") == PROTOCOL_VERSION,
        "describe protocol version mismatch",
    )
    required_methods = {
        "runtime.snapshot",
        "window.close",
        "tab.new",
        "tab.rename",
        "pane.split",
        "pane.resize",
        "pane.prompt",
        "pane.paste",
        "pane.read",
        "pane.procs",
        "pane.send_key",
    }
    capabilities = set(ctx.description.get("capabilities") or [])
    missing = sorted(required_methods - capabilities)
    require(not missing, f"runtime.describe omits required methods: {missing}")

    shell = ctx.detect_shell()
    require(
        shell in {"powershell", "pwsh", "cmd", "bash", "zsh", "fish", "nu", "sh", "dash"},
        f"unrecognized default shell: {shell}",
    )
    process_tree = ctx.api(
        "pane.procs", {"window_id": ctx.window_id, "pane_id": ctx.pane_id}
    )
    require(process_tree.get("processes"), "pane.procs returned an empty process tree")
    tab = ctx.tab_for_pane(snapshot, ctx.pane_id)
    pane = next(item for item in tab["panes"] if item["id"] == ctx.pane_id)
    cwd = pane.get("cwd", "")
    require(path_shape(cwd) != "relative", f"pane cwd is not absolute: {cwd!r}")

    return {
        "window_count": 1,
        "default_tab_present": True,
        "runtime_methods_present": True,
        "process_tree_available": True,
        "shell_category": shell,
        "cwd_shape": path_shape(cwd),
        "startup_ms": ctx.startup_ms,
    }
