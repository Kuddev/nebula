# Pebrel internationalization / 多语言架构

## Scope / 范围

The project uses the Pebrel name while retaining the `Kuddev/nebula` GitHub
repository and existing CLI, configuration, installer, and update identifiers.
Renaming those compatibility interfaces is a separate migration, not a text replacement.

项目名称采用 Pebrel，GitHub 仓库保留 `Kuddev/nebula`。CLI、配置、安装及更新标识
属于兼容接口，不随展示名称机械替换。

The first language set is English, Simplified Chinese, Traditional Chinese,
French, German, Spanish, Brazilian Portuguese, Italian, Russian, Japanese, and
Korean. The picker displays each language's native name. POSIX locale suffixes,
regional variants, and Chinese Hans/Hant scripts are negotiated at language resolution.
Terminal subprocess output and environment variables are not translated or overridden.

首批语言为英语、简体中文、繁体中文、法语、德语、西班牙语、巴西葡萄牙语、意大利语、
俄语、日语及韩语。选择器使用语言自称，并在解析语言时处理 POSIX 后缀、地区变体及
中文 Hans/Hant。不会翻译终端子进程的输出，也不会覆盖它们的语言环境变量。

The initial catalogs cover navigation, common actions, appearance labels, and
network controls in all eleven languages. French also covers all existing
catalog-based diagnostics. Many older inline bilingual descriptions are not yet
migrated; they intentionally fall back to English. This is not a claim of complete
translation of every screen, configuration template, or installer language.

初版词典覆盖全部十一种语言的导航、常用操作、外观标签与网络控件；法语同时覆盖原有
词典中的诊断文案。大量旧的内联双语长说明尚未迁移，明确回退英文，不宣称所有页面、
配置模板和安装向导已经完成全量翻译。后续翻译应经过母语审校。

## Runtime contract / 运行时合同

- `nebula_settings/src/language.rs` owns the supported-language registry,
  native names, saved values, and locale negotiation. Both shells use it.
- `nebula_app/build/i18n.rs` validates JSON and generates static Rust data at
  compile time. Runtime lookup never parses a JSON file, constructs a map,
  reads a file, takes a lock, or allocates a string.
- `UiLanguage::text(Message::...)` uses a typed identifier and direct table
  indexing. Use this for new UI code; missing identifiers fail compilation.
- `tr("stable.id")` is a compatibility interface using a generated string
  match. Its fallback is target language → English → original key.
- Existing `pick(Chinese, English)` remains a migration bridge: Chinese and
  English retain their immediate branches; other languages use an unambiguous
  catalog match or English. Ambiguous English phrases do not borrow a translation
  from the wrong context. New context-sensitive messages must use typed ids.
- Formatting is a separate path that builds a `String` only when arguments
  are needed. Values are inserted literally, not recursively interpreted as
  placeholders. Missing arguments stay visible; `{{` and `}}` escape braces.
- GPUI keeps the resolved language in application state. System locale
  detection is outside text rendering. The pinned component library has a
  smaller locale set; unsupported widget locales explicitly use English.

核心原则：运行时只读编译好的静态表；新代码使用类型化 ID 直接索引；只有带参数的文案
才创建字符串。旧 `tr` 和 `pick` 是渐进迁移入口，不应作为继续堆积双语硬编码的理由。
组件库的语言覆盖与自有文案分开记录，不支持的组件语言明确使用英文，避免假装全量覆盖。

## Add a language / 新增语言

1. Add one row to the `languages!` registry in `nebula_settings/src/language.rs`.
   Saved values are canonical locale tags; do not change existing tags or order.
2. Add `nebula_app/i18n/<locale>.json`. Keep the same message ids and named
   placeholders as English. A partial catalog is allowed and falls back to English.
   Do not copy English entries just to inflate the translation coverage count.
3. Set the component locale to one actually supplied by the pinned component
   library, or use `en`. Do not claim right-to-left layout support without testing it.
4. Run the catalog, preference, UI-switching, and allocation tests. Review
   long labels, font coverage, keyboard access, and the real UI with a native speaker.

新增语言只需注册表一行及对应 JSON；选择器、语言枚举和运行时表由同一注册表生成，
无需在所有页面添加分支。构建会拒绝重复 ID、空字符串、未知 ID、非法叶子类型和占位符
不一致；简中与英文基础词典必须完整对齐，其他语言可渐进补齐。

## Module boundaries / 解耦边界

- `src/i18n/`: renderer-independent lookup, locale resolution, formatting, and tests.
- `src/i18n/outcomes.rs`: presentation adapters for provider/proxy results; it is
  compiled separately from the pure lookup core and does not move I/O into translation.
- `settings_pane/navigation.rs`: route, navigation, and display metadata.
- `settings_pane/localization.rs`: select values and translated labels.
- `settings_pane/status.rs`: semantic operation results rendered in the active language.
- `settings_pane/shell_picker.rs`: shell selector items and import boundary.
- `settings_pane/initialization.rs`: entity construction and event subscriptions.
- `settings_pane.rs`: pane state, orchestration, and rendering.

这轮优先拆分语言与设置链路，不改变持久化格式、事件含义和设置生效时机。其他存量超大
文件仍由行数预算约束，应在对应功能改动时按职责拆分，而不是为了行数任意切片。

## Checks / 验证

```text
cargo test -p nebula-settings
cargo test --manifest-path tools/i18n-contract/Cargo.toml --locked
cargo test -p nebula --test i18n_contract
cargo test -p nebula --bin nebula --features gpui-shell i18n::
cargo test -p nebula --bin nebula --features gpui-shell gpui_shell::settings_pane::tests::
cargo test -p nebula --test file_line_budget
cargo test -p nebula --test i18n_contract --release -- --ignored --nocapture
```

The allocation contract checks both the first and repeated lookup across every
language. The timing benchmark is informational, not a machine-dependent CI speed
threshold. The initial translated UTF-8 payload has a 256 KiB budget; string table
pointers and generated code are additional binary overhead, not included in that count.

零分配测试覆盖首次与重复查词；微基准只报告实测值，不把某台机器的耗时写死成 CI 阈值。
初版 UTF-8 翻译文本预算为 256 KiB；指针表与生成代码是额外开销，不冒充完整二进制体积。

The independent contract workspace compiles the production generator and lookup
files without renderer dependencies. The `architecture-contracts` PR job executes
it, while real GPUI/platform checks and native-language layout review remain separate.
Language or ownership changes also follow the [engineering contracts](project-constraints.md).
