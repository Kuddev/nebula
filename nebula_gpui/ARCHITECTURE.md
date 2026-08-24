# Nebula GPUI Foundation

`nebula_gpui` is an isolated GPUI component gallery. It remains a validation
surface, but it no longer owns the product terminal or workspace runtime.
Those implementations moved into `nebula_app/src/gpui_shell/` in P3 and run as
the product main window through `nebula.exe --gpui`.

It is a laboratory, not a home: the single maintained product is
`nebula_app` (`nebula.exe`), and modules proven here are integrated into
`nebula_app` rather than accumulating product code in this crate. The
staged plan lives in the repo-root `ROADMAP.md`.

## Adoption & Rollback Discipline

GPUI adoption is direct: once a surface is judged genuinely better served by
GPUI components than by the hand-drawn UI, the GPUI implementation becomes
that surface's code path — no runtime switches, no long-lived feature flags
keeping two UI paths alive. Parallel paths rot; the component library's value
is precisely that we stop maintaining hand-drawn widgets.

Rollback is provided by code retention and git history instead:

- The winit shell and the existing hand-drawn UI modules stay in the tree and
  stay buildable until the GPUI shell has passed the three gates (IME,
  performance baseline, CJK clarity) plus a stabilization period. Deleting
  them is the final step of the migration, not a side effect of it.
- Shared logic (terminal engine, render contract, config, sessions) lives in
  shared crates, so moving between shells never loses behavior.
- Each migration step is an isolated commit; if GPUI develops serious bugs,
  rollback = reverting commits. The old code is still there and still
  compiles.

The `gpui-shell` cargo feature currently gates the product GPUI path as well as
the older dual-runtime spike. It is still scaffolding: after the product-form
gates pass and GPUI becomes the default entry point, the feature gate and
`NEBULA_GPUI_SHELL` spike are removed rather than becoming rollback mechanisms.

## Direct Component Use

Product views should import approved upstream components from
`gpui_shell::prelude`:

```rust
use crate::gpui_shell::prelude::*;

let action = Button::new("save").primary().label("保存");
let input = Input::new(&input_state);
```

`prelude` deliberately re-exports upstream types without changing their
behavior. Do not create one-to-one wrappers such as `NebulaButton` or
`NebulaInput`. A local component is justified only when it owns a Nebula
business contract, such as a terminal pane, an atomic-save editor workflow, or
a session-aware dock panel.

## Ownership Boundaries

- `gpui_shell::run_shell` owns the product GPUI/component-library initialization.
- `gpui_shell::theme` owns Nebula visual tokens, fonts, density, and global defaults.
- `gpui_shell::prelude` is the approved direct-component import surface for product views.
- View code may use GPUI entities and window APIs.
- Terminal, SSH, SFTP, configuration, history, and other core crates must not
  expose `gpui::Entity`, `gpui::Window`, or GPUI actions in their public APIs.

This preserves direct UI composition while keeping the product core portable
to another UI library. UI adapters translate core state and commands at the
application boundary.

## Dependency Policy

The v1.16.1 migration baseline is pinned by both package version and immutable
Git revision:

```toml
gpui = "=0.2.2"
gpui_platform = "=0.1.0"
gpui-component = "=0.5.2"
gpui-component-assets = "=0.5.1"
```

- `gpui` and `gpui_platform` come from `zed-industries/zed` commit
  `eb8e1c8b5502b7007465fbbc465f4a736fa39210` (the official `v1.16.1` tag).
- `gpui-component` and its assets come from `Kuddev/gpui-component` commit
  `4ee9f274e990d6228e4f276f0a1e48f62f6a2048`. This baseline contains the
  upstream snapshot plus the Zed revision pin; it intentionally excludes
  Nebula product patches.
- The `nebula-v1.16.1-base` branch and tag in both Kuddev forks are audit and
  recovery anchors. Cargo dependencies always use the full commit SHA, never
  those movable names.

Cargo source identity includes the repository URL. All Zed crates therefore
use the official URL, including transitive dependencies from gpui-component;
using the Kuddev Zed mirror for only part of the graph would create two
incompatible copies of GPUI even when both URLs point to the same commit.

