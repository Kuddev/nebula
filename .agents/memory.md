# Nebula 持久化发布记忆

最后核验：2026-08-25，Nebula Terminal 1.3.1 发布与文档修订。

本文记录已经实际遇到并核实的发布陷阱，以及后续版本可直接执行的 Release Note 与 GitHub Release 规则。它不保存密钥、令牌、临时构建路径或未经验证的猜测。

## 1. 命名规范是接口，不是装饰

以已发布的 `v1.3.0` 和 `v1.3.1` 为准：

| 对象 | 规范 | 示例 |
| --- | --- | --- |
| Git 标签 | `vX.Y.Z` | `v1.3.1` |
| GitHub Release 标题 | `Nebula Terminal X.Y.Z` | `Nebula Terminal 1.3.1` |
| Portable ZIP | `NebulaTerminal-vX.Y.Z-windows-x64.zip` | `NebulaTerminal-v1.3.1-windows-x64.zip` |
| Windows 安装器 | `NebulaTerminal-X.Y.Z-windows-x64-setup.exe` | `NebulaTerminal-1.3.1-windows-x64-setup.exe` |
| Release Note 文件 | `docs/release-notes/vX.Y.Z.md` | `docs/release-notes/v1.3.1.md` |

- ZIP 带 `v`，安装器不带 `v`。不要为了“看起来统一”擅自改成同一种格式。
- 应用内更新会按精确安装器文件名选择资产。发布资产改名不仅影响展示，还可能直接破坏自动更新。
- GitHub CLI 上传资产时，`<path>#<label>` 中的 `#...` 会设置自定义资产标签。GitHub Assets 区域随后优先显示这个 label，造成文件名看起来被改乱。正式上传只传文件路径，不加 `#label`。
- 资产 `label` 的正确值是空字符串。若文件本身、大小和 digest 都正确，只是 label 错了，应通过 GitHub API 清空 label；不要重新上传并损失下载计数或引入重复资产。

## 2. Release Note 内容与结构

### 固定结构

沿用 `docs/release-notes/v1.3.0.md`：

```text
# Nebula Terminal X.Y.Z

## English
### Added
### Fixed
### Improved

## 中文
### 新增
### 修复
### 改进

## Contributors
---
SHA256
```

- 某个分类没有内容时可以省略该分类，但不能让中英文分类不对称。
- `CHANGELOG.md` 的同版本条目与独立 Release Note 必须语义一致；GitHub Release 正文应直接取自独立 Release Note。
- `CHANGELOG.md` 会被 `scripts/package-release.ps1` 和 `scripts/build-installer.ps1` 收进发布包。因此必须先定稿说明，再构建最终二进制和归档。
- `SHA256` 只能在最终文件生成后计算。文件被重新构建或重新打包后，即使版本号相同也必须重新计算，不能沿用旧值。

### 选材与措辞

- 从上一个版本标签到候选发布提交审计用户可感知变化；多个连续提交修复同一问题时合并成一条，不把调试过程写成多个功能。
- 优先陈述结果、触发条件和用户影响。实现细节只在能解释安全性或行为边界时保留，例如安装器会校验大小、PE 文件头与 SHA256。
- `Added` 用于新能力，`Fixed` 用于已存在行为的回归或错误，`Improved` 用于兼容性、可用性或一致性提升。不要为了让三个分类都有内容而错误分类。
- Debug-only 强制更新提醒只是验收探针，正式版本必须移除，Release Note 也不得把它描述成启动时必定弹出的产品行为。
- 自动更新说明必须以真实 GitHub 最新 Release 检测为前提：比较运行包版本，遵守跳过版本和稍后提醒；不得发布写死版本或强制测试弹窗。
- “已修复”必须有相应实现和针对性验证。Issue 仍开放、只覆盖其中一部分或缺少复现验证时，应准确写“改进”“部分处理”或不引用，不能替用户关闭结论。

## 3. Issue 引用规则与 1.3.1 样例

发布前通过 GitHub 读取候选 Issue 的标题、正文、状态和必要评论，再与具体代码和说明逐项映射。仅凭标题相似、路径相关或用户给出的编号都不够。

规范格式：

```markdown
Addresses [#60](https://github.com/Kuddev/nebula/issues/60).
对应 [#60](https://github.com/Kuddev/nebula/issues/60)。
```

`v1.3.1` 已核实映射：

- `#60` 对应终端不透明度设为 100% 后回到 82%。
- `#61` 对应侧边栏可见分隔线与鼠标拖拽命中位置不一致。
- `#64` 对应通用确认弹窗缺少确认操作；说明必须覆盖“确认操作正常显示并执行”，不能只写弹窗尺寸或取消行为。
- `#47` 是“自动更新或使用 scoop/winget 更新”的宽泛且仍开放功能诉求。它可以作为在线更新方向的需求背景，但正文没有逐项要求 GitHub API、右下角提醒、Windows x64 资产或 SHA256；不能把这些实现细节都说成 `#47` 报告的 Bug。

同一 Issue 不要散落重复挂在许多条目上。旧版本的 Issue 链接属于历史记录，也不能因为主题相近而复制到新版本。

## 4. 版本面必须一起更新

发布新版本时核对以下位置，不能只改 `nebula_app/Cargo.toml`：

