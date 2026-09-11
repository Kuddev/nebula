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

## 5. UI 设计约束 / UI design constraints

用户在 2026-09-09 明确要求将交互反馈纳入工程约束。以下是产品实现与人工
评审合同，不是“调用了某个组件就自动合格”，也不表示既有界面已经全部达标。
新增或修改 UI 时，必须同时检查行为、状态、视觉与实际命中区域。

### 5.1 交互反馈与状态可见性

- 每个交互控件应声明适用的状态：默认、hover、pressed、键盘焦点、selected、
  disabled、loading、success、error。不是每个控件都需要全部状态，但不能遗漏
  用户理解当前操作所必需的状态。
- 操作必须形成“触发 → 执行 → 可感知结果”的闭环。只绑定 `on_click`、写入
  剪贴板或调用接口，不等于完成用户可用的交互。
- 小范围操作优先**就地反馈**；toast 是辅助的非阻塞通知，不应每次操作都用
  大弹窗或重复通知打断阅读。失败需要清楚说明原因或可执行的恢复方式。
- success 只能在实际操作完成、或平台接口已接受写入后出现，不能在任务尚未
  执行时提前显示。接口若返回错误必须处理；接口不暴露完成/错误状态时，应在
  验证记录中明确能力边界，不得冒称已经做了系统级成功验证。
- 重复点击、取消、超时、视图关闭和旧异步结果不得破坏反馈状态。定时恢复要
  随拥有它的视图释放，新的操作应刷新期限，旧定时器不能提前清除新的反馈。

### 5.2 复制操作的具体合同

- 复制代码、路径、公式等内容后，必须提供用户能辨认的结果。轻量复制的默认
  模式是“复制图标 → 勾选图标 + 已复制说明 → 恢复”，而不是点击后毫无变化。
- 成功反馈期间，即使鼠标移开，反馈仍应可见。恢复时间使用共享实现或令牌，
  约 1.5 秒是当前交互的起点，不是通用平台标准；连续复制要重置显示期限。
- 成功与失败不能只用颜色区分。复制按钮变化、tooltip/短文案与可用的辅助技术
  状态通知应表达同一事实；失败不能显示成功勾。
- hover 显示的复制按钮使用覆盖式定位，不为隐藏按钮预留永久布局槽位，不在
  显示/变勾/恢复时移动代码、公式、邻近文字或滚动位置。
- 复制代码默认复制原始代码，不夹带语言标签、Markdown fence、行号或装饰。
  “复制 Markdown 源码”如有需要应是独立且命名明确的操作。
- 剪贴板适配、反馈期限和成功/失败语义保持单一权威实现，不在每个视图里复制
  一套略有差异的定时器或虚假的成功 toast。

### 5.3 Hover、键盘焦点与命中区域

- 可点击内容必须有可发现的 hover/pressed 反馈。鼠标 hover、键盘焦点、菜单
  展开和持久选中不是同一种状态，不得共用一个布尔值混为一谈。
- 鼠标点击后不得残留无语义的黑框；修复时不能顺手禁用键盘焦点指示。键盘
  Tab/方向键/Enter/Escape 路径必须仍然可达、可见、可退出。
- 图标视觉尺寸与真实点击区域必须分开定义。常规桌面图标可使用约 16–18
  logical px、约 32px 点击区域作为项目起点；经确认的紧凑控件可以不同，但
  必须验证实际命中，不得拿截图物理像素当逻辑尺寸或声称这些值是普遍标准。
- 标签控件应让整块标签区域（含留白）可点击，而非只有文字附近响应。
  拖拽分隔线的可见宽度与命中宽度分离；释放鼠标、取消或离开有效生命周期后
  必须结束拖动，且拖动控件不得误触发正文选区。
- hover 弹出的工具栏必须允许指针从内容移动到工具栏，不能一离开公式/代码
  就关闭导致无法点击；同时支持适用的键盘操作方式。

### 5.4 颜色、布局与组件默认值