Do not use wildcard versions, Git `main`, or branch-only pins. Verify every
dependency revision change with:

```powershell
cargo check -p nebula --features gpui-shell --locked --offline
cargo tree -p nebula --features gpui-shell -i gpui --locked --offline
```

GPUI upgrades happen in a dedicated branch with component acceptance testing;
they are not bundled with feature work. Each required Nebula component patch
must be restored in its own commit, advance the exact component revision, name
the blocked behavior, and define its upstream or removal condition. The fixed
baseline branch and tag must never move.

## Workspace Boundary

`nebula_gpui` is a member of the root Cargo workspace (merged 2026-08-12).
The former blocker — two packages holding `links = "fontconfig"` — was
resolved by aligning both sides on `yeslogic-fontconfig-sys ^6.0`:

- `gpui 0.2.2` → `zed-font-kit 0.14.1-zed` → `yeslogic-fontconfig-sys ^6.0`
- `nebula` → `crossfont 0.9.0` (upstream bumped to `^6.0` in 0.9.0)
- The winit Wayland decoration feature switched from the crossfont flavor to
  `wayland-csd-adwaita` (ab_glyph), so `sctk-adwaita 0.10` no longer drags
  `crossfont ^0.8` (and with it `yeslogic ^5`) back into the graph. This is
  Linux-only decoration text; Windows/macOS builds are unaffected.

One lockfile now holds `gpui 0.2.2`, `crossfont 0.9.0`, and a single
`yeslogic-fontconfig-sys 6.0.1`, and both binaries compile from the merged
workspace. `crossfont` still leaves the graph entirely when the legacy
OpenGL renderer is retired; the decoration switch was the first consumer
removed on that path.

## Terminal Vertical Slice

`nebula_app/src/gpui_shell/terminal/` hosts the product terminal integration and proves the hardest
migration claim: `nebula_terminal` (PTY, VT parser, grid, selection) renders
inside GPUI as a custom `Element` without forking either side.

- `session.rs` — owns the ConPTY + `EventLoop` wiring. Terminal events cross
  into GPUI through one futures channel; the PTY I/O thread never touches
  GPUI types.
- `element.rs` — the paint hot path. One `FairMutex` lock per frame builds a
  plain-data snapshot (background runs, styled text segments, cursor,
  selection), then paints with `paint_quad` + `shape_line(force_width)` so
  CJK wide cells stay grid-aligned.
- `view.rs` — focus, keyboard, mouse selection, wheel scrolling, clipboard,
  and IME via `EntityInputHandler` (marked text drawn at the cursor cell,
  candidate window anchored through `bounds_for_range`).
- `keymap.rs` — encodes only control/modified keys; printable text (including
  IME commits) flows through the text-input path so nothing is sent twice.
- `colors.rs` — resolves vte colors against `Term::colors()` overrides, then
  a default palette.

The boundary rule stays symmetric: no `nebula_terminal` type escapes
`src/terminal/`, and no GPUI type enters `nebula_terminal`.

## Multi-Tab Terminal Workspace

`nebula_app/src/gpui_shell/workspace.rs` is the product shell: a custom
`TitleBar`, sidebar and center content surface owning terminal and settings
tabs. The workspace owns tab semantics only (title, close, focus forwarding
and lifecycle); terminal behavior stays in `TerminalView`.

Lifecycle rules proven by manual acceptance:

- New tab: title-bar button or `ctrl-shift-t`; the new tab activates and
  focuses its terminal.
- Close: sidebar close action, `ctrl-shift-w`, or shell `exit` converge on the
  workspace tab-removal path.
- Session teardown: explicit shutdown remains the primary path and
  `TerminalView::drop` is the backstop.
- The current center layout intentionally has no split owner. Before consuming
  `nebula-split`, define pane/tab ownership and prove that removing one pane
  cannot leak or prematurely shut down another pane's PTY.

App-level hotkeys must be let through by the terminal key handler
(`view.rs` passes `ctrl-shift-t/w` up instead of encoding them as C0 bytes).
