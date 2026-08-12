# Nebula GPUI Foundation

`nebula_gpui` is an isolated GPUI application workspace. It is a validation and
incremental migration surface; it does not replace the existing `nebula_app`
runtime yet.

## Direct Component Use

Views should import approved upstream components from `ui::prelude`:

```rust
use crate::ui::prelude::*;

let action = Button::new("save").primary().label("保存");
let input = Input::new(&input_state);
```

`prelude` deliberately re-exports upstream types without changing their
behavior. Do not create one-to-one wrappers such as `NebulaButton` or
`NebulaInput`. A local component is justified only when it owns a Nebula
business contract, such as a terminal pane, an atomic-save editor workflow, or
a session-aware dock panel.

## Ownership Boundaries

- `ui::bootstrap` owns the single GPUI/component-library initialization point.
- `ui::theme` owns Nebula visual tokens, fonts, density, and global defaults.
- `ui::prelude` is the only approved direct-component import surface for views.
- View code may use GPUI entities and window APIs.
- Terminal, SSH, SFTP, configuration, history, and other core crates must not
  expose `gpui::Entity`, `gpui::Window`, or GPUI actions in their public APIs.

This preserves direct UI composition while keeping the product core portable
to another UI library. UI adapters translate core state and commands at the
application boundary.

## Dependency Policy

The validated dependency set is pinned exactly:

```toml
gpui = "=0.2.2"
gpui-component = "=0.5.1"
gpui-component-assets = "=0.5.1"
```

Do not use wildcard versions, Git `main`, or the tty7 forks. GPUI and
gpui-component must resolve to one GPUI version; verify each dependency change
with:

```powershell
cargo check --locked --offline
cargo tree -i gpui
```

GPUI upgrades happen in a dedicated change with component acceptance testing;
they are not bundled with feature work. Do not fork an upstream crate unless a
documented upstream issue blocks a release, the temporary patch has a defined
removal condition, and the maintenance owner is explicit.

## Workspace Boundary

`nebula_gpui` is excluded from the root Cargo workspace because its current
font stack uses a different native `fontconfig` `links` dependency than the
existing OpenGL app. Keeping it as its own workspace avoids an unsatisfiable
Cargo native-link conflict and leaves the legacy application's lockfile and
build path unchanged.

This boundary does not prevent future reuse of pure Rust core crates, for
example `nebula_terminal`; it prevents GUI native dependencies from being
resolved together before their ownership and rendering boundaries are ready.

## Terminal Vertical Slice

`src/terminal/` hosts the terminal integration and proves the hardest
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

`views/workspace.rs` is the product shell: a custom `TitleBar` plus a center
`TabPanel` of terminal tabs. `terminal/panel.rs` adapts `TerminalView` to the
upstream `Panel` contract; the tab layer owns tab semantics only (title,
close, focus forwarding, lifecycle) and terminal behavior stays in
`TerminalView`.

Lifecycle rules proven by manual acceptance:

- New tab: title-bar button or `ctrl-shift-t`; the new tab activates and
  focuses its terminal.
- Close: per-tab close button (`title_suffix`, because a root-level
  `TabPanel` is treated as locked layout by the component library and never
  renders its own close button), `ctrl-shift-w`, or shell `exit`. All three
  paths converge on `TabPanel::remove_panel`.
- Session teardown: `Panel::on_removed` sends `Msg::Shutdown` immediately;
  `TerminalView::drop` is the backstop. `DockArea` keeps the initial
  `DockItem` snapshot alive, so Drop alone is not sufficient — do not remove
  the `on_removed` cleanup.
- Last tab closed quits the app (`quit_if_empty`, also subscribed to
  `PanelEvent::LayoutChanged` to cover the per-tab close button path).
- The center layout is intentionally a root-level `TabPanel` (no
  `StackPanel` parent): tabs cannot be dragged out into splits, which keeps
  `on_removed` equal to "really closing". Revisit teardown ownership before
  introducing split layouts.

App-level hotkeys must be let through by the terminal key handler
(`view.rs` passes `ctrl-shift-t/w` up instead of encoding them as C0 bytes).
