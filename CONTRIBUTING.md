# Contributing to Pebrel / 贡献指南

Pebrel 1.6 uses the `pebrel` command, Pebrel installation directories, and
`PEBREL_*` environment variables. Legacy configuration and integration identifiers
remain compatibility inputs; follow the [identity migration decision](docs/architecture-decisions.md#adr-0003---pebrel-16-identity-migration)
when changing them. Internal Rust crate and source-directory names remain stable.

## Required reading

- [Architecture and module ownership](docs/architecture.md)
- [Enforced contracts and review rules](docs/project-constraints.md)
- [Decision log and policy change process](docs/architecture-decisions.md)
- [Evidence behind the rules](docs/engineering-evidence.md)
- [Internationalization](docs/internationalization.md) when changing UI text

These rules apply equally to maintainers, outside contributors and coding agents.
They constrain responsibilities and behavior, not personal taste. A demonstrated
problem in a rule is a reason to review the rule, not to conceal or ignore a failure.

## Ways to contribute

- **Report a bug:** use the existing issue form. Include the version/build, OS,
  display scale, relevant settings, minimal steps, expected result and actual result.
  Search existing issues first. Remove tokens, credentials, private paths and command
  history from logs or recordings before sharing them.
- **Propose a feature:** describe the workflow and user need before prescribing an
  implementation. For broad changes, agree on scope before writing a large PR.
- **Translate or improve docs:** follow the language registry/catalog contract;
  preserve named placeholders and review terminology, clipping and fallback. Do not
  mark a language complete merely because English placeholders were copied into it.
- **Fix or test code:** start with a focused regression and an existing module's
  tests. Add fixtures or improve coverage when that is the actual contribution.

No programming contribution is required to be useful. Clear reproduction steps,
native-language review and accessibility feedback also help the project.

## First code contribution

1. Fork the existing `Kuddev/pebrel` repository and create your work branch from the
   PR target branch. See [INSTALL.md](INSTALL.md) for environment prerequisites.
2. Use the pinned Rust toolchain in `rust-toolchain.toml`. Build the actual GPUI
   product, not an accidentally substituted legacy executable:

   ```sh
   cargo build --locked -p nebula --bin pebrel --features gpui-shell
   ```

3. Make the focused change, run the checks below, and open a PR against the agreed
   target. Draft PRs are useful for early design feedback; describe remaining work.
4. Respond to review and rerun affected checks after changes. Maintainers decide
   readiness from the actual evidence, not from checked boxes alone.

Do not include build outputs, local probes, screenshots containing secrets or
unrelated generated files. Preserve third-party license/attribution notices and
identify the source of any externally copied code or assets for license review.

Keep one-off test scripts and probe outputs under `tmp/`; external project checkouts
and investigation notes belong in the reserved `research/`, `reference-projects/`
or `external-probes/` local directories. These are not vendored build dependencies:
do not blanket-ignore `third_party/` or maintained test/diagnostic sources. `docs/`
is private by default; public documentation needs an exact reviewed allowlist entry
in `.gitignore`. Do not publish competitor studies or HTML prototypes by opening a
whole documentation subtree. Ignore rules do not untrack files already in Git;
raise any existing tracked private artifact for an explicit maintainer decision.

## Small, reviewable changes

1. State the user-visible problem and the behavior to preserve. Discuss a new
   cross-layer dependency, persisted format, core abstraction or threading model
   before implementing it. Record significant decisions in the decision log.
2. Keep one conceptual change per PR. A necessary extraction and its behavior
   tests may accompany the feature; unrelated rewrites and formatting may not.
3. Put shared rules in their existing authority. UI modules adapt those rules;
   they must not fork persistence, state transitions or domain behavior.
4. Add regression tests that fail for the defect, and test error/cancellation
   paths where relevant. Explain what was actually run and what was not.
5. Fill in the PR template. A green build is necessary, not sufficient: a
   maintainer must also review cohesion, public interfaces, compatibility and cost.

## Local checks

The fast checker requires Python 3.11+ and no third-party Python packages. Use
`python` instead of `python3` on Windows if that is your configured interpreter.
Use the actual target branch commit, not the feature branch's own HEAD, for PR
ratcheting. Run without `--base` for current-tree checks during development.

```sh
python3 scripts/check_architecture.py --base <PR-base-commit>
python3 -m unittest scripts.tests.test_architecture_budgets scripts.tests.test_architecture_dependencies scripts.tests.test_architecture_governance
cargo test --manifest-path tools/i18n-contract/Cargo.toml --locked
cargo test -p nebula-settings
cargo test -p nebula --test file_line_budget
cargo fmt --all -- --check
```

Run affected behavior tests and the appropriate real product checks as well:

```sh
cargo check -p nebula --bin pebrel --features gpui-shell --tests --locked
```

The isolated i18n test compiles production files, not copied implementations. It
does not test real window layout, every OS integration, or the full application.
Existing platform build/package checks still apply. Never turn a metadata-only
compile check into a claim that UI tests or a packaged application were run.

## CI feedback and package builds

The Preview workflow starts package builds and native tests concurrently on Linux,
Windows, Apple Silicon and Intel macOS. Tests run in separate core/product jobs;
package aggregation requires every native test job and every packaged conformance
check to pass. Stable releases always request the complete workspace test suite.

For pushes and PRs, `python -m scripts.ci plan --base <base-commit>` selects complete
test suites for changed crates and all transitive consumers, including build, dev,
optional and platform dependencies. PRs compare against the merge base; pushes
compare the entire pushed range. Renames cover both old and new owners. Changes to
manifests, lockfiles, shared infrastructure or unclassified inputs select every
crate. Unavailable history also selects the full suite. The architecture job still
runs for every PR. Weekly Preview and manual full runs cover the whole workspace;
the separate weekly/manual Linux job also exercises the explicit legacy shell.

Tests use `.github/ci-profile.toml`: no debug information, no LTO, and unoptimized
dependencies, including the named overrides inherited from the development profile.
The product's normal GPUI configuration is checked before the complete test suite
with GPUI test support. This compiles the two feature configurations while avoiding
the previous repeated workspace/product/dialog test builds. The profiles used for
normal interactive development and release optimization remain separate.

Cargo downloads are shared once per operating system and architecture. Compiled
targets are cached separately for core/product/release roles, so a large Git
dependency is not stored three times per platform and evicts another platform's
build. Preview and stable packages share the release target cache. The pinned
compiler, SDK, build flags, manifests and CI profile participate in target keys;
Cargo still validates its own fingerprints. Complete downloads and compiled
targets are saved separately, retaining completed compilation after a test fails.
macOS also caches the compiled Swift verification helper and its imported modules.
Already compressed package artifacts are uploaded without another compression pass.
Release builds retain O3 for the product and thin LTO; the large product crate uses
16 codegen units to parallelize LLVM compilation. Native package validation still
checks the real product binary, signing, manifests and conformance evidence.

The target is less than ten minutes for all platforms, including packages, once
caches are populated. The Actions summaries report actual time, cache cost and
package bytes; cold compilation, runner queues and Apple notarization can exceed
that objective. A timeout or omitted test is never reported as meeting it. Compare
completed runs before claiming the target has been achieved.

On 2026-09-08, the populated-cache [cross-platform Preview run](https://github.com/Kuddev/pebrel/actions/runs/34188496290)
at `7f390b7` passed in **8m52s**, including all native tests, package conformance,
aggregation and uploads. The separate [complete Windows packaging run](https://github.com/Kuddev/pebrel/actions/runs/34185855622)
at `9fa1f7e` passed in **9m22s**. Both runs used `publish=false`.

| Verified job | Job duration |
| --- | --- |
| Linux x86_64 packages and conformance | 5m59s |
| Apple Silicon DMG and conformance | 5m52s |
| Intel macOS DMG and conformance | 7m57s |
| Windows Preview executable and conformance | 8m10s |
| Windows ZIP and installer, separate complete packaging run | 8m55s |

The Preview Windows lane validates the comparison executable; the separate run
verifies the ZIP and installer. These measurements establish the populated-cache
result, not a cold-build or notarization-time guarantee.

Package size changes retain the bundled fonts and licenses. macOS DMGs use LZMA
(`ULMO`, supported by the minimum macOS 14 target); Windows uses a 128 MiB LZMA
dictionary so the embedded and installable copies of the font can share compressed
data. Windows ZIPs use the smallest supported .NET compression level, with an
`Optimal` fallback for Windows PowerShell 5. Windows packaging builds the product
and hook without linking the unshipped component lab. Package helper tests and
native mounts/install layouts remain part of verification.

Measured package sizes against the published 1.6.0 assets are below. macOS sizes
come from the successful native package jobs in [run 34176429079](https://github.com/Kuddev/pebrel/actions/runs/34176429079)
at `12d65b1`; Windows sizes come from the complete packaging run at `9fa1f7e` above.
These are actual built assets, not estimates or the controlled fixture below.

| Asset | Published 1.6.0 bytes | Optimized bytes | Reduction |
| --- | ---: | ---: | ---: |
| Apple Silicon DMG | 33,653,425 | 23,626,972 | 29.79% |
| Intel macOS DMG | 34,948,801 | 25,254,879 | 27.74% |
| Windows installer | 30,277,117 | 23,101,991 | 23.70% |
| Windows ZIP | 31,735,316 | 31,218,437 | 1.63% |

The Windows compression comparison on 2026-09-08 used the same published 1.6.0
payload and Inno Setup 6.7.1 for both runs: the default dictionary produced
30,275,390 bytes in 39.4 seconds; 128 MiB produced 22,953,293 bytes in 49.8 seconds
(24.18% smaller). This isolates compression cost; it does not predict the size of
a newly compiled version or timings on GitHub runners.

## Review and enforcement

`architecture-contracts` is the stable PR job name. Maintainers must enable it as
a required check and require Code Owner approval in the target branch ruleset;
see the [activation checklist](docs/project-constraints.md#server-side-activation).
Local hooks are convenient, but bypassable; they are not the enforcement boundary.
Submitting a workflow or `CODEOWNERS` file does not configure server-side rules.

Do not silence a failing contract by increasing a budget, removing test coverage,
compressing code, adding `continue-on-error`, or broadening an exclusion. If the
contract is wrong, submit a focused policy fix with a reproducer and both positive
and negative tests. Review a justified policy change separately from unrelated
feature work; there is no routine `--skip-architecture` option.

## 中文摘要

- 先读架构图、工程合同和决策记录；按职责拆分，不按行号切片。
- 2000 行是现有仓库的防灾上限，800 行只提示审查，不是“大厂标准”。
- 普通功能 PR 不得增加存量债务；有问题的规则可以修订，但要有反例、测试和维护者审批。
- 新增核心抽象、依赖方向、持久化或线程模型改变要先说明设计，不强迫每个小修复写 ADR。
- 修改热路径要给出成本证据；不能为了“可扩展”增加没有实际用途的框架。
- PR 要附实际测试结果；本地钩子和勾选框不代替服务端必需检查与人工评审。
