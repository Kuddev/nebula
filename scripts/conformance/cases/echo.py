from __future__ import annotations

import re

from conformance.harness import ConformanceContext


def run(ctx: ConformanceContext) -> dict[str, object]:
    marker = "NEBULA_ECHO_OK"
    ctx.prompt(ctx.marker_command("NEBULA_ECHO_", "OK"))
    ctx.wait_for_line(re.compile(marker))
    return {"tail_contains_marker": True}
