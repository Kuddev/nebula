from __future__ import annotations

from conformance.harness import ConformanceContext, require


def _directions(layout: dict[str, object] | None) -> set[str]:
    if not layout or layout.get("type") != "split":
        return set()
    directions = {str(layout["direction"])}
    directions.update(_directions(layout.get("first")))
    directions.update(_directions(layout.get("second")))
    return directions


def run(ctx: ConformanceContext) -> dict[str, object]:
    original = ctx.pane_id
    right = ctx.api(
        "pane.split",
        {
            "window_id": ctx.window_id,
            "pane_id": original,
            "direction": "left_right",
        },
    )
    right_id = int(right["action"]["pane_id"])
    down_id = 0
    try:
        down = ctx.api(
            "pane.split",
            {
                "window_id": ctx.window_id,
                "pane_id": right_id,
                "direction": "top_bottom",
            },
        )
        down_id = int(down["action"]["pane_id"])
        snapshot = ctx.snapshot()
        tab = ctx.tab_for_pane(snapshot, down_id)
        require(len(tab["panes"]) == 3, f"split tab has {len(tab['panes'])} panes")
        require(tab.get("focused_pane_id") == down_id, "newest split did not receive focus")
        require(
            _directions(tab.get("layout")) == {"left_right", "top_bottom"},
            f"unexpected split tree: {tab.get('layout')!r}",
        )
        ctx.api("window.focus", {"window_id": ctx.window_id, "pane_id": original})
        focused = ctx.tab_for_pane(ctx.snapshot(), original)
        require(focused.get("focused_pane_id") == original, "explicit pane focus did not apply")
        return {
            "pane_count": 3,
            "nested_directions_present": True,
            "new_pane_focused": True,
            "explicit_focus_applied": True,
        }
    finally:
        if down_id:
            ctx.best_effort_api(
                "pane.close", {"window_id": ctx.window_id, "pane_id": down_id}
            )
        ctx.best_effort_api(
            "pane.close", {"window_id": ctx.window_id, "pane_id": right_id}
        )
        ctx.best_effort_api(
            "window.focus", {"window_id": ctx.window_id, "pane_id": original}
        )
