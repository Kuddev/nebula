# Linux / macOS Preview 发布检查表

更新：2026-09-05。本文描述已经接入的代码和门禁，不代表已完成原生发布。

## 1. 不需要本地 Mac 的发布入口

1. 将本次要发布的代码、资源、测试、`packaging/` 和 workflow **按具体路径**提交并推送。
   当前工作区存在其他修改和未跟踪文件，不要使用全量暂存、清理或重置。
   `workflow_dispatch` 工作流需要先存在于默认分支，之后才能在 Actions 中选择待发布分支。
2. GitHub Actions → **Cross-platform Preview packages** → **Run workflow**。
   首轮保留 `publish=false`、`macos_signing=adhoc`。`preview_id` 可以留空（run/attempt 编号），
   或填 `20260905.1` 一类不超过 32 字符的唯一标识。
3. 等待 Linux、Mac Apple Silicon、Mac Intel、Windows 对照和聚合全部成功。
   下载 `NebulaTerminal-v<version>-preview.<id>` artifact；分别查看各平台的
   `preview-evidence-*`，包括 runtime 报告和 Mac 安装启动日志/截图。
4. 先分发 artifact 给预览测试者，完成第 4 节的人工检查。公开发布前，按第 1.1 节确定新的唯一 id，
   提交同步的 Changelog 和 Release Note；只写已有代码、构建或运行证据的变化。
   然后选择这份已审阅源码提交，显式填写相同 `preview_id`，发起 `publish=true`。
   工作流会先校验说明，再重新构建并重新验收，不能跳过构建或复用旧包。
5. 成功后生成 `preview-v<version>-<id>` prerelease，不设置为 latest，不覆盖已有标签或 Release。
   工作流重新读取远端标题、正文、标签指向、文件名、空 label、状态、大小与 SHA256。
   任一步失败都要调查日志，不能通过重用旧包或伪造报告绕过。
6. 从最终 artifact 取 `PREVIEW_NOTES.md`，原样同步到该 Preview 的独立 Release Note，补上真实 SHA256，
   再提交这份纯文档修订；正文必须与 GitHub Release 一致。不要移动对应二进制的 Preview 标签。

本次改动没有执行提交、推送、创建标签或发布；默认行为只是构建和上传 Actions artifact。

### 1.1 公开发布说明的预构建校验

`publish=false` 可以使用自动编号和生成的验收摘要；它不等于正式发布说明。
`publish=true` 必须显式选择 id，并在构建提交中准备
`docs/release-notes/v<version>-preview.<id>.md` 及 `CHANGELOG.md` 同版本条目。

- 独立说明标题为 `# Nebula Terminal <version> Preview <id>`，依次包含 `## English`、`## 中文`、
  `## Contributors`、`## SHA256`。沿用 `Added / Fixed / Improved` 与 `新增 / 修复 / 改进` 对称分类。
- Changelog 标题为 `## <version>-preview.<id> - YYYY-MM-DD`；完整复制独立说明中 English 到
  中文末尾的内容，只将这些内容的标题加深一级。校验忽略这一层标题差异，正文必须一致。
- 在 Contributors 区域写入 `macOS signing / macOS 签名: \`adhoc\`` 或
  `macOS signing / macOS 签名: \`developer-id\``，与工作流选择一致；中英文均准确披露签名限制。
- SHA256 区域构建前只写 `<!-- PREVIEW_SHA256 -->`；聚合在原生证据通过后，用最终资产校验和
  替换该占位符，绝不接受旧 checksum。公开发布使用审阅后的正文，不用自动摘要覆盖用户说明。
- 缺失说明、Changelog 不一致、双语分类不对称或签名模式不符，都会在正式构建前失败。

## 2. 产物与自动验收范围

| 平台 | 产物 | 原生自动验收 |
| --- | --- | --- |
| Linux x86_64 / Ubuntu 22.04 | AppImage、tar.gz、deb | workspace 单测、SSH 弹窗测试、Bash/Zsh PTY 测试；最终 AppImage 的 X11/Wayland conformance；安装 deb 后的 X11 conformance |
| macOS Apple Silicon | aarch64 DMG | macOS 15 原生构建/单测、SSH 弹窗测试、Zsh PTY 测试、签名/架构检查、DMG 内应用 conformance、安装副本的 LaunchServices 启动 |
| macOS Intel | x86_64 DMG | 同上，在独立 Intel runner 执行 |
| Windows x86_64 | 只作对照，不发布 Windows 包 | 同一提交的 workspace/SSH 弹窗测试与 Runtime API conformance |

聚合必须拿到 **6 份 runtime 报告 + 2 份 Mac 安装启动报告**，核对来源提交和平台，
验证公共不变量，并在白名单外逐字段比较。缺少报告、失败或 Mac GUI 启动没有 UTF-8 locale /
home cwd 都会阻止聚合。PR 和 main push 也执行门禁，但只有显式 dispatch 可以取得发布权限。

