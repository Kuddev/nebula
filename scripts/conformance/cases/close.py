from __future__ import annotations

from conformance.harness import ConformanceContext


def run(ctx: ConformanceContext) -> dict[str, object]:
    ctx.ensure_single_tab()
    exit_ms = ctx.close_and_wait(timeout=5.0)
    return {"exited_within_5s": True, "exit_ms": exit_ms}
