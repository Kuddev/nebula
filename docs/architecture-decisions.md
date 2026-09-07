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

## ADR-0003 - Pebrel 1.6 identity migration

- **Status:** Requested by the maintainer in this working session, 2026-09-07;
  implementation and native package verification in progress.
- **Context:** Display branding alone left users with a Nebula installation
  directory, executable, command and configuration files. Renaming those interfaces
  also affects upgrades, stored credentials and managed integrations.
- **Decision:** Ship `pebrel.exe`, `pebrel-hook.exe`, the `pebrel` command and Pebrel
  configuration names. Keep the existing Inno AppId to identify the same product.
  Migrate a registered installation whose final directory component is
  `Nebula Terminal` to the sibling `Pebrel` directory. Preserve other custom
  directory names and explicit installer directory choices. Only remove known
  installer-owned legacy files; preserve unknown files. Copy legacy configuration
  into the new data directory without overwriting newer files or deleting the
  source, so absolute imports into the old directory remain valid. Serialize
  migration with an exclusive lock, publish copied files atomically, and record
  success only after the copy completes; the success marker prevents subsequent
  launches from restoring files the user deliberately removed. New configuration
  takes precedence over legacy data. Migration failures must be visible and
  retryable. Read old credential and integration identifiers as compatibility
  inputs, while writing new names.
- **Repository:** The existing repository was renamed to `Kuddev/pebrel` on
  2026-09-07, retaining repository ID `1289958986`. GitHub redirects the old
  repository and Git clone/fetch/push URLs. Keep `Kuddev/nebula` unused so that
  creating a new repository at that path cannot take over the redirects. GitHub
  Pages and callers of an action through the old repository name need separate
  migration; this repository had no Pages site or action manifest at verification.
- **Boundaries:** Historical release notes, release assets, upstream attribution,
  source directory names and library crate identifiers are not rewritten as if
  the old releases had different names. New user-facing artifacts and
  documentation use Pebrel. Existing Runtime API and hook protocol names remain
  stable for clients already using them.
- **Release compatibility:** Old clients select an exact `NebulaTerminal-...` asset
  name. A future public release needs a compatibility asset containing the same
  installer bytes until those clients have migrated. Repository redirects do not
  create aliases for renamed asset filenames. Keep published asset filenames and
  tags intact when changing the repository name.
- **Validation:** The installer migration passed 77 isolated native fixture
  checks and a complete Inno Setup syntax build on 2026-09-07. This covers owned
  files, custom directories, shortcuts, PATH, locks and retry behavior; it does
  not stand in for upgrading the user's real installation. Targeted terminal/SSH,
  config migration and update selection tests, architecture contracts, and a
  fresh Windows GPUI ZIP/installer build are the remaining release checks.
  Package tests must use isolated user state. The SSH startup regression checks
  device-attributes delivery; live Helix behavior still needs user acceptance.
- **Revisit condition:** Remove compatibility readers only after support for old
  clients and persisted configurations is explicitly retired.

## 中文说明

记录重大取舍而非每次小修复；事实与测试能推翻旧决定。规范误伤、安全修复与旧预算冲突时，
先记录问题和最小修订，维护者审查后更新合同；不能将“只减不增”变成拒绝纠正规范的理由。
