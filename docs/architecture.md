# Pebrel architecture / 架构地图

## Shape: modular desktop application

Keep a modular monolith: domain rules and adapters communicate inside the desktop
application unless a current lifecycle or isolation requirement justifies another
process. Existing mux, CLI hook and acceptance-lab processes have concrete roles;
they are not a reason to introduce generic microservices or a service locator.

The dependency direction is **composition/UI → application capabilities → shared
domain rules**. Platform and I/O details adapt domain inputs/results. They must not
make a domain crate depend on a view. This is a responsibility model, not a demand
to rename all existing directories or create abstract interfaces everywhere.

## Current ownership map

| Area | Owns | Must not become |
| --- | --- | --- |
| `nebula_settings` | Runtime settings, language registry, shared preference contracts | A UI/widget library |
| `nebula_split` | Split tree, geometry, navigation rules | Window management or rendering |
| `nebula_terminal` | Grid, VT processing, terminal/PTY behavior | Product panels or GPUI state |
| `nebula_config`, `nebula_config_derive` | Configuration abstractions and derives | Application orchestration |
| `nebula-completions` | Completion matching and presentation-independent results | Terminal view ownership |
| `nebula_hook` | Small process/lifecycle hook bridge | An application dependency container |
| `nebula_app/src/i18n` | Static lookup, locale resolution and formatting | Runtime catalog parsing or UI ownership |
| `nebula_app/src/math` | Parse, validate, layout, compile and cache responsibilities | A duplicated per-shell math engine |
| `nebula_app/src/platform` | Explicit platform capabilities and native adapters | A dumping ground for unrelated logic |
| `nebula_app/src/gpui_shell` | GPUI views, UI state, commands and subscriptions | A second settings/domain implementation |
| `nebula_app/src/product_ui` | Feature-selected shared presentation facade | A route to legacy rendering dependencies |
| `nebula_app/src/display`, `renderer` | Legacy rendering and still-shared extracted models | A source of new undifferentiated functionality |
| `nebula_gpui` | Component acceptance lab | A dependency of the product |
| `nebula_app/build`, `tools/i18n-contract` | Generation and independent contract verification | Runtime configuration loading |

The map records current responsibilities; it does not assert that every legacy
file has already reached the target shape. Eleven oversized legacy files remain
under [ratcheted budgets](project-constraints.md). Move their independent rules
when working on the relevant capability; do not mix a whole-app rewrite into a fix.

## Rust module names are not file locations

`main.rs` selects `product_ui/mod.rs` as the `display` module when the legacy shell
is disabled. It likewise selects product adapters for several legacy-looking paths.
The product facade currently reuses extracted models via `#[path]` declarations.
`i18n/outcomes.rs` is compiled as the separate root module `localized_status`.

Consequently, neither an import containing `display` nor a file beneath that folder
proves that the product loads the legacy renderer. Keep semantic boundaries tested
with the actual feature configuration. Avoid relocating code merely to satisfy a
string-matching architecture tool.

## Feature modules and lifecycle

For a capability that grows, prefer a small entry/facade plus cohesive child modules:

```text
capability/
  mod.rs       exposed operations and orchestration
  model.rs     owned state and transitions, when substantial
  adapter.rs   I/O/platform conversion, when needed
  view.rs      renderer-specific presentation, when needed
  tests.rs     behavior contracts/fixtures, when substantial
```

This is a possible shape, not a mandatory directory scaffold. Name children after
their real responsibility (`navigation`, `shell_picker`, `parser`, `layout`), not
`part1`, `common2` or `misc`. A short cohesive module can stay a single file. Keep
orchestration readable; it should not absorb every callback, parser and widget.

Cross-feature calls should use deliberately exposed commands/results rather than
mutating another feature's internal collections. Prefer concrete types and plain
functions until a second real implementation, testing seam or native boundary
justifies a trait. Do not make fields public simply to make extraction compile.

Background work carries an owning scope and cancellation/staleness policy. Results
must be applied only to a still-valid view/session. Renderer callbacks should use
prepared state, not synchronously reload preferences, scan disks or wait for a child
process. Measure hot-path effects instead of extrapolating a tiny benchmark to the
whole product.

## Extension checklist

- New setting: shared registry/model → persistence contract → shell adapter → tests.
- New language: one registry row + catalog; reuse static generation and fallback.
- New panel: put presentation under the shell, reuse existing domain operations.
- New platform support: model the capability, then implement a native adapter;
  unsupported behavior must be explicit, not a silently successful stub.
- New dependency/crate/long-lived service: first document its responsibility,
  direction, cost, ownership and why an existing module is insufficient.

See [CONTRIBUTING](../CONTRIBUTING.md), [enforced contracts](project-constraints.md),
and [decision records](architecture-decisions.md). Architectural review evaluates
cohesion and knowledge shared across boundaries; CI provides narrower mechanical
checks, not a guarantee against every future design mistake.

## 中文摘要

坚持模块化桌面应用：界面负责呈现与编排，领域规则有唯一权威实现，平台和 I/O 放在明确适配边界。
按能力、状态归属和生命周期分组，不为了“可扩展”先造微服务、全局容器或层层 trait。
拆文件必须减少跨模块知识，不以文件数增加作为解耦证明；旧大文件按相关功能逐步治理。
