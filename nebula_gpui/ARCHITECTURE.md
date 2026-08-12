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
