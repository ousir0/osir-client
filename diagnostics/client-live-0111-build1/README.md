# 0.1.11 → 0.1.11+1：发布后真实 updater harness

状态：三份真实 final manifests 已逐字节接入并纳入 seal；此控制分支获授权 push，尚未 dispatch 或执行原生升级。不能作为升级通过收据。

## 固定身份

- 独立 repo：`/Users/ouwei/.codex/release-evidence/20261115-client-live-0111-build1-control`
- 分支：`codex/client-live-0111-build1-20261115`；沿用旧控制 commit `d4952bdda4028ccc055f5ff5bf28e46768954b90` 的 native post-public-cutover 流程。
- 基线：真实已发布 `0.1.11`；commit `902ac8bc50bda2fa25ac54f8f24b12a15797ae32`，source `a9a7c27e85c1fcf4b1f2805e8fbe8913e43c069688ffcb6abd494a33e7ee52ae`。
- 候选：内部 `0.1.11+1`；source commit `776abf7f7b2f47ce2aa6164ccd9ca7d6e29e57ac`；source SHA256 `e748f403dffa7b63821911f07b224b7c32a2a3c26b1bf66174a39ca4d5cd57a9`。
- ARM run `43631070443` attempt 1，`.github/workflows/osir-macos-arm-release.yml`；Intel / Windows run `43631088371` attempt 1，`.github/workflows/osir-windows-release.yml`。执行时必须重新核对 completed/success、同一 source commit、workflow、branch。
- Feed/native version 保留 `0.1.11+1`；release ID 与工件文件名使用 `prod-0.1.11.build.1`。对象必须精确位于 `https://osirvedio.cn-nb1.rains3.com/osir-client/releases/prod-0.1.11.build.1/`。

`baseline-feed.json`、`baseline-current.json` 为先前真实公开观测原始字节；三个 baseline manifests 与已发布备份逐字节一致。校验器来自实际 0.1.11 源快照的 `scripts/osir-release-updater.mjs`，保持字节不变。所有签名都是公开 updater 签名。

## 最小适配与准确边界

旧已安装 0.1.11 的界面尚未剥离 metadata，所以升级前最新版本应真实显示 `0.1.11+1`；不能要求旧包具有新 UI。新包 native 版本必须完整为 `0.1.11+1`，Mac Info.plist 也按冻结 verifyApp 的精确版本规则校验。候选公开显示规则为 `0.1.11`；未登录欢迎页没有版本字段，本 harness 不伪造欢迎页版本显示证明，也不登录来打开设置。

保留原流程：官方一次性 GUI runner → 真实旧包 → schema26 初始化与合成数据 → 真实“更新并重启”点击 → 旧 PID 退出 → 不同新 PID 自动出现 → 同安装路径、精确新二进制字节、原生版本、签名、真实窗口 → schema26 数据保留。点击之后不手动启动 UI；Mac 只允许额外运行立即退出的只读 `--osir-release-diagnostics`，空环境，不进正常启动路径。

Windows 新增真实已安装应用的只读 `check_app_update_available`，要求成功返回 JSON null；异常/超时绝不能当作 null。再观察真实 renderer 启动检查窗口，拒绝再次出现安装按钮，重读同一真实 feed；保存 `native-no-reoffer.json` 并绑定新 PID/二进制 hash。只检查，不再次安装。

**Mac no-reoffer 保持 blocked。** AX 可以在已登录后的真实“关于与更新”页面点击“检查更新”，通过新出现的“已是最新”成功提示与“检查更新失败，请稍后重试。”错误提示区分结果；不能仅以没弹更新窗作为成功。但本 harness 的一次性 runner 不登录：冻结源 `src/features/osir/OsirRoot.tsx:72-79` 只在 connected 时挂载工作台；`OsirWorkbench.tsx:1685-1700` 内才有设置/关于；`AboutSection.tsx:162-171` 才拥有上述检查与成功/失败提示。欢迎页没有该控件，自动检查失败只写 context/console（`UpdateContext.tsx:108-111`）。因此当前完整 AX 路径缺少登录后的真实入口，不能用注入、调试口、伪造会话或“无弹窗”补齐。Mac 驱动保留升级/窗口/原生版本/数据保留观察，最后 pending no-reoffer + blocked，绝不生成 passed。Windows 可独立完整执行；三平台总体仍 pending。

