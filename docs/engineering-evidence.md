# Engineering policy evidence / 规范依据

Reviewed on **2026-09-05** using primary sources and an independent repository
review. Mature projects support these principles, not the claim that our exact
checker or numerical limits are universally correct. No source below establishes
800 or 2000 as the optimal size of every source file.

| Primary source | What it supports | What we deliberately do not infer |
| --- | --- | --- |
| [Google: review standard](https://google.github.io/eng-practices/review/reviewer/standard.html) | Technical facts and evidence should outweigh personal preference; improve code health | A reviewer's preferred structure is automatically mandatory |
| [Google: what to review](https://google.github.io/eng-practices/review/reviewer/looking-for.html) | Review design, complexity, tests and over-engineering | Every possible future extension needs an abstraction now |
| [Google: small changes](https://google.github.io/eng-practices/review/developer/small-cls.html) | Keep changes conceptually focused and reviewable | PR-size guidance proves a universal file-line threshold |
| [VS Code: source organization](https://github.com/microsoft/vscode/wiki/Source-Code-Organization) | A large desktop editor uses explicit layers and module responsibilities | Pebrel must copy VS Code's names, process model or entire framework |
| [Android: modularization patterns](https://developer.android.com/topic/modularization/patterns) | Balance high cohesion and low coupling; choose meaningful module boundaries | More modules or files always means better decoupling |
| [Android: lint baseline](https://developer.android.com/studio/write/lint#create-baseline) | Treat existing findings separately while stopping new regressions | Every old file must be rewritten before a small fix can merge |
| [Chromium: layering cookbook](https://www.chromium.org/developers/design-documents/cookbook/) | Separate reusable core from host-specific code; verify dependency checks actually run | A successful empty scan or path-name regex proves sound architecture |
| [Cargo: dependency sources](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html) and [dev cycles](https://doc.rust-lang.org/cargo/reference/resolver.html#dev-dependency-cycles) | Package identity/source matters; some dev cycles are legal | All dependency graph cycles should be mixed into one prohibition |
| [Rust: modules](https://doc.rust-lang.org/reference/items/modules.html) | Module paths and file paths can differ with attributes | `crate::display` necessarily refers to the legacy renderer |
| [Microsoft: decision log](https://microsoft.github.io/code-with-engineering-playbook/design/design-reviews/decision-log/) | Record consequential architecture decisions and their context | ADRs should be immutable or required for every trivial edit |
| [GitHub: code owners](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners) | Ownership and protected-branch review requirements work together | Adding `CODEOWNERS` alone makes approvals mandatory |
| [GitHub: required checks](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks) | Required checks must actually run; skipped filtered workflows can remain pending | A path-filtered docs-only PR always receives a usable required status |
| [GitHub: secure workflow use](https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions) | Treat contributor code and event input as untrusted | A privileged `pull_request_target` job should execute arbitrary PR code |

## Local evidence and resulting changes

- Initial inventory: 76 Rust files exceeded 800 lines; only 11 exceeded 2000.
  Making 800 a hard gate would add 65 legacy exceptions. We retain the existing
  hard ceiling and make the smaller number a nonblocking review aid.
- The existing settings/split/hook manifests explicitly preserve low-dependency
  production roles. The new policy preserves those concrete contracts, without
  banning legitimate test-only tools on runtime-performance grounds.
- Feature aliases in `main.rs` and `product_ui/mod.rs` disprove a simple forbidden
  `display` import rule. We check declared local crate dependencies and real
  compilation instead of pretending to resolve Rust with a regex.
- Independent adversarial fixtures exposed an empty-base bootstrap bug, ancestor
  symlink traversal and false classification of an external same-named package.
  These cases became regression tests before adopting the checker.
- Historic platform-cfg counting misclassifies comments/strings and some nested
  expressions. It is not used as proof of decoupling or promoted into this gate.

## Confidence and limits

The assurance is narrow: the declared contracts have executable positive/negative
tests and CI wiring. The assurance is not that all future architecture is good,
that all translations are complete, or that GitHub protection is already enabled.
Maintainers must review rule changes and verify the remote activation checklist.

Review this evidence when changing a rule, not by calendar ritual alone. A numerical
threshold should be revised when repository evidence warrants it; cohesion and
compatibility are the objectives, not winning a line-count metric.

中文：采用成熟实践中的分层、增量基线与负责人审查，但不照搬数字、不把正则当语义分析。
门禁自己也有正反测试；能够复现的误报必须修正，而不是让后来的贡献者适应错误规范。
