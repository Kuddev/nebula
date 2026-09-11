# 科学渲染性能设计与最终受限验收

**状态：** 自动化验证已记录；GUI 人工覆盖仍有明确范围 \
**日期：** 2026-09-08 \
**范围：** Pebrel 自有 crate 的数学、SMILES、Markdown 预览和终端科学覆盖层 \
**不包含：** GPUI/第三方依赖修改、80 个真实 AI、100 MB 全文阅读、GPU/FPS 容量承诺

本文是工程证据记录，不是容量保证。48 MiB 科学资源缓存只约束 Pebrel 自己的缓存计账；它不约束进程 RSS、GPU、普通 Markdown 图片、GPUI 自身缓存或外部 AI 进程。

## 最终实现

ScientificRender 是 App 级共享资源服务，统一处理公式布局、CPU 位图、SMILES SVG 和单帧图像。视图保存 source 和短期图像句柄，资源服务负责去重、后台工作、缓存和 Wakeup。当前边界如下：

| 边界 | 值 | 语义 |
|---|---:|---|
| 科学资源缓存 | 48 MiB、1024 entries | App 级 byte + entry LRU |
| 科学 pending | 64 jobs、4 MiB charge | 入队前去重并拒绝超限 |
| 科学后台任务 | 2 | 公式/分子任务最多同时运行 2 个 |
| 文档工作许可 | 2 | 文件加载和 Outline 解析另有独立 permit |
| 磁盘读取 | 8 MiB + 1 probe byte | 截断或编码错误内容只读 |
| 富预览总输入 | 512 KiB | AST 前限制 Outline 输入 |
| 单块富预览 | 32 KiB | 超限块按 literal source 展示 |
| 单公式位图 | 8192 px 边长、24 MiB | 超限或不可读时源码回退 |

RenderCache 的命中更新 touched 时间，按最近最少使用逐项淘汰；单项过大时先拒绝，不驱逐现有热资源。FormulaKey 包含 source、display、verbatim、字号和像素比例；图像 key 还包含缩放和 RGBA。分子 key 包含 source 和 dark theme。

缓存 charge 的关键修正是：Layout 使用 MathLayout::allocated_bytes()；Image 和 Molecule 使用 RenderImage::as_bytes(0) 的实际像素长度。逻辑 480 × 240 geometry 不能推断 GPUI 图像内存。此前一次 GPUI 回归显示实际 1,843,200 bytes，旧的 480 × 240 × 4 = 460,800 估算低估了 2 倍边长造成的 4 倍像素。旧的 7.03 MiB 级分子像素估算同样作废；按当前实际像素计量，16 张对应图像约为 28.125 MiB。普通 Markdown 图片、图片临时文件、GPUI cache 和外部 AI 不在 48 MiB 预算内。

数学 CPU 合成位于 math/bitmap.rs，GPUI 只包装 MathBitmap 的像素与几何。布局只处理从 Math root 可达节点，跳过解析重写留下的孤儿节点。overbar 使用字体 MATH 表的 vertical gap 和 rule thickness 生成规则线，不依赖 U+203E 字形。boxed/tag 有兼容降级；原始文档矩阵行末只有单个反斜杠时回退原始 source，terminal 传输层有独立行分隔修复。

Markdown Outline 在 AST 前限制 512 KiB，并限制脚注/引用展开总量；超预算时设置 previewLimited，富预览显示“已缩短”的提示，用户可切换 Edit 阅读已经加载的 source。单块超过 32 KiB 或被截断的科学块按 literal source 展示。虚拟列表按需创建 TextViewState，并保留已创建状态。Ctrl+A 设置 whole-document selection；之后懒创建的新块立即继承选中状态，复制 source 不要求先解析所有块。

Wakeup 在入队前合并；CommandStart、AiHookEnvelope、CommandDone、Exit 等语义事件保持顺序。文档加载/Outline 解析 permit 与科学后台任务计数器分开。

## 自动化验证

最终全量产品日志 All-20260908-083933.stdout.log：

**1246 passed / 0 failed / 8 ignored / 2 filtered out**。两项过滤是既有凭据用例。该轮也实际构建了最新 GPUI 产品 binary。

Outline 引用展开预算补丁后的核心日志 core-tests-final.log：