- 本地发布 crate：`nebula-completions`、`nebula_app`、`nebula_config`、`nebula_config_derive`、`nebula_settings`、`nebula_split`、`nebula_terminal` 的 package version。
- `nebula_app` 内部 path dependency 的版本约束，以及 `nebula_config` / `nebula_config_derive` 互相引用的 dev-dependency 版本。
- `Cargo.lock` 中上述 Nebula 本地包记录。不要全局替换第三方 crate 的同号版本。
- `nebula_app/windows/nebula.rc` 的 `FILEVERSION`、`PRODUCTVERSION`、`FileVersion` 和 `ProductVersion`。
- `scripts/installer.iss` 的 `AppVersion` 与四段式 `NumericVersion`。
- `CHANGELOG.md` 和新的 `docs/release-notes/vX.Y.Z.md`。

不要机械改动：

- `nebula_gpui`、`nebula_hook` 的独立版本，除非该版本本身确实要发布。
- 历史 Release Note、历史 Changelog、第三方依赖版本。
- 测试中用于表达“旧版本/新版本”的固定字符串，除非测试语义要求更新。
- `scripts/package-release.ps1` 的默认值 `unreleased`。

## 5. 构建与打包踩坑

- `cargo build --workspace` 可能把不带 `gpui-shell` 的 legacy `nebula.exe` 留在输出目录。正式脚本先构建 `--workspace --exclude nebula`，最后显式构建 `-p nebula --bin nebula --features gpui-shell`；不要在它之后再用 workspace 默认构建覆盖产品 exe。
- 正式包必须全新构建。`-SkipBuild` 和 `-AllowStale` 只用于脚本自测，不能用于正式 Release。
- Windows 正在运行的 `nebula.exe` 可能锁住输出文件，并把链接失败表现为 `os error 5`。应使用用户允许的独立 `TargetDirectory` 和 `OutputDirectory` 隔离构建，不要结束用户正在使用的实例。
- 不得根据剩余空间擅自选择磁盘。本次用户明确禁止操作 `E:\`；这一约束优先于任何“空间更大”的构建建议。
- 构建脚本会验证 `nebula.exe --help` 包含 `--gpui`、`--version` 与目标版本匹配，并检查二进制不早于源码。不要绕过这些检查。
- Inno Setup 的简体中文翻译来自固定上游提交并校验 SHA256；下载失败或哈希不符应停止发布，不能静默使用未知版本。
- ZIP 和安装器必须从同一个发布提交、同一套新鲜二进制生成。产物生成后核对文件清单、大小、可执行文件头、版本输出和 SHA256。

## 6. Git、GitHub CLI 与发布顺序

- 大规模或核心修改前先提交可回退基线。正式发布提交只包含预期代码、版本和文档；工作区有大量未跟踪探针时必须逐路径 `git add`，禁止 `git add -A`、清理或重置用户文件。
- 受限执行环境中的 GitHub CLI 登录、Issue 或 Release 查询可能返回 `401`、`403` 或共享 IP rate limit，而同一机器的授权宿主环境实际有权限。先在获准的真实网络与凭据上下文复核，再要求用户重新登录，不能把沙箱假阴性当成仓库无权限。
- 使用普通 push，同步目标发布分支和 `main`；禁止 force push，除非用户明确要求且风险已经说明。
- 先确定发布提交并从它构建产物，再创建 `vX.Y.Z` 标签和 GitHub Release。上传资产时不设置自定义 label。
- 发布后若只修订 Release Note：提交并推送文档到相关分支，使用同一 Markdown 更新 GitHub Release 正文；版本标签继续指向实际生成二进制的发布提交。除非用户明确要求，不移动或强制更新标签。

`v1.3.1` 的实际先例：

- 二进制发布提交与标签：`8de58a373c48068ab7af23d100538bcdb38c6810`。
- 后续 Issue 引用文档提交：`c5866feb557559d91e59c2974a3b6c5bc9de1f0e`，同步到 `main` 和 `upgrade/gpui-v1.16.1`。
- 文档补充后没有移动 `v1.3.1` 标签，也没有重传资产；只同步仓库文档和 GitHub Release 正文。

## 7. GitHub Release 最终核验

创建或编辑 Release 后必须重新读取远端数据，逐项确认：

1. Release 为非 draft、非 prerelease，标题为 `Nebula Terminal X.Y.Z`，tag 为 `vX.Y.Z`。
2. Release 正文与 `docs/release-notes/vX.Y.Z.md` 一致，中英文、Issue 链接和 SHA256 均存在。
3. 资产名称完全符合约定，`label` 为空，状态为 uploaded，大小与本地最终文件一致。
4. GitHub 返回的 asset digest、本地 `Get-FileHash` 和 Release Note 中的 SHA256 三者一致。
5. `main`、发布分支和版本标签分别指向预期提交；特别区分“分支上的后续文档提交”和“标签对应的二进制发布提交”。
6. 自动更新实际能识别精确的 Windows x64 安装器资产，测试用强制提醒不在 release build 中。
7. 没有误上传探针、临时目录、旧包或用户文件，也没有操作用户禁止的磁盘和目录。

`v1.3.1` 最终已核实资产：

```text
NebulaTerminal-v1.3.1-windows-x64.zip
SHA256 211A657F2B772AA44A69ED3E7DF20448C79D734217EFDDBB094CF8459FF46216

NebulaTerminal-1.3.1-windows-x64-setup.exe
SHA256 41FCC6B3E09B5E7D0EBF4B7316FC75379EAD4CC5FB74FD7EDA092AE355E27F58
```

两个资产的 GitHub `label` 均为空；Release 标题为 `Nebula Terminal 1.3.1`，标签为 `v1.3.1`。
