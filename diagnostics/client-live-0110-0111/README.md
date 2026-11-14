# Client 0.1.10 → 0.1.11 原生升级控制 harness

状态：**prepared-not-dispatched；原生升级 not-run**。独立控制分支 `codex/client-live-0110-0111-20261114` 新增本目录，并仅在该分支把已登记的旧 workflow 入口改成 bridge；`client-live-019-0110` 目录和旧历史提交保留。没有修改冻结产品源码或切换 stable。旧 0.1.9→0.1.10 的通过结果不属于本轮。

## 身份与硬门禁

| 项目 | 精确绑定 |
| --- | --- |
| 基线 | 正式 0.1.10 r3；`ec2b05bab6224334e91922cd70c71faf8e9aee89` |
| 基线 source | `ca4c28e21069dda1cc948e55e5ef8e1438161d96a42a21c0971a8f8848a04e73` |
| 候选 | 0.1.11；`902ac8bc50bda2fa25ac54f8f24b12a15797ae32` |
| 候选 source | `a9a7c27e85c1fcf4b1f2805e8fbe8913e43c069688ffcb6abd494a33e7ee52ae` |
| ARM build | Client run `43601475491` attempt 1；`osir-macos-arm-release.yml` |
| Intel / Windows build | Client run `43601479837` attempt 1；`osir-windows-release.yml` |
| 数据库 | 旧、新均严格 schema **26**；逐项比较合成数据和用户配置 |
| 控制 commit | 后续 reviewed harness 的完整 commit，必须等于 `GITHUB_SHA` |

所有平台都必须是同一候选 source/commit。执行时只读 GitHub 公开 API 核对精确 run/attempt、repo、workflow、branch、commit 和 completed/success；不增加用户 token 权限。重新构建或 attempt 改变时必须另审控制绑定，不能静默采用最新 run。**API run 元数据本身不证明最终文件来自该 run**：收集/最终化时仍须核验 CI artifact 来源，最终 manifest 的原始字节、源指纹、架构、可执行文件及 updater 包 hash/size/签名必须全部锁入 public spec。没有最终 manifests 时拒绝执行，不以当前运行中的 CI 或 source 一致冒充字节验收。

`baseline-manifests/*.json` 和 `baseline-feed.json` 仅作为已发布 0.1.10 的来源输入，不是新升级通过证据。基线包直接使用固定 RainS3 对象，重新校验大小/hash/0.1.10 原内置公钥签名；不重建旧包。公钥签名验证代码已与 0.1.10 r3 冻结源码逐字节比较一致。

## 已做的安全准备

在本目录运行以下命令只读文件，不联网、不启动应用：

```sh
node prepare-next.mjs inspect
node --input-type=module -e "import {verifyLocks} from './contract.mjs'; verifyLocks()"
```

两个 CI run 的已知元数据已只读核对为候选 commit，查询时仍 in_progress。未下载大工件、安装依赖或重跑产品 focused tests。Anywhere 的 run `43600642383` attempt 2 仅属上下文，不进入 Client 升级结论。

## 后续接入与调用

1. 完成并审阅候选三平台最终化和原包验收、CI artifact 来源。将**精确原始最终 manifest**放入本目录的 `final-manifests/{darwin-aarch64,darwin-x86_64,windows-x86_64}.json`；不要复制旧目录的 0.1.10 final-manifests 当候选。最终文件必须为 release 模式、verifiedAtEnd=true、updater signatureVerified=true。
2. 审阅 helper lock、下述 workflow 与最终 manifests，在授权 `ousir0/osir-client` 独立控制 ref 提交最终输入。实现文件为 `.github/workflows/osir-client-live-0110-0111.yml`，同时支持 `workflow_call`；已登记的 dispatch 入口为 `.github/workflows/osir-client-live-019-0110.yml`，在本分支只透传四个输入到同一 commit 的新 workflow，不继承 secrets。两者分别与 `workflow.review.yml`、`bridge.review.yml` 逐字节一致并纳入锁。新 workflow 尚未在 default 登记，调用时须指定这个已登记的旧文件名及本分支；不要向旧控制分支 dispatch。最终 manifests 缺失时仍会拒绝执行。发布分支不移动。
3. 仅在主控完成真实正式 0.1.11 条件切换后，保存公开 feed 的**原始字节**及真实发布回执；建立绝对路径的外部 `context.json`，精确字段如下。不提供可执行占位 spec，也不生成模拟 feed。