## 当前 pending（不是占位发布凭据）

1. `0.1.11+1` 真正正式切换回执、真实 live feed 原始字节、实际 UTC 时间：未收到。不能预造；主控交付前禁止 dispatch。
2. Mac 已安装 updater 无更新结果：blocked，见上。

两个 exact build runs 已完成且 success，attempt 均为 1；执行时仍重新核验。三份真实输入来自 `20261115-client-0111-build1-finalized/{mac-arm64,mac-intel,windows-x64}/build-manifest.json`，没有改写；公开 SHA-256 如下：

| final-manifests 文件 | 原 CI artifact ID | 原 artifact archive SHA-256 | final manifest SHA-256 |
| --- | --- | --- | --- |
| darwin-aarch64.json | 14129973590 | 21a6aa5ba9ee12042dc0694e6ee777c22e66c012d7c9b8f68ddb898eee8be9d6 | 5c101b860fe4ae84003362e9ae7bcd850185bf2b4d9e8b95d6df0c277f5e88eb |
| darwin-x86_64.json | 14129478520 | 5ab4ca759710753ff49206fb2726411d8af4a71cda078cf8e3ed534b24f66996 | 86d1adbf70ca9d8d565d9a9d52226c14c563990aed84711453b3a973f41bc58f |
| windows-x86_64.json | 14129726888 | 7de152d57442f5b04cd213dec4187130dcc41755418dd9ca54b49ccb558d623c | 2f946095537f1c83db07c4ccba2ac19818770fbc479ed1a863d28c95612cb33e |

Helper seal 同时锁准备文件、真实基线和三份 final manifests；最终 manifests 另由严格 spec 的原始 SHA256/size、源码/二进制身份绑定，CI 从 spec.harnessCommit 对应 checkout 读取。缺文件、内容改变或没有真实切换输入时门禁拒绝。

## 本地入口与主控执行顺序

在本目录：

```sh
node prepare-next.mjs inspect
node --test contract.test.mjs reviewed/prepare.test.mjs
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest -v test_harness.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s reviewed -p test_disposable.py -k test_0111_build1 -v
```

原生入口：`live-macos.py` / `live-windows.ps1`；实现 workflow `.github/workflows/osir-client-live-0111-build1.yml`。已登记的 dispatch 入口仍为 `.github/workflows/osir-client-live-019-0110.yml`，仅本独立控制分支将其转发到同 commit 新 workflow；不继承 secrets。已授权 push；dispatch 仍必须等真实 feed 切到 `prod-0.1.11.build.1` 且主控提供实际 cutover 回执。

发布后才可建立显式外部 context JSON，字段精确为：

- `harnessCommit`：最终已审阅、含三平台 final manifests 的完整控制 commit。
- `cutoverConfirmed`：true。
- `cutoverConfirmedAt`：真实已发生的 UTC ISO 时间。
- `cutoverReceiptPath`：实际发布回执绝对路径。
- `publicFeedPath`：实际公开 +1 feed 原始字节文件绝对路径。

```sh
node prepare-next.mjs assemble /absolute/real-context.json /absolute/new-public-spec.json
node contract.mjs validate /absolute/new-public-spec.json
node contract.mjs verify-builds /absolute/new-public-spec.json
```

准确 dispatch 参数（现阶段全部待主控执行，尚未发送）：

- workflow：`osir-client-live-019-0110.yml`，repo `ousir0/osir-client`。
- ref：`codex/client-live-0111-build1-20261115`，必须解析到最终 spec.harnessCommit。
- `public_spec`：上述生成文件精确 UTF-8 内容。
- `public_spec_sha256`：该文件原始字节 SHA256。
- `cutover_confirmed`：true，仅真实发布后。
- `windows_only`：true 只做 Windows 完整验收；false 会执行三平台，当前 Mac 必须以 blocked 结束。不得据 Windows 通过宣称三平台通过。

此处不提供含虚构内容的可执行 dispatch 请求文件。正式入口只从真实已发布 feed 下载包；不替换 feed、不改 hosts/TLS/端点、不运行继承的历史 TLS helper、不碰生产应用与15721。

反思：本次准备能证明严格门禁与升级证据要求；真实升级结论只能来自正式发布后的新一次运行，Mac 缺证据就保留 blocked。
