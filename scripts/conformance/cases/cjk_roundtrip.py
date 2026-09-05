from __future__ import annotations

from conformance.harness import ConformanceContext, require


def run(ctx: ConformanceContext) -> dict[str, object]:
    # This intentionally uses non-ASCII text: the case verifies the UTF-8
    # request -> PTY -> terminal-grid path. Native IME composition needs a GUI
    # automation layer and is not claimed here.
    text = "NEBULA_CJK_跨平台终端"
    ctx.prompt(text, submit=False)
    read = ctx.poll(
        # PowerShell's restored multi-line prompt can leave the input above the
        # bottom 20 rows of an otherwise empty grid. Scan the full viewport and
        # available history instead of mistaking trailing blank rows for loss.
        lambda: ctx.read(lines=160),
        lambda value: text in value["text"],
        "CJK text did not round-trip through the terminal grid",
    )
    require(text in read["text"], "CJK input was altered")
    ctx.send_key("c", control=True)
    return {"utf8_roundtrip": True, "coverage": "runtime_input_not_native_ime"}
