from __future__ import annotations

import json

from conformance.harness import ConformanceContext, ConformanceError, require


RESTORED_LABEL = "conformance-restored"


def run(ctx: ConformanceContext) -> dict[str, object]:
    ctx.ensure_single_tab()
    ctx.api(
        "tab.rename",
        {"window_id": ctx.window_id, "tab_index": 0, "name": RESTORED_LABEL},
    )
    ctx.api("tab.new", {"window_id": ctx.window_id})

    saved = ctx.wait_for_session(
        lambda value: len(value.get("tabs") or []) == 2
        and value["tabs"][0].get("custom_name") == RESTORED_LABEL
    )
    require(not saved.get("clean_exit", False), "autosave incorrectly marked a live session clean")

    restart_ms = ctx.restart()
    snapshot = ctx.poll(
        ctx.snapshot,
        lambda value: len((value.get("windows") or [{}])[0].get("tabs") or []) == 2,
        "cold restart did not restore two tabs",
        timeout=10.0,
    )
    ctx.refresh_targets(snapshot)
    labels = [tab.get("label") for tab in snapshot["windows"][0]["tabs"]]
    require(RESTORED_LABEL in labels, f"restored tab label is missing: {labels}")
    ctx.detect_shell()

    pane_ids = [
        int(pane["id"])
        for tab in snapshot["windows"][0]["tabs"]
        if tab.get("kind") == "shell"
        for pane in tab.get("panes") or []
    ]
    markers = {pane_id: f"NEBULA_RESTORED_PANE_{pane_id}_OK" for pane_id in pane_ids}
    observations: dict[int, dict[str, object]] = {}
    for pane_id, marker in markers.items():
        processes = ctx.api(
            "pane.procs",
            {"window_id": ctx.window_id, "pane_id": pane_id},
            allow_error=True,
        )
        observations[pane_id] = {"processes": processes}
        try:
            # Keep the complete marker out of PowerShell's command echo. Only
            # executed output may satisfy the observation below.
            ctx.prompt(ctx.marker_command(marker.removesuffix("OK"), "OK"), pane_id)
        except Exception as error:
            observations[pane_id]["prompt_error"] = str(error)

    def read_restored_panes() -> dict[int, dict[str, object]]:
        for pane_id in pane_ids:
            try:
                observations[pane_id]["read"] = ctx.read(pane_id, lines=40)
            except Exception as error:
                observations[pane_id]["read_error"] = str(error)
        return observations

    try:
        ctx.poll(
            read_restored_panes,
            lambda value: all(
                markers[pane_id] in str(value[pane_id].get("read", {}).get("text", ""))
                for pane_id in pane_ids
            ),
            "restored panes did not answer runtime input",
            timeout=10.0,
        )
    except Exception as error:
        raise ConformanceError(
            f"{error}; restored pane diagnostics: "
            f"{json.dumps(observations, ensure_ascii=False, sort_keys=True)}"
        ) from error

    return {
        "autosave_converged": True,
        "unclean_exit_restored": True,
        "restored_tab_count": 2,
        "restored_custom_label": True,
        "restored_panes_live": True,
        "restart_ms": restart_ms,
    }
