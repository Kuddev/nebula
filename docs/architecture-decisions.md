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
  name. Version 1.6.0 supplied that alias with identical installer bytes. On
  2026-09-12, the maintainer explicitly retired this alias starting with 1.7.0.
  The published 1.6.0 update resolver already prefers the Pebrel filename;
  clients that require the old name must download the current installer from the
  Release page. Current CI publishes only the Pebrel installer, and the shared
  manifest rejects an old-name extra asset from 1.7.0 onward. Historical manifests
  still require their original assets and byte identity, covered by positive and
  negative release-helper tests. This does not retire configuration or protocol
  compatibility readers. Repository redirects do not create aliases for renamed
  asset filenames; keep existing published assets and tags intact.
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

## ADR-0004 - Current conversation identity at workspace save

- **Status:** Implemented for the maintainer-reported restore defect, 2026-09-07;
  validation is part of the 1.6.0 release candidate checks.
- **Context:** A Codex pane can identify its foreground program without receiving
  a session ID from the CLI hook. Saving only the program restores the tab but
  cannot construct an exact conversation resume command.
- **Decision:** Keep hook identities authoritative. For WSL and Linux, match the
  process environment to both the Pebrel instance and pane, then read only the
  first metadata record of open Codex rollout files. Accept one main conversation
  whose filename and metadata agree. Do not choose by working directory or time.
  Native Windows and macOS keep the existing hook path.
- **Lifecycle:** Queries use the existing background executor, a two-second time
  limit and bounded output. Results carry the foreground command generation.
  Window close and the application Quit action allow up to three seconds before
  saving and stopping panes; the UI thread remains responsive. A refreshed
  inferred identity is required at close. Hook identities are not replaced.
- **Persistence:** Reuse the existing optional `source` and `session_id` fields;
  there is no new file format or dependency. A missing or ambiguous ID cannot
  launch a different conversation as a fallback. Existing autosave and the final
  operating-system shutdown callback save the identities already available.
- **Validation:** File save/load and exact resume commands, primary-thread
  metadata selection, WSL user arguments, pane/instance isolation, output limits,
  stale-result rejection, and a subprocess deadline have focused regressions.
- **Revisit condition:** Replace the procfs fallback when a supported CLI session
  identity API covers these launches consistently.

## ADR-0005 - Molecular diagrams and file hover previews

- **Status:** Requested in the current working session, 2026-09-08; implementation
  and validation in progress, pending normal review.
- **Context:** Markdown and AI CLI output need the same SMILES interpretation.
  File tree previews must not decode large images or PDFs during row rendering.
- **Decision:** Keep SMILES parsing and SVG depiction in one presentation-independent
  application module, using exact `chematic-smiles` / `chematic-depict` 1.0.9 pins
  (MIT OR Apache-2.0). Use the existing GPUI SVG image pipeline. Disable the
  depiction crate's optional PNG/PDF backends; they add unrelated renderers.
  Bound input, molecule size and cache capacity, perform layout in background work,
  and preserve source text on invalid or unsupported input. UI adapters own their
  pending work; discarded views cannot receive stale results.
- **Preview boundary:** Native Windows PDF first-page rendering lives under
  `platform`, through Windows.Data.Pdf using the already-resolved `windows` 0.61.3
  family. Image decoding and thumbnail caching share a bounded application adapter.
  Cache identity includes path, modification time and length; no preview changes
  the user's file or creates a persistent format.
- **Alternatives:** Handwritten SMILES geometry would duplicate specialized ring
  and stereochemistry rules. A Python/Node subprocess or a web service would add
  deployment or availability requirements. A whole PDF viewer is outside hover
  preview responsibility.
- **Validation:** Regression fixtures cover common rings, branches, charges and
  invalid SMILES; unsupported stereo notation preserves source because the pinned
  depiction backend does not project atom parity / alkene direction. Preview checks
  cover dimension bounds, cache invalidation and
  real first-page rendering. Real product compilation and UI inspection are
  reported separately from these behavior tests.
- **Revisit condition:** Remove or replace an adapter when upstream support covers
  the same behavior, or measured parsing/rendering cost exceeds the bounded use.

## ADR-0006 — Remote file workflows and persistent host organization

