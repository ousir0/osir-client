# Client 0.1.9 → 0.1.10 公开更新验收 harness

状态：**prepared-not-dispatched**。这是可评审的外部工具，不是更新验收通过回执。workflow 已登记；r3 绑定在专用控制分支提交，尚未 dispatch 或运行 Client。冻结产品源码未改；本机生产 PID 31517 / proxy 15721 不属于操作范围。

## 验收范围

仅在公开切换完成后使用真实固定 feed：`https://osirclaw.com/api/v1/osir-client/updater/prod.json`。不修改 DNS、hosts、证书信任、TLS 或应用更新端点。独立包读回仅允许精确 RainS3 对象；拒绝重定向，校验大小、SHA-256 和原 0.1.9 内置公钥的 updater 签名。

三个独立 GitHub 原生 GUI job 并行运行，任一失败或 blocked 都不能视为整体通过：

| 平台 | runner | 必须匹配的原生架构 |
| --- | --- | --- |
| Windows x64 | windows-2022 | x86_64 PE |
| Mac ARM | macos-15 | arm64 Mach-O，拒绝 Rosetta |
| Mac Intel | macos-15-intel | x86_64 Mach-O |

实际顺序为：真实旧包部署 → 旧应用初始化 schema 25 并出现原生窗口 → 只停止本 job 的旧 PID → 写入合成供应商、项目、设置和工具配置 → 重开旧应用 → 真实 UI 点击“更新并重启” → 旧 PID 退出 → **应用更新器自动启动**不同的新 PID → 同一安装路径、精确新二进制 hash、0.1.10 版本和可见原生窗口 → 只停止该新 PID → 检查 DB schema 26 与合成数据逐项保留。

点击后没有 harness 启动新应用的后备路径。新候选包的独立下载只验证公开字节，从不手动安装候选包。Windows 仅旧 0.1.9 基线使用静默 NSIS 安装；Mac 仅部署真实已签 updater archive，**不覆盖 DMG 安装器验收**。

Windows 复用原 CDP 真实按钮点击及 native IPC，额外绑定 loopback 监听进程到确切 Client PID 的子进程树；原生窗口只读探针摘自冻结 r2 smoke。Mac 复用 AXPress 和现有原生窗口探针，校验 PID、bundle、路径、架构、签名完整性与版本。Mac 的版本证据为精确二进制 hash 绑定的发行诊断 + 本机 bundle Info.plist，不声称进行了 renderer IPC 版本调用。

## 必需的最终输入；当前均未代填

运行 `node contract.mjs validate <public-spec.json>` 会先校验完整 helper lock 和全部最终 manifest。没有有效的示例 spec，不允许缺字段、null、重复平台、任意 URL、全相同字符占位 hash 或未确认 cutover。

| spec 字段 | 精确要求 |
| --- | --- |
| schemaVersion / fromVersion / toVersion | 1 / 0.1.9 / 0.1.10 |
| sourceCommit | `ec2b05bab6224334e91922cd70c71faf8e9aee89` |
| sourceSha256 | `ca4c28e21069dda1cc948e55e5ef8e1438161d96a42a21c0971a8f8848a04e73` |
| harnessCommit | 最终 reviewed harness 所在的真实 40hex dispatch commit；必须等于 GITHUB_SHA |
| cutover | 仅 confirmed=true；已发生的 confirmedAt UTC ISO 时间；真实 receiptSha256 |
| feed | 固定 url、最终公开原始 JSON 字节的 sha256、精确 size（≤128 KiB） |
| platforms | 恰好 darwin-aarch64、darwin-x86_64、windows-x86_64 |

每个平台项只包含 `platform, manifestPath, manifestSha256, manifestSize, executableSha256, updater`。`updater` 只包含 `objectUrl, feedUrl, sha256, size, signature`，signature 为最终 feed 中原样的 base64 签名。

最终 manifest 必须以原始字节放入 harness commit：

- `final-manifests/darwin-aarch64.json`
- `final-manifests/darwin-x86_64.json`
- `final-manifests/windows-x86_64.json`

这些文件已绑定真实 r3 三平台最终字节。每份必须通过最终发行 manifest 校验：0.1.10/prod/com.osir.client、r3 commit/source hash、原生 target/arch、真实 executable hash、原始内置 updater 公钥/端点、verifiedAtEnd=true、非 pending release mode、updater 包大小/hash/signatureVerified=true。manifest 原始 hash 和大小必须与 spec 完全一致。

