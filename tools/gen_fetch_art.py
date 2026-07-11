#!/usr/bin/env python3
"""Procedurally bake the Nebula welcome art into nebula_fetch_art.rs.

v3: SATURN, per the user's reference image (2026-07-04):
- planet ball in three latitude bands: `o/0` indigo top, `*` cyan belt,
  `~` magenta south — dense fill, hard round silhouette
- a golden `=` ring tilted across the ball: the front arc passes IN FRONT
  of the planet (overwrites it), the back arc hides behind the ball and
  only shows outside the silhouette
- sparse starfield around it: `+` `.` in white / gold / violet
- 62 cols wide (welcome.rs info_col = 65 depends on this), 19 rows,
  cell aspect ~2.6:1 corrected via ASPECT

Regenerate: `python tools/gen_fetch_art.py` (stdout shows a true-color
ANSI preview; D:\\SHARE\\nebula_fetch_preview.html the same in a browser).
"""

import math

OUT_RS = r"D:\temp_build\nebula\nebula_app\src\window_context\nebula_fetch_art.rs"
OUT_HTML = r"D:\SHARE\nebula_fetch_preview.html"

W, H = 62, 19
ASPECT = 2.6            # terminal cell height/width ratio

# --- planet ---
CX, CY = 27.0, 9.4      # ball center (cells); slightly left so the ring can sweep right
R = 16.0                # ball radius in x-units
BELT_TOP = -0.18        # latitude (normalized dy) where the cyan belt starts
BELT_BOT = 0.30         # where the magenta south begins

# --- ring ---
TILT = math.radians(14.0)   # ring plane tilt: lower-left -> upper-right
RING_A = 31.0           # ring ellipse semi-major (x-units)
RING_B = 8.6            # semi-minor before aspect correction
RING_HALF = 0.20        # band half-thickness in ellipse-e units (e in [1-h, 1+h])
RING_EDGE = 0.12        # |e-1| beyond this renders `-` instead of `=`

# --- starfield ---
STAR_P = 0.022          # per-cell probability outside planet/ring

# --- palette (sampled from the reference) ---
TERMBG = (13, 15, 24)
COL_TOP0 = (126, 138, 232)   # indigo `0/o` north, upper rows
COL_TOP1 = (104, 116, 210)   # ... slightly darker toward the belt
COL_BELT = (86, 202, 218)    # cyan `*` belt
COL_SOUTH = (198, 112, 214)  # magenta `~` south
COL_RING = (214, 170, 78)    # golden `=`
STAR_COLS = [(212, 216, 230), (214, 170, 78), (150, 128, 224)]
STAR_CHARS = ["+", ".", "."]


def hash01(*args):
    """Deterministic per-cell noise in [0,1) — bake must be reproducible."""
    h = 2166136261
    for a in args:
        h ^= int(a) & 0xFFFFFFFF
        h = (h * 16777619) & 0xFFFFFFFF
    return ((h >> 8) & 0xFFFF) / 65536.0


def planet_at(x, y):
    """Inside the ball? -> (char, color) by latitude band."""
    dx = x - CX
    dy = (y - CY) * ASPECT
    if dx * dx + dy * dy > R * R:
        return None
    lat = dy / R  # -1 (north pole) .. 1 (south pole)
    if lat < BELT_TOP:
        # North: mixed o/0 rows, indigo, slightly darker toward the belt.
        t = (lat + 1.0) / max(1e-6, BELT_TOP + 1.0)  # 0 at pole -> 1 at belt
        col = tuple(round(COL_TOP0[i] + (COL_TOP1[i] - COL_TOP0[i]) * t) for i in range(3))
        ch = "0" if hash01(x * 31, y * 57) < 0.55 else "o"
        return ch, col
    if lat < BELT_BOT:
        return "*", COL_BELT
    return "~", COL_SOUTH


def ring_at(x, y):
    """On the ring band? -> (in_front, char, color). Ellipse in tilted plane."""
    dx = x - CX
    dy = (y - CY) * ASPECT
    # Rotate into the ring plane (screen y points down, tilt is CCW).
    u = dx * math.cos(TILT) - dy * math.sin(TILT)
    v = dx * math.sin(TILT) + dy * math.cos(TILT)
    e = math.hypot(u / RING_A, v / RING_B)
    if abs(e - 1.0) > RING_HALF:
        return None
    # Band core reads `=`, the thin inner/outer fringes read `-`.
    ch = "=" if abs(e - 1.0) <= RING_EDGE else "-"
    # v > 0 = the arc dipping below the ring plane on screen -> in front.
    return v > 0.0, ch, COL_RING


grid = [[None] * W for _ in range(H)]  # (char, (r,g,b)) or None
for y in range(H):
    for x in range(W):
        planet = planet_at(x, y)
        ring = ring_at(x, y)

        if ring is not None:
            in_front, ring_ch, ring_col = ring
            if in_front or planet is None:
                # Front arc always wins; back arc only outside the ball.
                grid[y][x] = (ring_ch, ring_col)
                continue
        if planet is not None:
            grid[y][x] = planet
            continue
        # Starfield. Spatial-hash mix of both coords per word, otherwise
        # nearby rows correlate and stars line up in fake columns.
        rnd = hash01(x * 73856093 ^ y * 19349663, y * 83492791 ^ x * 2971215073)
        if rnd < STAR_P:
            pick = int(hash01(x * 419 + y, y * 631 + x) * len(STAR_COLS)) % len(STAR_COLS)
            grid[y][x] = (STAR_CHARS[pick], STAR_COLS[pick])

# --- emit ---
rs_lines, html_lines, filled = [], [], 0
for y in range(H):
    row_rs, row_html = [], []
    for x in range(W):
        cell = grid[y][x]
        if cell is None:
            row_rs.append(" ")
            row_html.append(" ")
        else:
            filled += 1
            ch, (r, g, b) = cell
            row_rs.append(f"\\x1b[38;2;{r};{g};{b}m{ch}")
            row_html.append(f'<span style="color:rgb({r},{g},{b})">{ch}</span>')
    line = "".join(row_rs).rstrip()
    if line:
        line += "\\x1b[0m"
    rs_lines.append(line)
    html_lines.append("".join(row_html).rstrip())

header = (
    "//! Nebula welcome-screen fetch art: Saturn (user reference 2026-07-04).\n"
    "//! Latitude-banded ball (o/0 indigo, * cyan, ~ magenta), golden tilted\n"
    "//! `=` ring with front/back occlusion, sparse starfield.\n"
    "//! Regenerate with `python tools/gen_fetch_art.py`.\n\n"
    "#[cfg(windows)]\n"
    "pub const NEBULA_STAR_ART: &[&str] = &[\n"
)
body = "".join(f'    "{l}",\n' for l in rs_lines)
with open(OUT_RS, "w", encoding="utf-8") as f:
    f.write(header + body + "];\n")

with open(OUT_HTML, "w", encoding="utf-8") as f:
    f.write(
        "<html><body style='background:#0d0f18'>"
        "<pre style='font:14px/0.9 Consolas,monospace'>"
        + "\n".join(html_lines)
        + "</pre></body></html>"
    )

print(f"baked {W}x{H}, filled {filled} ({filled * 100 // (W * H)}%)")
# True-color ANSI preview straight to the terminal.
for y in range(H):
    out = []
    for x in range(W):
        cell = grid[y][x]
        if cell is None:
            out.append(" ")
        else:
            ch, (r, g, b) = cell
            out.append(f"\x1b[38;2;{r};{g};{b}m{ch}")
    print("".join(out).rstrip() + "\x1b[0m")
