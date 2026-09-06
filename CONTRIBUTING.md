# Contributing to Pebrel / 贡献指南

Pebrel is the project name. The repository remains `Kuddev/nebula`; existing
`nebula` CLI, configuration, installation and update identifiers are compatibility
interfaces, not opportunities for search-and-replace renaming.

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

1. Fork the existing `Kuddev/nebula` repository and create your work branch from the
   PR target branch. See [INSTALL.md](INSTALL.md) for environment prerequisites.
2. Use the pinned Rust toolchain in `rust-toolchain.toml`. Build the actual GPUI
   product, not an accidentally substituted legacy executable:

   ```sh
   cargo build --locked -p nebula --bin nebula --features gpui-shell
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
cargo check -p nebula --bin nebula --features gpui-shell --tests --locked
```

The isolated i18n test compiles production files, not copied implementations. It
does not test real window layout, every OS integration, or the full application.
Existing platform build/package checks still apply. Never turn a metadata-only
compile check into a claim that UI tests or a packaged application were run.

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