对象名称固定为 `OSIR-Client-prod-0.1.10-<target>.app.tar.gz`（Mac）或 `OSIR-Client-prod-0.1.10-x86_64-pc-windows-msvc-setup.exe`。objectUrl 必须在 `https://osirvedio.cn-nb1.rains3.com/osir-client/releases/prod-0.1.10/` 下精确匹配文件名。feedUrl 仅允许同一对象 URL、精确 `https://osirclaw.com/api/v1/osir-client/artifacts/prod-0.1.10/<filename>`，或精确 `https://osirclaw.com/api/v1/osir-client/download/prod-0.1.10/<platform>/updater`（platform 必须等于当前平台）。不接受其他 release、platform、kind、host、query、fragment 或路径变体。实际旧应用可走其既有生产下载链路；独立读回不跟随重定向。

## 登记与 dispatch 边界

评审后可将此目录的锁定文件（包含隐藏的 `.gitattributes`）和最终 manifests 登记到授权 repo `ousir0/osir-client` 的 `diagnostics/client-live-019-0110/`，将 `workflow.review.yml` 原样登记到独立 `.github/workflows/` 文件；只提交具体 harness 文件，不带产品工作树改动。`.gitattributes` 禁止 checkout 换行转换，public spec 以 UTF-8 原始字节写入，确保 Windows 下仍按同一原始 digest 校验。

**仅 r3 控制输入绑定提交；当前不 dispatch。** 必须先由主控提供真实最终 public feed 字节、切换收据和明确公开切换确认。workflow 只允许手工 dispatch，输入为原始 public_spec、这些 UTF-8 字节的 public_spec_sha256、显式 cutover_confirmed=true。Ubuntu gate 校验完整合同和 focused tests 后，才启动三平台。不得用 JSON 占位内容进行“试跑”。

## 隔离及证据

只接受 `ousir0/osir-client` 的 github-hosted runner、当前 run 所有权标记和 RUNNER_TEMP 下全新专用目录。拒绝既有 Client/默认数据。隔离 CC_SWITCH_TEST_HOME、CODEX_HOME、CLAUDE_CONFIG_DIR、OPENCODE_CONFIG_DIR；Windows 不覆盖 HOME/USERPROFILE。禁用会话自动扫描，无登录、账户密钥、模型请求。只上传 job 自有 `evidence/`，不上传 home、数据库本体、缓存、包或私有目录；证据中的数据均为合成数据。

最终 result.json 必须具有八个有序事件、old/new PID、同路径、不同且精确的旧新 hash、schema25→26、真实点击/窗口/数据报告。Windows 额外要求真实 native IPC 0.1.10、精确进程树、可见窗口尺寸；Mac 要求 bundle 身份和原生窗口。任何未完成的边界都写 failed/blocked，不能由单纯 installer 成功、手工重启或文件替换变成 passed。

## 复用、测试与当前限制

`reviewed-provenance.json` 记录来源和 hash。`reviewed/` 的旧输入、签名验证、数据 helper 和 11 项 tests 原样复用；原先 0.1.8→0.1.9 GUI helper 保留原件，对新流程只做必要适配。`reviewed/disposable.py` 保留旧 TLS serve 函数以维持原件 hash，**新流程只调用 init/seed/verify，不调用 serve**。

本地 focused 测试命令（不启动应用、不构建产品）：

```sh
node --test reviewed/prepare.test.mjs contract.test.mjs
python3 -m unittest discover -s reviewed -p test_disposable.py -v
python3 -m unittest -v test_harness.py
swiftc -frontend -parse helpers/click.swift
swiftc -frontend -parse helpers/gui-preflight.swift
swiftc -frontend -parse reviewed/osir-macos-window-probe.swift
node --input-type=module -e "import {verifyLocks} from './contract.mjs'; verifyLocks()"
```

最终实测结果见 `PREPARATION.json`。`helpers-lock.json` 覆盖所有可执行工具、测试、原件、workflow 和本文；`seal.mjs` 首次生成 seal，拒绝覆盖已有 seal。修改后必须重新评审并更新 seal，不能静默沿用旧 hash。最终 manifests/spec/results 分别由 spec 原始 hash、dispatch 输入 digest 和运行证据绑定，不在 helper lock 中。

Windows PowerShell/C# 尚未在原生环境解析或执行；workflow 在下载前新增真实 PowerShell parser gate。Swift 本地仅语法解析通过，未编译或执行 GUI helper。Mac runner 必须已有 console session 与 Accessibility 权限，缺少即 blocked，不改 TCC。Windows 必须有交互桌面和可用 WebView2 debug 监听，缺少即 blocked。三平台真实升级目前均 **not-run**，本地 helper 测试不代表公开更新已通过。

反思：这次工具的通过边界是旧应用自身完成更新、自动重启和迁移；最终包可安装或新版本能手工启动，均不足以替代这条证据链。