Mac 安装启动用 `ditto` 复制最终 DMG 中的 `.app`，通过 `open -n -W -a` 启动，
不是只直接执行 `Contents/MacOS/nebula`。测试配置独立，清理只针对这份唯一临时安装路径的进程；
不会按进程名结束用户的其他 Nebula。截图作为人工审核材料，不冒充像素一致性测试。

Linux GLIBC 依赖上限为 2.35；macOS 编译 deployment target 为 14.0，但原生 runner 是 15。
**不能把 macOS 14 deployment target 写成 macOS 14 已实测。**

打包脚本检查二进制新鲜度、GPUI 标识和版本。Mac 拒绝未打包的非系统动态库；Linux 检查真实
动态库依赖并由 `dpkg-shlibdeps` 生成 Debian 依赖。归档与安装包必须来自同一提交的全新构建。

## 3. 两种 Mac 签名模式

- `adhoc`：无需 Apple 账号即可生成预览 DMG，但不是 Developer ID 签名，也没有 Apple 公证。
  下载后的 Gatekeeper 体验不同于正式签名应用，必须在说明中披露；不提供关闭全局安全机制的指令。
- `developer-id`：准备以下 Actions secrets；工作流只在显式选择此模式时导入临时 keychain。
  包装使用 hardened runtime 与 timestamp，提交最终 DMG 给 `notarytool`，只有 `Accepted`
  才附加和验证票据并做 `spctl` 检查。缺少凭据、签名或公证失败都终止，绝不降级。

| Secret | 内容 |
| --- | --- |
| `APPLE_CERTIFICATE_BASE64` | Developer ID Application 证书及私钥的 `.p12`，base64 编码 |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 密码 |
| `APPLE_SIGNING_IDENTITY` | 完整 `Developer ID Application: ...` 身份名称 |
| `APPLE_API_KEY_BASE64` | Apple 公证 API `.p8` 密钥，base64 编码 |
| `APPLE_API_KEY_ID` | API key id |
| `APPLE_API_ISSUER` | API issuer id |

不要把这些内容写进仓库、报告或日志。两个架构 runner 分别完成签名/公证；临时凭据在 always
清理步骤删除。尚未提供上述凭据时，只能验证 ad-hoc 路线，不能宣称签名发布已完成。

## 4. 仍需原生人工检查

这些检查适合让预览测试者完成，并记录**版本、提交、系统版本、架构和截图/日志**：

- 从浏览器下载 DMG，保留下载隔离属性，拖到 Applications 后首次打开。CI 的本地产物安装
  不等于真实下载的 Gatekeeper 测试；分别核对已公证和 ad-hoc 包的说明。
- 中文输入法的组合态、候选框、回车确认；Option/Alt、Cmd+C/V、Ctrl+C、选区和多行粘贴。
  `cjk_roundtrip` 只是 UTF-8 Runtime API 输入，不是输入法测试。
- Retina/外接屏、系统缩放、深浅色、字体/Nerd Font 图标、通知授权与后台通知。
- SSH：陌生主机取消不写 known_hosts；明确确认后连接；已保存主机指纹改变时必须拒绝；
  密码、MFA、加密私钥、重连，以及 SFTP 列目录/上传/下载。不要为了自动化新增信任绕过接口。
- 凭据：Mac Keychain 允许/拒绝访问；Linux 有/无 `libsecret-tools`、keyring 锁定/未启动。
  无可用存储时仍可手动输入本次连接所需秘密；保存失败必须可见，不得提示已经保存。
- Mac 默认 Zsh 和显式选 Zsh 都加载用户登录配置；测试自定义 ZDOTDIR。Linux Bash 保留
  `.bashrc`、PROMPT_COMMAND 和已有 DEBUG trap。自定义 `-c` / rcfile 不注入。

`ssh_loop` 当前仍明确 skipped，因为 Runtime API 没有 `ssh.open`。独立 SSH 确认通道单测
及 GPUI 弹窗测试不能代替完整 SSH/SFTP 联调；未来 API 增加 `ssh.open` 后，未实现的 case
将直接失败，而不继续静默 skip。

## 5. Preview 的明确边界

- Linux/macOS 暂不启用系统托盘、关窗隐藏驻留、全局快速终端热键、系统提示音、自动安装更新、
  自动本地 AI hook 配置。最后窗口关闭就退出；没有假装可用的空实现开关。
- 系统通知后端已经接入，但投递依赖桌面服务、应用身份和系统权限，不保证每次都出现横幅。
- OSC 7 / 133 注入覆盖 Zsh 与默认 Linux Bash。Mac Bash 保留原生登录启动，不强改其配置；
  Fish/Nushell/自定义 shell 不在此注入范围。旧 Bash 已有 DEBUG trap 时不覆盖它，命令边界
  能力可能降低；不以破坏用户 hook 换取表面一致。
- 五件套和 SHA256 完整只代表产物结构通过。真正原生 Actions 成功之前，不称为“Mac/Linux 已验证可用”，
  更不能升级为与 Windows 完全等价的 Stable 承诺。