- **Status:** Implemented in the working tree on 2026-09-08 for the maintainer's
  requested workflows. Native compilation and focused regression checks passed;
  full GPUI/Explorer/server acceptance and normal maintainer review remain separate.
- **Context:** Upload drops only target the currently visible directory; Windows
  has no outbound remote-file drag. Remote files cannot enter the editable document
  lifecycle. The saved-host MRU also serves as the long-term host list.
- **Decision:** Keep transfer policy and I/O in the existing SSH/SFTP capability.
  A gesture captures the source and destination identities; asynchronous directory
  checks and native dialogs cannot retarget it when the focused pane changes.
  Windows outbound drag uses a scoped native OLE adapter and virtual file streams;
  its worker owns the drag, materialization, cancellation and temporary resources.
  The GPUI thread never waits for file contents. Reuse the already pinned Windows
  bindings rather than adding another native framework or changing the GPUI fork.
  The worker associates its input queue with the captured source window thread
  for the native gesture, then detaches it. File descriptors are prepared lazily,
  and initialization rechecks the physical button state before entering OLE.
  The direct `windows-core` 0.61.2 edge is required by the official COM implement
  macro and shares the version already resolved by `windows` 0.61.3.
- **Documents:** Share text decoding, BOM/newline handling and conditional-save
  semantics between local and remote documents. The editor owns a fixed document
  source, dirty buffer and in-flight revision. SFTP reads and writes run on the
  existing network runtime, use bounded text snapshots and preserve drafts on
  failure. Source conflicts and unsupported replacement semantics remain visible.
- **Hosts:** Existing profiles remain the authority for explicitly managed hosts;
  recent connections remain bounded separately. Optional organization metadata is
  backward compatible, excludes credentials, and survives profile edits/renames.
  UI search and grouping consume cached profile data, indexing profiles once per
  filter pass instead of scanning every profile for every candidate. Import/export must validate
  before changing persisted state and must never serialize private credentials.
  Profile saves reuse the existing OS-handle lock and atomic state writer, and
  reject a snapshot that differs from the file last loaded by that writer.
- **Alternatives:** A second SSH transport, renderer-specific persistence engine,
  synchronous download in a drag callback, or enlarged MRU alone would preserve
  the workflow gaps or duplicate existing contracts.
- **Validation:** Windows GPUI product compilation, architecture, i18n, settings
  and line-budget checks passed. Production-source regressions cover destination
  identity, cancellation, names, encoding, host retention and import. Eight remote
  save cases use the real SFTP codec over an injected peer, including permission
  failure, publication rollback and competing writes. A separately invoked desktop
  OLE test copies a Unicode filename and its contents between owned test windows.
  These tests do not exercise real Explorer, GPUI gestures or SSH authentication.
  Full-workspace formatting still reports differences in other working-tree
  changes; the files modified for these workflows have no remaining format findings.
- **Revisit condition:** Replace the OLE adapter when the pinned GPUI exposes an
  equivalent Windows virtual-file drag with the same ownership contract. Revisit
  persistent host identity before adding shared/cloud mutation or credential export.

## 中文说明

记录重大取舍而非每次小修复；事实与测试能推翻旧决定。规范误伤、安全修复与旧预算冲突时，
先记录问题和最小修订，维护者审查后更新合同；不能将“只减不增”变成拒绝纠正规范的理由。

## ADR-0007 — Editable documents, reusable layouts and pane drop targets

- **Status:** Implemented in the working tree, 2026-09-08, for the requested editor,
  recipe and quick-terminal workflows; pending normal maintainer review.
- **Context:** The code viewer accepted edits without a save operation. Markdown
  had a separate read-only lifecycle. Dragging a tab into a split workspace always
  split the root. The quick window used fixed monitor dimensions on creation.
- **Decision:** A GPUI file editor owns its input buffer, dirty state and scoped
  asynchronous loads/saves. One renderer-independent document snapshot implements
  UTF-8/BOM/newline handling, external-change checks and temporary-file replacement.
  Partial/invalid previews cannot be saved. Preview image URL rewriting never
  changes the source buffer. Markdown headings and source positions come from the
  existing Markdown parser. Root blocks remain intact and receive reference
  definitions when rendered as individually navigable preview blocks.
