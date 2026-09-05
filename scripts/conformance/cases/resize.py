from __future__ import annotations

from conformance.harness import ConformanceContext, require


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
    try:
        source_before = ctx.measure_columns(source)
        sibling_before = ctx.measure_columns(sibling)
        resized = ctx.api(
            "pane.resize",
            {"window_id": ctx.window_id, "pane_id": source, "ratio": 0.70},
        )
        ratio = float(resized["action"]["ratio"])
        source_after = ctx.measure_columns(source)
        sibling_after = ctx.measure_columns(sibling)
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
    finally:
        ctx.best_effort_api("pane.close", {"window_id": ctx.window_id, "pane_id": sibling})
        ctx.best_effort_api("window.focus", {"window_id": ctx.window_id, "pane_id": source})