**63 passed / 0 failed / 1 ignored**。覆盖 chemistry、document_io、math parser/layout/rasterizer/bitmap、Outline 预算与引用展开、RenderCache、scientific_corpus；忽略项是 informational corpus benchmark。该测试为独立核心路径，未链接 GPUI。

其他独立结果：

| 日志 | 结果 |
|---|---|
| Settings-20260908-085121 | 41 passed |
| I18n-20260908-085253 | 14 passed / 1 ignored |
| Integration-20260908-085625 | file budget 2 passed；i18n 14 passed / 1 ignored |

历史 check-3.log 只作为较早完整 GPUI check 的证据；之后数学、预览和真实像素预算又有修订，最终判断以本节全量日志和后续人工复核为准。上一轮失败暴露的 geometry 计量已经改为 as_bytes 计量；最终全量日志已为 0 failed。

最终格式化后的补充 Build 已通过：tmp/scientific-stress-20260908/Build-20260908-092256.stderr.log 记录 Finished dev profile，verify 返回 EXIT_CODE=0，耗时 2m51s。Integration 阶段也已实际构建 GPUI 产品 binary。

## 核心 CPU benchmark

tmp/scientific-stress-20260908/benchmark.log 是 O1 独立核心二进制的 dev benchmark，缓存为 dev 依赖；不是 Release，不链接 GPUI，不测 GPU/FPS。Markdown parse 每个文档重复 10 次；公式和分子使用各自样例分布。n=10 的 P95/P99 都是最大样本，不能当稳健尾延迟。本文不与旧 O3 探针比较。

| 文档 | 阶段 | n | median ms | P95 ms | P99 ms |
|---|---|---:|---:|---:|---:|
| math | Markdown parse | 10 | 8.2548 | 9.7160 | 9.7160 |
| math | compile | 121 | 0.0483 | 0.1313 | 0.2859 |
| math | bitmap | 121 | 0.7571 | 1.6388 | 2.6597 |
| chemistry | Markdown parse | 10 | 9.1201 | 10.3631 | 10.3631 |
| chemistry | compile | 55 | 0.0397 | 0.0796 | 0.0994 |
| chemistry | bitmap | 55 | 0.6739 | 1.0073 | 1.1081 |
| chemistry | molecule SVG | 61 | 0.0344 | 0.1569 | 0.2687 |
| biology | Markdown parse | 10 | 15.9760 | 17.8119 | 17.8119 |
| biology | compile | 74 | 0.0714 | 0.1221 | 0.1722 |
| biology | bitmap | 74 | 0.9691 | 1.2482 | 1.4304 |
| biology | molecule SVG | 67 | 0.0383 | 0.1656 | 0.1884 |
| paper | Markdown parse | 10 | 17.1089 | 17.6822 | 17.6822 |
| paper | compile | 153 | 0.0176 | 0.1491 | 0.2150 |
| paper | bitmap | 153 | 0.1806 | 2.1432 | 3.2010 |
| paper | molecule SVG | 12 | 0.0231 | 0.4599 | 0.4599 |

## Native SVG 与 UI 观察

Native SVG benchmark 日志 native-svg.stderr.log：

| 文档 | n | median ms | P95 ms | P99 ms |
|---|---:|---:|---:|---:|
| chemistry | 61 | 3.5352 | 10.2605 | 13.6102 |
| biology | 67 | 8.7111 | 14.5232 | 18.5305 |
| paper | 12 | 10.4214 | 13.8351 | 13.8351 |

这是已预热字体的开发产品测试二进制中的 CPU SVG 栅格化；样例不是同一输入重复，不包含 GPU 绘制或 FPS。独立小测试进程 native-svg-process.json 的采样峰值工作集为 **18,984,960 bytes**，不能外推为桌面 App 或 80 AI 的占用。原生 GPUI SVG benchmark 尚未变成 GPU/FPS 证据。

native/window-results.json 记录 9 张截图：数学、化学、论文各自执行 3 次尺寸操作，实际尺寸为 1280×900 和受最小窗口限制的 1162×821，再恢复到 1280×900。一个 App 加空 cmd，窗口已关闭，无 stderr 报错。工作集约 154–161 MiB，观测峰值约 164 MiB。截图复核确认数学公式渲染成功；化学和论文只看到了首页标题/介绍，未滚动完整文档，不能声称所有分子和全文均人工检查。该记录没有实际鼠标拖选、80 真实 AI 或 100 MB 全文。

