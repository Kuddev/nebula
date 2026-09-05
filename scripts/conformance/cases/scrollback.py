from __future__ import annotations

import re

from conformance.harness import ConformanceContext, require


def run(ctx: ConformanceContext) -> dict[str, object]:
    generated_lines = 600
    marker = "NEBULA_SCROLL_DONE"
    ctx.prompt(ctx.scrollback_command(generated_lines, marker))
    _, read = ctx.wait_for_line(re.compile(marker), timeout=20.0)
    read = ctx.poll(
        lambda: ctx.read(lines=40),
        lambda value: int(value["history_available"]) >= 500,
        "terminal history never retained 500 rows",
        timeout=5.0,
    )
    history = int(read["history_available"])
    require(history >= 500, f"only {history} scrollback rows are available")
    return {
        "generated_lines": generated_lines,
        "completion_marker_seen": True,
        "history_at_least_500": True,
        "history_available": history,
    }
