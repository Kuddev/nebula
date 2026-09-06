# Pebrel engineering contracts / 工程约束

These are project contracts, not claims that a numerical score proves good design.
The [evidence review](engineering-evidence.md) separates established practices from
local thresholds. See [architecture](architecture.md) for module responsibilities.

## 1. File size: a safety limit, not a design score

- Keep the existing **2000 physical-line hard limit**. Apply it to first-party
  Rust and maintained `.py`, `.ps1`, `.mjs`, `.js`, `.sh`, `.lua` sources in the
  declared roots. No maintained script exceeded this limit at adoption.
- **800 lines is advisory only**. Review cohesion and consider extraction; a
  cohesive 801-line module is not automatically worse than fragmented wrappers.
- Physical lines include comments, tests and blanks; CRLF/LF and a final line
  without a newline are handled consistently. Do not remove useful tests/comments,
  compress formatting, or create `part1/part2` files to pass a size check.
- `architecture/file-budgets.txt` is the single shared source of roots, the hard
  limit and legacy allowances. Eleven existing files exceeded 2000 at adoption.
  They receive their measured size, not additional growth room.
- A normal PR cannot add/raise an allowance or raise the default limit. With
  `--base`, an oversized file's permitted size is also bounded by its actual base
  size, so an old allowance cannot be reused after the file has shrunk.
- Remove allowances when a file is deleted or reaches the default limit. A
  reduction above that limit gives a tightening notice. No wildcard allowances.

The Python checker runs offline and scans untracked local source additions too.
It fails on malformed budgets, missing roots, read errors and symlinked source
paths, including symlinked ancestors. It scans declared first-party roots only,
not repository-root probes, `tmp/`, release bundles or `third_party/`. A `target/`
directory is skipped only at a Cargo package/workspace root; a real `src/target/`
module remains covered. Dependency caches are not product sources.

Generated translation tables live in Cargo output, not handwritten source roots.
JSON catalogs and other data assets use semantic/payload tests, not arbitrary
line-count limits. `nebula_app/tests/file_line_budget.rs` reads the same budget;
the Python PR checker additionally enforces history-relative ratcheting.

## 2. Dependency direction

`architecture/dependencies.toml` classifies every root workspace member and lists
allowed local dependencies separately for normal, build and test use.

- Core crates must not depend on the application or acceptance lab. The policy
  rejects direct renderer packages in core production/build dependencies.
- `nebula-settings`, `nebula-split` and `nebula_hook` retain their existing
  zero-production-dependency contracts. Test-only tools are not prohibited by a
  runtime-performance argument; they remain subject to normal dependency review.
- Normal/build local dependency edges must be acyclic, across the declared target
  and optional configurations. Dev edges are checked for allowed direction but
  do not create production cycles; the existing config/derive dev cycle is legal.
- Aliases resolve to their declared `package`; inherited workspace paths resolve
  from the workspace root. Only resolved local paths form workspace graph edges.
  An external older package with the same name is not silently treated as local.
- Every new workspace member needs a classification and source root. Workspace
  globs/exclusions currently fail explicitly rather than silently escaping review.
  Custom Rust target paths may be shared within declared roots, not outside them.

This checker is not Cargo's resolver: it does not prove transitive third-party
dependency purity or analyze Rust macro/cfg-expanded intra-crate imports. Real
feature/platform builds remain required. In particular, `crate::display` is a
feature-selected shared facade in the GPUI product; a path-name blacklist would
be incorrect. Do not add such a blacklist without semantic evidence and fixtures.

## 3. Responsibilities, interfaces and cost

Required human review rules:

- One authority for shared behavior: persistence, language registry, split rules,
  terminal state and domain transitions must not be reimplemented per UI shell.
- Group by capability and lifecycle. Extract domain rules, I/O adapters, rendering
  and tests where they have distinct responsibilities; do not require every tiny
  feature to create all four files or another crate.
- Keep state private; expose the smallest useful command, query or result type.
  Default to private or `pub(super)`; justify broader visibility. Avoid catch-all
  `utils`, global service containers and traits with no concrete boundary need.