## 资源与生命周期核对

公式请求先按 FormulaKey 查 Layout 或 Image。未命中时，pending HashSet 防止相同 key 重复排队；charge 同时计入 key、布局或真实像素。后台构建结束后先写缓存，再合并一次 UI refresh。单条公式超边长、超 24 MiB、无法布局或无法栅格化时返回源码回退；失败结果可被负缓存，避免每帧重复失败工作。

SMILES 请求在 Pebrel chemistry 中解析并生成 SVG，再由 SvgRenderer 生成单帧 RenderImage。MoleculeView 和终端覆盖层都保留 source，图片未完成时显示加载或跳过覆盖，失败时显示 source/invalid 状态。dark theme 会形成独立 key。这里的单帧图像只说明资源生命周期，不说明 GPU 纹理已经归还。

Markdown 文件读取、Outline 和科学资源任务分别受 8 MiB 文件上限、512 KiB Outline 上限、32 KiB block 上限、2 个 document permits 和 2 个科学后台任务约束。引用或脚注展开超过额外预算时，预览保持 source fallback 并标记 previewLimited；这和磁盘只读取 8 MiB 是两个不同层级。

虚拟列表只为可见 block 创建 TextViewState。Ctrl+A 先标记 whole-document selection，再遍历已创建状态；后续可见块创建后立即补选。Copy 在 whole-document 状态下直接复制 InputState 的完整 source，所以远离视口的块无需解析。鼠标拖选仍必须观察跨块边界和真实系统选区。

普通 Markdown 图片、动画图片转码、GPUI 自身图片缓存、字体缓存、SVG renderer 内存和外部 AI 都不从科学 RenderCache 预算扣除。科学缓存的 used_bytes、应用工作集、GPU 分配和子进程工作集必须在报告中分栏。

## 证据矩阵

| 层级 | 输入/日志 | 已确认事实 | 明确未覆盖 |
|---|---|---|---|
| 核心单元 | core-tests-final.log | 63/0/1；数学、chemistry、Outline 引用预算、LRU、文档前缀读取 | GPUI、窗口、真实输入 |
| 全量产品 | All-20260908-083933.stdout.log | 1246/0/8/2；最新产品 binary 已构建 | 全文人工浏览、GPU/FPS、真实 AI |
| 设置与集成 | Settings、I18n、Integration 日志 | 设置 41；I18n 14/1；file budget 2、i18n 14/1 | 发布环境长期运行 |
| O1 CPU benchmark | benchmark.log | 四文档 parse/compile/bitmap/SVG 分布和实际毫秒值 | Release、GPUI、GPU、FPS |
| Native SVG | native-svg.stderr.log | chemistry/biology/paper 三组 CPU SVG 栅格化分布 | GPU 绘制和桌面 App 资源 |
| Native UI | native/window-results.json | 9 截图、3 次尺寸操作、2 种实际尺寸、无 stderr、工作集与峰值记录 | 全篇滚动、拖选、80 AI、100 MB |

全量产品测试之前曾暴露 geometry 像素估算错误；最终证据以实际 as_bytes charge 和 1246/0/8/2 日志为准。旧日志中的失败不应被删改，但也不能继续作为当前实现状态。

## 验收顺序

为了复现最终结果，先用四份 docs 语料运行独立核心测试，核对 63 passed、0 failed、1 ignored；再读取 O1 benchmark 表，注意每份 Markdown parse 的 n=10 尾延迟限制。随后运行完整产品测试并保存 All 日志，确认 1246 passed、0 failed、8 ignored、2 filtered out。

在 GUI 中逐个打开 math、chemistry 和 paper 文档，执行 3 次尺寸操作，涉及 1280×900 和最小 1162×821 两种实际尺寸，再恢复 1280×900。确认数学首屏、化学/论文的首屏 source/标题、previewLimited 提示、缺图或坏公式回退，然后继续人工补做完整滚动、跨文字拖选、Ctrl+A 后滚动到新 block、复制 source 和 tab 返回。现有 9 张截图只覆盖前述有限观察，不能替代这些后续动作。

