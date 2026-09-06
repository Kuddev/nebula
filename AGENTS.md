# Nebula 项目代理指引

## 强制工程规范

- 修改前阅读 `CONTRIBUTING.md`、`docs/architecture.md` 和 `docs/project-constraints.md`；新 UI 文案同时遵循 `docs/internationalization.md`。
- 行数预算与依赖方向由 `architecture/` 声明，使用 `python3 scripts/check_architecture.py --base <PR-base-commit>` 验证；800 行仅提示，2000 行是既有防灾上限。
- 禁止靠抬高预算、删测试、压缩格式、机械分片或静默排除目录让门禁通过。规范存在缺陷时，先给可复现反例，补正反测试并记录经维护者审查的规则修订。
- 核心规则保持单一权威实现；按职责和生命周期拆分，不因文件夹名称或历史模块别名推断耦合。新增长期依赖、跨层接口、持久化和线程模型变更须给出设计依据。
- 普通翻译查询维持已测零分配合同；参数格式化和冷路径另行评估。不能把单机微基准写成普遍性能保证。
- 不得声称仅加入 workflow 或 `CODEOWNERS` 就已启用 GitHub 强制保护；服务端设置及负例 PR 验证须另行获准并核验。

## 发布任务的持久化上下文

- 处理版本号、构建、打包、自动更新、GitHub Release、Changelog 或 Release Note 前，必须先完整阅读 [`.agents/memory.md`](.agents/memory.md) 的发布章节。
- `.agents/memory.md` 是经过实际发布核验的项目记忆，不是临时推理草稿。只有在代码、脚本、GitHub 元数据或真实运行结果已经核实后才能更新；过时结论必须直接修正并注明新的核验日期。
- 发布相关命令必须使用 UTF-8。工作区可能包含用户的未跟踪探针、构建目录和截图，只能显式暂存本次文件，不得用清理、全量暂存或重置命令处理它们。

## Release Note 原则

1. 发布说明只写已经实现并有代码、构建或真实运行证据的用户可感知变化。调试探针、测试用强制弹窗、内部重构和未经验证的推断不得写成已交付功能。
2. 沿用 `v1.3.0` 的双语结构：English 的 `Added / Fixed / Improved`、中文的 `新增 / 修复 / 改进`、`Contributors` 和最终资产 `SHA256`。中英文必须表达同一事实，不能一侧增加能力或承诺。
3. 同一发布的 `CHANGELOG.md`、`docs/release-notes/vX.Y.Z.md` 和 GitHub Release 正文必须同步。应在正式构建前完成 Changelog，因为 ZIP 和安装器会把它打进产物。
4. Issue 引用必须先读取对应 Issue 的标题、正文、状态和必要评论，再挂到最准确的一条说明上。禁止按编号猜测、复用旧版本链接，或把宽泛需求说成逐项 Bug 报告。
5. 英文引用使用 `Addresses [#N](https://github.com/Kuddev/nebula/issues/N).`，中文使用 `对应 [#N](https://github.com/Kuddev/nebula/issues/N)。`。一个 Issue 涵盖多项紧密修复时，优先只在最具代表性的条目引用一次。
6. 文案以用户结果为主，例如“100% 不透明度不再回到 82%”，不要用内部类型名、辅助函数或提交标题代替行为说明。仍开放或只部分覆盖的问题不得宣称彻底解决。

## 发布安全边界

- 正式发布不得使用 `-SkipBuild` 或 `-AllowStale` 绕过新鲜度检查，也不得把 workspace 默认构建产生的 legacy shell 当作 GPUI 产品包。
- 不得因为某个磁盘空间更大就擅自使用它。构建和输出目录必须位于用户明确允许的路径；当前环境尤其不得操作 `E:\`。
- GitHub 推送使用普通非强制推送。发布后若只有说明文档修订，可同步分支和 Release 正文，但不得擅自移动已经对应二进制的版本标签。
- 发布结束前必须核验 Release 标题、标签、正文、资产真实文件名、空资产标签、大小、SHA256、分支与版本标签指向；不能只以创建命令返回成功作为完成依据。