- **Layout authority:** Recipes wrap the existing session schema with a name and
  format version, stored under the application settings directory. They reuse
  `restore_tab` with AI resume disabled and exclude agent identity from stored
  trees. They save terminal layout/launch identity, not command history or output.
  Reads and writes belong to the recipe view's background tasks. Restoring creates
  a separate regular window. Bounds, count and version validation precede restore.
- **Split authority:** `nebula_split::SplitTree::dock_at_leaf` owns subtree grafting.
  Local/cross-window gestures carry a stable destination pane ID and direction;
  preview rectangles use the same tree layout math as the resulting split.
- **Preferences:** `nebula_settings` owns the quick-window mode and optional logical
  dimensions. The window registry samples only normal, settled window bounds,
  saves changes through the existing preference writer and clamps restored sizes
  to the current display. Animation positions do not become saved dimensions.
  New built-in terminal/chrome palette data also lives in the shared settings crate.
- **Alternatives:** A second editor persistence implementation, Markdown source
  modification for navigation, root-only docking, or replaying command history
  from a layout would add inconsistent state or change the requested semantics.
- **Validation:** Regression coverage targets conditional saves, BOM/CRLF and
  Unicode, truncation, heading positions/references, recipe round trips, malformed
  preferences, palette contrast and grafting a fifth pane without resizing the
  three unrelated panes. Native product checks and actual UI results are reported
  separately; this record does not assert release or service-side enforcement.
- **Revisit condition:** Add command replay or externally imported recipes only
  with a deliberate replay contract. Replace the preview block adapter when the
  pinned TextView exposes an equivalent public heading/navigation API.

## ADR-0008 — Formula bitmap admission and preview view lifetime

- **Status:** Implemented for the maintainer's memory reduction request,
  2026-09-09; validation and normal review tracked separately.
- **Context:** The shared 48 MiB scientific cache bounded completed resources,
  but two workers could allocate formula bitmaps before cache eviction. Reading
  created lazy preview views that remained retained after switching to source.
- **Decision:** Reuse bitmap geometry preflight for both worker admission and
  allocation. Reserve formula output bytes against the existing cache allowance
  before starting a worker; shrink the LRU allowance while reservations live and
  release reservations on success/failure. Compose alpha directly in the final
  BGRA allocation. Source mode releases parsed preview views while preserving
  the outline, source, scroll position and editor undo state.
- **Boundaries:** This does not change worker count, persistence, terminal
  identity or dependencies. It does not cap the entire process: compiler/glyph
  scratch, GPU copies, queued inputs, other images and references outside the
  cache remain separate costs. Molecular rendering is currently disabled.
- **Alternatives:** Evicting only after allocation retains the peak; shrinking
  the cache constant alone does not reserve in-flight output. Evicting arbitrary
  actively selected Markdown blocks would lose selection state.
- **Validation:** Regression tests cover fractional-DPI preflight/allocation,
  alpha overlap, reservations exceeding available bytes, failure release,
  cold-entry eviction and source/preview transitions. Native product checks and
  any measured memory reduction are reported separately.
- **Revisit condition:** Extend reservations to other resource types when they
  are enabled/profiled; add viewport eviction only with preserved selection and
  measured parsed-view accounting. No 50 MB process-wide guarantee is implied.

## ADR-0009 — Optional in-app AI message toasts

- **Status:** Requested by the maintainer, 2026-09-11; implemented in the working
  tree, with native validation pending.
- **Decision:** Add the default-on `ai_toasts` preference to `nebula_settings` and
  its existing persistence/reset contracts. The GPUI adapter caches it with other
  runtime settings. Disabling it hides only in-app AI completion/confirmation
  cards; native system notifications, tab indicators and terminal state retain
  their existing behavior. Source identity reuses the shared agent registry.
- **Lifetime:** Existing cards use the component notification identity and
  dismissal lifecycle. A settings observer dismisses only AI cards across open
  windows; deferred startup delivery rechecks the preference. No second queue,
  background service, dependency or per-event settings-file read is introduced.
- **Validation:** Regression coverage includes defaults, parsing, round trips,
  reset, independent delivery channels, search, and component-card dismissal.
  Native compilation and UI results must be reported separately.
- **Revisit condition:** Add separate system-notification or per-agent controls
  only when requested, rather than expanding the meaning of this persisted key.