- Views must not block on network, disk scans, subprocess waits or long-held locks.
  Async work needs ownership, cancellation, stale-result handling and cleanup.
- Hot-path changes require representative cost evidence. Static translation
  lookup has a tested zero-allocation contract; formatting and cold-path I/O are
  different operations. No universal ban on allocation or numeric timing gate.
- New UI text uses typed message IDs, named placeholders and explicit fallback.
  The [i18n contract](internationalization.md) defines coverage and extension.
- Preserve compatibility identifiers and persisted semantics unless a separately
  reviewed migration explicitly changes them.

These judgments cannot be proven by line counts or a source regex. Reviewers must
ask whether an extraction reduces knowledge shared across modules, not just whether
it produces more files.

## 4. Tests and rule changes

Each new automated gate needs a stated invariant, a passing legitimate example,
a failing violation, and fixtures for known false positives. Guardrail bugs are
bugs: fix them before calling the gate mandatory. A failing architecture check must
not be ignored, but a demonstrably defective policy must be revisable.

Record important changes in [the decision log](architecture-decisions.md): context,
evidence, alternatives, consequences, validation and a replacement/removal condition.
Ordinary fixes do not need ceremonial ADRs. A change to a budget or rule is a
dedicated governance change, not an unexplained edit hidden inside a feature PR.
No automatic exception-adding or budget-increasing command is provided.

Emergency/security repairs must not be forced into a dangerous broad refactor just
to preserve a flawed metric. Escalate the demonstrated conflict to a maintainer,
record the scoped policy decision and its regression test, then repair the contract.
Do not silently disable the job or permanently widen unrelated allowances.

Existing platform-cfg counting is a historical heuristic, not a proof of platform
decoupling. It is not promoted to this new gate: its comment/string/nesting behavior
needs separate work. Fork patches retain exact-revision pins, thin adapter scope,
recorded motivation and an upstream-removal condition. Release safety remains in
`AGENTS.md` and the existing verified release instructions.

## Server-side activation

The repository files alone do **not** activate GitHub merge protection. A maintainer
with the appropriate repository permissions must configure and verify:

1. Require a PR and the unique **`architecture-contracts`** status check on protected
   branches; require an up-to-date branch or an appropriately configured merge queue.
2. Require Code Owner approval. `@Kuddev` is the initial owner entry; verify the account
   has write permission, and extend ownership to real maintainers as the team grows.
   Before enabling this requirement, ensure another authorized owner can review a
   maintainer-authored PR: an author cannot approve their own PR. With only one
   maintainer, explicitly resolve and document this limitation and the emergency
   process first; do not enable an unfulfillable approval requirement or invent an owner.
3. Dismiss stale approvals / require approval of the latest reviewable push. Protect
   `CODEOWNERS` itself, workflows, checkers, budgets and decision records; the catch-all
   owner entry covers these files as well as product source.
4. Apply rules to administrators and restrict bypass permissions according to the
   repository's emergency process. A normal PR author must not waive their own gate.
5. Validate with a disposable PR: a known violation must fail and block merging;
   the corrected PR must pass. Confirm review invalidation on a later policy edit.

The workflow runs for every PR, including docs-only PRs, with no path filter and
without `continue-on-error`. It also accepts `merge_group` events. It uses read-only
permissions and ordinary `pull_request`, not privileged `pull_request_target` to
execute contributor code. The initial policy's base lacks a budget; only that
absence permits bootstrap. Empty/invalid budgets and unavailable base commits fail.

Checkers and workflows are versioned code that a PR can change; required reviews
and server-side settings are the trust boundary. This change does not claim those
settings have already been enabled or that remote negative-PR tests have been run.

## 中文要点

硬门禁只拦明确可测的合同：规模红线、存量不增长、清单依赖方向和独立编译/行为测试。
800 行仅提示；模块职责、抽象是否值得、线程生命周期和热路径成本仍须人工评审。
规范本身允许基于证据修正，不能靠“规则就是规则”维护错误设计，也不能借修规则绕过问题。
服务端必需检查与 Code Owner 审批启用并实测后，才构成真正的合并约束。