缓存观察应同时记录 entry count、used_bytes、真实 as_bytes、pending count、pending charge、running count、LRU 淘汰和 Weak 引用存活像素。遇到逻辑 geometry 与真实像素不一致时，以 RenderImage::as_bytes(0) 为准，不能恢复旧的 width × height × 4 估算。

## 复核不变量

| 不变量 | 复核方式 | 失败时的正确解释 |
|---|---|---|
| 科学 cache 不超过 48 MiB/1024 entries | 记录实际 charge、条目数和逐项淘汰 | 只说明 Pebrel cache 计账，不说明 RSS/GPU |
| pending 不超过 64/4 MiB，科学任务不超过 2 | 记录 admission、拒绝、start/finish 和峰值 | 说明队列有界，不说明调度公平或输入流畅 |
| document permit 不超过 2 | 同时触发多份加载和 Outline 解析 | 说明文档工作受限，不等于总线程上限 |
| Image/Molecule 按真实 as_bytes 计量 | 对每个保留 Weak 升级并求像素长度 | 不用逻辑 geometry 代替实际像素 |
| 公式只排版 root 可达节点 | 使用孤儿 accent、overbar、坏矩阵和 boxed/tag | 失败必须源码回退，不能猜测缺失行 |
| 大文档不继续展开富预览 | 注入超 512 KiB、超 32 KiB block 和引用/脚注展开 | 显示 previewLimited/source，不代表完整文件已读 |
| Ctrl+A 不依赖所有块物化 | Ctrl+A 后滚动到尚未创建的 block 再复制 | 新块继承选中状态，复制仍取完整 source |
| Wakeup 合并不丢语义事件 | flood Wakeup 后检查 Command/AiHook/Done 顺序 | 只能合并 Wakeup，不能合并业务边沿 |

### 回退路径

公式回退包括解析失败、未知 presentation command、单反斜杠矩阵行末、超边长、超像素和不可读字号。分子回退包括非法 SMILES、超原子/超键/超字节、SVG 失败和主题 key 未完成。Markdown 回退包括图片缺失、引用/脚注展开超限、整份 Outline 超 512 KiB 和单 block 超 32 KiB。每类回退都应保留可复制 source，不能复用其他 key 的旧图。

### 结果书写规则

自动化日志给出的是测试二进制和输入集合的结果；benchmark 的 n、预热状态和构建模式必须随数字一起保存。原生 UI 记录给出的是有限截图和工作集观察；它不覆盖全文。任何“全部科学块通过”“所有图片正确”“输入延迟达标”“80 AI 平滑”“支持 100 MB”都需要额外证据，不得由本文件推断。

## 复现与未覆盖项

复现输入是四份维护语料：

1. docs/math-rendering-test.md
2. docs/chemistry-rendering-test.md
3. docs/biology-rendering-test.md
4. docs/scientific-rendering-acceptance-paper.md

复核时保留日志、构建 revision、字体、显示缩放、GPU、viewport 和主题。核心测试应确认 63/0/1；全量测试应确认 1246/0/8/2；再分别检查缓存实际 as_bytes、Outline 预览缩短提示、Ctrl+A 后新块选择和 source 复制。

以下事项仍不由这些结果覆盖：真实鼠标跨文字拖选；完整论文/化学/生物文档逐屏人工浏览；GPU 绘制与 FPS；桌面 App RSS/GPU 的容量上限；80 个真实 AI 的进程、事件和成本；100 MB 全文 Markdown AST、滚动和保存；发布端到端 P95/P99。普通图片、GPUI 自身 cache 和外部 AI 需要独立资源报告。不能用 48 MiB 科学 cache 或 18,984,960 bytes 独立进程峰值替代这些容量数据。

本文件记录的是最终可复核的工程证据和边界。用户在主代理提交后仍需人工补齐拖选、全篇滚动、缺图/回退和真实窗口交互记录；这些动作完成前不扩大 GUI 覆盖结论。

## 交接清单

提交前保留 All、core-tests-final、benchmark、native-svg、native-svg-process、native/window-results 和 Build-20260908-092256.stderr.log 原始文件，不用截图代替日志。记录当前源 revision、产品 binary 路径和人工测试日期；若新证据改变像素计量、预览预算或 UI 覆盖范围，只修订本文件对应事实并保留旧日志。