```json
{
  "harnessCommit": "<真实40位reviewed控制commit>",
  "cutoverConfirmed": true,
  "cutoverConfirmedAt": "<已发生的UTC ISO时间>",
  "cutoverReceiptPath": "/absolute/real-cutover-receipt.json",
  "publicFeedPath": "/absolute/exact-public-prod-feed.json"
}
```

```sh
node prepare-next.mjs assemble /absolute/context.json /absolute/new-public-spec.json
node contract.mjs validate /absolute/new-public-spec.json
node contract.mjs verify-builds /absolute/new-public-spec.json
```

`assemble` 只读这些显式文件与最终 manifests，写一个全新 spec，不联网。spec 每个平台新增 `buildRun:{id,attempt}`，并锁定最终 manifest/executable/updater；缺字段、旧版本、旧源、失败 build、未来切换时间或旧 feed 均不能进入原生验收。`verify-builds` 仅只读公开构建元数据。

4. 用下面命令生成**dispatch 请求文件**，UTF-8 spec 必须保持原字节。此命令不发请求；主控独立核对后才可调用后续 `gh api`。`HARNESS_REF` 使用已审控制 commit 可定位的独立 ref；若 ref 后来移动，workflow 的 `GITHUB_SHA` 比较会阻断旧 spec。

```sh
PUBLIC_SPEC=/absolute/new-public-spec.json HARNESS_REF=codex/client-live-0110-0111-20261114 REQUEST=/absolute/new-dispatch-request.json python3 - <<'PY'
import hashlib,json,os
from pathlib import Path
raw=Path(os.environ['PUBLIC_SPEC']).read_bytes()
spec=json.loads(raw)
assert spec['fromVersion']=='0.1.10' and spec['toVersion']=='0.1.11'
assert spec['cutover']['confirmed'] is True
request={'ref':os.environ['HARNESS_REF'],'inputs':{'public_spec':raw.decode('utf-8'),'public_spec_sha256':hashlib.sha256(raw).hexdigest(),'cutover_confirmed':True,'windows_only':False}}
with Path(os.environ['REQUEST']).open('x',encoding='utf-8') as f: json.dump(request,f)
PY
# 以下命令会触发三平台真实升级，不属于本轮已执行的准备：
gh api --method POST repos/ousir0/osir-client/actions/workflows/osir-client-live-019-0110.yml/dispatches --input /absolute/new-dispatch-request.json
```

只在一次性 GitHub 官方 GUI runner 运行：ARM macos-15、Intel macos-15-intel、Windows windows-2022。禁止在用户本机伪造 runner 环境执行。Windows 保留默认真实用户 home 的干净检查与归属绑定，避免 RunAsUser 自动重启丢失进程级测试 home。Mac 拒绝 Rosetta；GUI/Accessibility 不足直接 blocked，不改 TCC。

## 实际通过标准与边界

真实 0.1.10 初始化 schema26 → 停止且只停止自己证明归属的 PID → 写合成供应商/项目/设置/工具配置 → 重开旧包 → 真实点击“更新并重启” → 旧 PID 退出 → 产品自动拉起不同新 PID → 同路径、0.1.11 原生版本/精确新字节/签名/窗口 → 合成数据保持、schema26→26。点击之后不允许 harness 代为启动。Windows 还要求真实 renderer/native IPC；Mac 校验 AXPress、bundle 身份与可见窗口。八个有序事件缺一不可。

固定正式 feed 和 RainS3 链路沿用旧产品端点；不改 DNS/hosts/TLS/应用端点，不启动复制来的旧 TLS serve helper。**因此 stable 尚为 0.1.10 时，本 harness 不会证明候选升级成功。** 新 0.1.11 发布前的本机正规安装与真实对话由主代理负责，和本任务分开记证据。

只上传本 job 的 evidence，不上传 home、DB 本体或包。`windows_only=true` 只控制本次平台范围；没有本轮 0.1.10→0.1.11 的 ARM/Intel 结果时不得声称三平台通过。旧 019→0110 的日志、截图、schema 迁移或 result 不能直接套用。

本地只运行新增 4 项版本/build 绑定测试与新增 2 项 schema26 测试；其余旧 focused suites 不重复运行。语法/lock 检查不等于原生升级验收。修改 helper 后重新生成新目录 seal，并重新评审；旧 seal 不覆盖。

反思：本轮要证明的是正式旧包自己完成跨版本更新和数据保留，不能用安装新包或旧版本测试通过替代。
