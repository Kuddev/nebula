# Architecture decisions / 架构决策记录

## Process

Record decisions that change dependency direction, core ownership, persistent
interfaces, threading/lifetime, performance contracts or governance itself. Do not
require a new record for each typo or ordinary bug fix. Records are reviewable;
an accepted decision may be superseded when evidence changes.

Use: **Status; Context; Evidence; Decision; Alternatives; Consequences; Validation;
Revisit condition.** Identify the accountable maintainer in the PR review. A policy
change must include a failing legitimate example when correcting a false positive,
plus a violation that must remain rejected. Do not use a policy edit to conceal an
unrelated feature's growth. Remote approval/enforcement is not implied by this log.

## ADR-0001 — Evidence-based architecture contracts

- **Status:** Adopted in the working-tree implementation, 2026-09-05; pending normal
  repository review and server-side activation. Initial owner entry: `@Kuddev`.
- **Context:** Multiple rendering adapters share behavior; a large contribution
  volume needs stable boundaries. Existing source-size documentation was ignored
  by Git and the old scanner could inspect local build/probe artifacts.
- **Evidence:** [Primary-source review](engineering-evidence.md), workspace manifests,
  feature-selected module aliases, and positive/negative checker fixtures.
- **Decision:** Keep the modular application and 2000-line hard / 800-line advisory
  limits. Share a precise legacy inventory, enforce production crate directions,
  and verify pure i18n independently. Review module cohesion and lifecycle manually.
- **Alternatives rejected:** An abrupt 800-line hard limit (65 additional oversized
  legacy files); import-name blacklists (incorrect under `#[path]`/cfg); mandatory
  microservices/traits; machine-specific nanosecond CI thresholds.
- **Consequences:** Ordinary PRs cannot silently expand debt. Large legacy changes
  may require a responsibility extraction. Valid source-layout or policy changes
  can require an explicit governance update and new fixtures, not a skip flag.
- **Validation:** The checker suite covers legitimate and forbidden dependencies,
  base ratcheting, paths, parsing and scan errors. CI runs those tests before using
  the checker. Real product compilation and human review remain separate evidence.
- **Revisit condition:** A reproducible legitimate change is rejected, the source
  layout changes, or measured review cost outweighs a threshold's benefit. Correct
  the narrow rule with tests; do not treat this ADR as immutable proof of quality.

## ADR-0002 — Static extensible UI translation

- **Status:** Implemented in the working tree, 2026-09-05; not a new release claim.
- **Context:** Shared language choices and translations must not add parsing,
  locking or string allocation to ordinary UI text lookup.
- **Decision:** One registry, build-time validated catalogs, typed static lookups,
  English fallback for partial locales, and a separate parameter-formatting path.
- **Consequences:** Adding a language is a registry/catalog change and requires a
  build. Initial coverage is partial; live downloadable language packs and complete
  locale-aware formatting are not promised.
- **Validation:** Compile production lookup/generator code in the isolated contract
  workspace; test invalid catalogs, fallback, locale matching and zero allocations.
- **Revisit condition:** A real requirement for runtime packs, richer plural/date
  formatting or RTL layouts warrants a separately measured design. See the
  [internationalization contract](internationalization.md).

## 中文说明

记录重大取舍而非每次小修复；事实与测试能推翻旧决定。规范误伤、安全修复与旧预算冲突时，
先记录问题和最小修订，维护者审查后更新合同；不能将“只减不增”变成拒绝纠正规范的理由。
