#!/usr/bin/env python3
"""平台 cfg 预算：`nebula_app/src/platform/` 之外的平台 `cfg` 只能减不能增。

三端一致性的编译门。平台差异应收进 `platform/`（能力表 + 每平台实现），
业务代码写 `if CAPABILITIES.xxx` 而不是 `#[cfg(windows)]`。基线数字写在
`scripts/platform_cfg_budget.txt`：超预算失败；低于预算时提示把数字收紧，
和 guardrails 的其他预算一样，报「少于预算」就照报告改数字。

用法：python3 scripts/check_platform_cfg.py [--update]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "nebula_app" / "src"
BUDGET_FILE = ROOT / "scripts" / "platform_cfg_budget.txt"

# 只数平台维度的 cfg：windows / unix / target_os。feature 维度（legacy-shell、
# gpui-shell）不是本脚本的对象。`cfg!(...)`、`#[cfg(...)]`、`cfg_attr(...)`、
# `all(...)`/`any(...)`/`not(...)` 里嵌套的都算一次。
PLATFORM_CFG = re.compile(r'\b(?:windows\b|unix\b|target_os\s*=\s*"[a-z0-9_]+")')
CFG_CONTEXT = re.compile(r"cfg(?:_attr)?!?\s*\(([^)]*(?:\([^)]*\)[^)]*)*)\)")

# 旧壳（legacy-shell）代码不在预算内：它只在显式 feature 下编译，也不是三端
# 发布对象。目录清单与 main.rs 的 `#[cfg(feature = "legacy-shell")] mod` 对应。
EXEMPT_DIRS = {"platform", "display", "renderer", "window_context", "input", "product_ui"}
EXEMPT_FILES = {"event.rs", "polling"}


def count_platform_cfgs(path: Path) -> int:
    text = path.read_text(encoding="utf-8", errors="replace")
    return sum(len(PLATFORM_CFG.findall(match.group(1))) for match in CFG_CONTEXT.finditer(text))


def is_exempt(path: Path) -> bool:
    relative = path.relative_to(SOURCE_ROOT).parts
    return bool(set(relative[:-1]) & EXEMPT_DIRS) or relative[0] in EXEMPT_FILES


def main(argv: list[str]) -> int:
    counts = {
        path.relative_to(ROOT).as_posix(): count_platform_cfgs(path)
        for path in sorted(SOURCE_ROOT.rglob("*.rs"))
        if not is_exempt(path)
    }
    total = sum(counts.values())
    if "--update" in argv:
        BUDGET_FILE.write_text(f"{total}\n", encoding="utf-8")
        print(f"platform cfg budget set to {total}")
        return 0
    budget = int(BUDGET_FILE.read_text(encoding="utf-8").strip())
    print(f"platform cfg outside platform/: {total} (budget {budget})")
    if total > budget:
        worst = sorted(counts.items(), key=lambda item: -item[1])[:10]
        print("budget exceeded; heaviest files:", file=sys.stderr)
        for name, count in worst:
            print(f"  {count:4d}  {name}", file=sys.stderr)
        print(
            "move the platform branch into nebula_app/src/platform/ (or a CAPABILITIES check)",
            file=sys.stderr,
        )
        return 1
    if total < budget:
        print(f"::notice::platform cfg count dropped to {total}; run with --update to tighten the budget")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
