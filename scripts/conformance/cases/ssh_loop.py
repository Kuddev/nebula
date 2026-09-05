from __future__ import annotations

from conformance.harness import ConformanceContext, ConformanceError, SkipCase


def run(ctx: ConformanceContext) -> dict[str, object]:
    capabilities = set(ctx.description.get("capabilities") or [])
    if "ssh.open" not in capabilities:
        raise SkipCase("runtime_api_has_no_ssh_open_method")
    raise ConformanceError("ssh.open is now available; implement its conformance case before publishing")