- 使用语义角色区分正文、次级文字、代码底色、hover、selected、focus、危险和
  成功。不能因为变量叫 `accent` 就用于选中下划线；必须核对当前主题中的实际
  对比度和状态含义。
- 原型已获确认时，落地应对照其颜色角色、位置、比例、留白和反馈，而不只是
  复制控件名称。必须检查第三方组件的内部字体、图标尺寸、焦点边框、菜单和
  搜索框是否覆盖应用层设定；外层 `.xsmall()` 等调用不构成视觉一致性证明。
- 搜索输入、菜单行和触发标签的尺寸按各自职责控制。语言选择标签不能因缩小
  自己而把弹出菜单的搜索框一起压矮；文本不得以字符数粗估宽度而截掉 CJK 标签。
- 不靠扩大字号代替留白；不靠不可见/超低对比度解决视觉噪声。正常、浅色、
  深色、高 DPI、窄窗口以及键盘焦点状态都要检查。
- 几何（圆角/间隙/分隔线）、配色、透明度/模糊效果与窗口全屏分别归属明确
  状态。文档专注模式不得调用系统全屏；隐藏侧栏后对应的标题栏色块、选中态
  和恢复入口也必须同步，不留半套旧界面。
- 主题候选需附可追溯来源、真实预览和适用范围，由用户审批后落地。社区采用
  情况与作者自述、个人审美判断要分开记录，不自行新增“凑数”主题。

### 5.5 数学公式的交互合同

- 公式 hover/focus 提供轻量操作：**复制 LaTeX** 与 **复制公式图片**。操作必须
  指向当前公式，不能复制整段文档、邻近公式或整窗截图。
- 默认 LaTeX 复制保留原始公式体；是否携带 `$`/`$$` 等定界符必须明确，不得
  悄悄使用标准化/裁剪后的显示文本替代源公式。
- 行内公式的基线、段落高度和换行不得因工具栏出现改变；显示公式同样不能
  因 hover 额外撑出操作栏。相邻公式要有独立身份与操作目标。
- 图片导出应复用有界渲染资源，明确格式、透明背景/文字色、像素尺寸与缩放；
  必须正确处理 BGRA/RGBA、DPI 和附加字体回补文字，不能产生颜色错误或缺字图
  后仍显示成功。未就绪/不支持的导出须解释或禁用，不输出不完整图片。
- hover 本身不得触发无界重编译、增加常驻整图副本或绕过科学渲染内存预算。
  图片编码等昂贵工作不得阻塞界面线程；取消/关闭视图需释放相关任务与资源。
- 文本复制与图片复制都遵循 5.2 的结果反馈规则，且应分别提供可识别名称。

### 5.6 验收要求与证据边界

- 交互测试要点击真实布局与控件，覆盖点击留白、hover 显示、键盘路径、重复
  点击、成功恢复、失败/取消以及视图销毁；不能只断言一个回调或布尔函数。
- 布局测试核对真实命中范围、工具栏不占位、选中线可见、长行/窄窗口以及不误选
  正文。涉及异步反馈时使用可控时钟，避免靠真实长 sleep 得到脆弱测试。
- 复制 LaTeX 核对实际剪贴板文本；图片核对内容完整性、通道顺序、尺寸和预算。
  需要平台实测的路径，在没有实测时必须明确标记。
- 对照批准原型保存关键状态截图：默认、hover、展开/搜索、复制完成、恢复及
  专注模式进入/退出。编译通过或“状态测试通过”不能冒充视觉验收通过。
- 这是新增/修改 UI 的评审要求，不声称现有 UI 全部达标，也不声称仅写入文档
  就已经启用自动 UI 门禁。现有待修差异要列入验收记录，不能用新规范掩盖。

参考：Nielsen 的 [Visibility of system status](https://www.nngroup.com/articles/visibility-system-status/)
与 [10 usability heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/)，
以及 W3C 的 [Focus Visible](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html)
和 [Target Size (Minimum)](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html)。
Web 可访问性标准与桌面逻辑尺寸不能直接画等号；具体项目数值仍须实测。

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
