from __future__ import annotations

from conformance.harness import ApiFailure, ConformanceContext, require


def run(ctx: ConformanceContext) -> dict[str, object]:
    first = "NEBULA_PASTE_ALPHA"
    last = "NEBULA_PASTE_OMEGA"
    try:
        ctx.paste(f"{first}\n{last}", submit=False)
    except ApiFailure as error:
        require(
            error.error.get("code") == "unsafe_input_mode",
            f"pane.paste failed outside its documented safety guard: {error}",
        )
        return {
            "safe_delivery_or_refusal": True,
            "outcome": "unsafe_input_mode",
            "multiline_roundtrip": False,
            "implicit_submission_prevented": True,
            "coverage": "runtime_paste_safety",
        }

    read = ctx.poll(
        lambda: ctx.read(lines=40),
        lambda value: first in value["text"] and last in value["text"],
        "multi-line bracketed paste did not reach the terminal grid",
    )
    require(first in read["text"] and last in read["text"], "paste text was not preserved")
    ctx.send_key("c", control=True)
    return {
        "safe_delivery_or_refusal": True,
        "outcome": "delivered",
        "multiline_roundtrip": True,
        "implicit_submission_prevented": True,
        "coverage": "runtime_paste_safety",
    }
