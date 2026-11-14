# 后续：最终 Client → Anywhere 实际公开安装 UI 验收

状态：**合同准备，不是已实现的 Anywhere 安装验收 driver，也未执行**。在主控提供最终公开产物并明确 cutover 后，使用全新 disposable GitHub GUI job；不能把 Client updater result 当作 Anywhere 安装通过。

## 可直接复用的部分

| 现有文件/来源 | 复用边界 |
| --- | --- |
| 本目录 contract.mjs、live-windows.ps1、live-macos.py | 复用 hosted/repo/run 所有权、新工具 home、Client 精确 hash/路径、PID 清理、证据上传规则；Anywhere 需要独立 spec/状态链，不能直接套 updater passed 条件 |
| helpers/check-ui.mjs | 复用 Windows CDP 连接、真实 DOM button click、截图、native IPC 版本读取；需增加实际“工作台→OSIR Anywhere→查看详情→安装→重新检测→打开 Anywhere”流程，保留现有框架 |
| helpers/click.swift、reviewed/osir-macos-window-probe.swift | 复用 Mac AX 树查找/AXPress、PID/bundle/path 身份和原生窗口；更新按钮文本不能直接用于安装按钮 |
| helpers/windows-window.cs | 只读可见原生窗口、owner PID、尺寸；不负责安装点击 |
| 冻结 r2 scripts/osir-windows-packaged-smoke.ps1 | 其 OsirWindows.Controls/ClickPrimary/QueuePrimary/Activate 可复用到新外部 driver，绑定真实 NSIS PID/窗口/控件；当前只读摘录没有这些操作函数 |
| reviewed/disposable.py | 可复用 owned-root / 合成数据隔离，不复用旧 TLS serve；Anywhere 安装成功依据不是此 helper 的 DB26 检查 |

以上都是现有 native/DOM UI 自动化路线，无需引入新框架。Mac **Finder DMG 拖拽/拷贝至 Applications 的真实 UI 操作尚无已完成 driver**；只读挂载、命令行 ditto/cp、直接打开 app 不能冒充完成原生安装 UI。

## 已明确的产品合同

固定公开清单为 `https://osirvedio.cn-nb1.rains3.com/osir-anywhere/releases/stable.json`。RSA-SHA256 封套为 `{payload,signature}`，payload 为 base64 JSON v1，固定 productId=osir-anywhere/channel=stable；公钥只能取冻结 Client 的 `src-tauri/resources/products/osir-anywhere/release-public.pub`，不得替换为运行时环境公钥。

已只读确认该冻结公钥 SHA-256：`87317e9826a1538342e45be6035457dc982247b9dbcf2c890d8290c46fcdb512`。

原生安装必须使用最终发布 Client 0.1.10/prod（r2 `8bebc7a884b51336590e0b54313f70f3f4364509`、source `a376ca7fc36ad9546b8095276cd736b282833513f22d7bc5dce1e44d75914017`）。`CC_SWITCH_TEST_HOME` 仅隔离数据；不能使用 dev/beta 或其他会触发 distribution::is_isolated 的包来绕过“正式版中操作”限制。

**真实 UI 进入条件已有明确阻塞**：冻结 r2 `src/features/osir/OsirRoot.tsx` 只有 `session.data?.connected` 才渲染 OsirWorkbench；未登录显示欢迎授权页，没有匿名工作台入口。当前 updater 可在欢迎页验收；Anywhere 安装页不能保持无账户同时完成真实 UI 验收。需要主控安排专用、可撤销测试账户的真实浏览器授权和 runner 操作方式。不得注入 React/query cache、伪造 session、复制生产凭据或用直接 invoke 写命令绕过。未安排则后续标记 `blocked: genuine-client-auth-required`，不能声称已测安装 UI。

真实按钮和只读观测合同已存在于冻结 UI：

- `.osir-anywhere-entry[aria-label="OSIR Anywhere 入口"]` → “查看详情”；unsigned 包的入口安装按钮会先进入详情，必须记录“安装提示”。
- `.osir-anywhere-page[aria-label="OSIR Anywhere"]` 内“安装”，下载中“取消下载”，取消/失败后“重新安装”，已安装后“打开 Anywhere”。
- `aria-label="重新检测 OSIR Anywhere"` 的按钮用于安装后显式检测；不能假定 waiting_installation 会自动轮询安装完成。
- 仅把 `osir_anywhere_release`、`osir_anywhere_install_status`、`osir_anywhere_detect` 作为只读观测；启动安装/取消/打开必须走真实 UI，不直接 invoke 写命令。
- 安装状态为 idle/downloading/ready/opening/waiting_installation/cancelled/error；waiting_installation **不表示 installed**，原生 installer 退出码也不表示 installed。
- detect 成功必须 installed=true/status=installed、精确 version/path、distribution=osir_standalone；随后真实打开按钮必须带来匹配路径/产品身份的新 Anywhere PID 和可见原生窗口。

macOS 检测真实 `OSIR Anywhere.app`、bundle `com.osir.anywhere.desktop`，候选为隔离 home 的 Applications 和系统 /Applications。Windows 检测 `OSIR Anywhere.exe` 的 PE 产品与 resources/app.asar 真正 package 身份，NSIS GUID=`d78e534b-2cbd-5c14-9c2e-5fdd65be38df`；仅伪造注册表或 fixture 通过不能算真实安装。

systemSignature 必须与最终签名清单、平台和实际包匹配：unsigned 可为无签或有效 ad-hoc Mac app；坏签名停止。Developer ID 必须最终 app 完整签名与 notarization；Windows authenticode 必须有效。保留 quarantine/Zone.Identifier、Gatekeeper、SmartScreen 和原生确认。测试不能关闭这些保护；不可操作的安全桌面/权限确认按 blocked 回执。

## 主控仍需提供的精确输入

1. **Client 最终公开身份**：三个最终 manifest 原始字节/hash/大小、实际 executable hash、包的精确公开 URL/hash/大小/updater 签名、公开 cutover 回执。可复用 updater strict spec，但 fresh-install UI 验收仍需注明 Client 基线部署方式；Mac updater archive 部署不能声称测了 Client DMG。
2. **Anywhere 最终公开身份**：真实 stable.json 原始 envelope 字节/hash/大小、payload 原始字节/hash、RSA 签名、releaseId/version/publishedAt 和所有 minimum 要求；每个平台选择出的 artifact 精确 URL/filename/size/sha256/systemSignature；如提供 Universal，必须给两个实际 native slice 的 identity/hash，不用占位数据。
3. **已安装内容身份**：Mac 最终 Info.plist 身份/版本、app 可执行文件及 app.asar hash、各 slice 架构和真实系统签名分类；Windows 最终 PE 产品/版本/架构/hash、app.asar hash、实际 electron-builder package 身份和 NSIS 安装配置（per-user/per-machine、默认路径、是否自动启动、finish 选项、语言/控件）。这些约束用于比对真实安装，不能手工写入注册表/descriptor 代替。
4. **兼容服务器**：若 minimumServerVersion 存在，固定 `https://anywhere.osirclaw.com/api/v2/health` 必须真实返回 `status:"ok",version:"x.y.z"` 且满足 minimum。需要公开端点切换回执与可用版本；不能拿 desktop version 或 renderer 输入充当 server version。
5. **真实授权安排**：专用测试账户经正式浏览器授权 Client、最小权限、有效期和撤销方式；不允许创建/复制模型 API 密钥。授权回调必须回到该 job 的真实 Client。需要说明 unattended runner 能否完成此流程；若需要人工参与，必须有明确 GUI 交接方式，不能把本机生产登录导入 runner。
6. **原生安装操作边界**：Mac 实际 DMG 卷名/布局/Applications 目标及 Finder 可操作控件；Windows 实际 NSIS 窗口/页/产品身份。GUI 会话、AX、screen capture、UAC/SmartScreen 可操作性是 runner preflight，不能承诺预先通过。若 secure desktop 或 Gatekeeper 要求凭据/管理员操作且环境不支持，保留截图/阶段并 blocked，不静默改配置。

上述无实际值时不得生成可 dispatch 的有效 Anywhere spec。当前 updater 的 helpers-lock 不包含未来 driver，未来新增必须独立评审/锁定。

## 真实证据链与覆盖边界

全新 GUI runner 上确认 Anywhere not_installed → Client 真实导航/安装按钮（截图、节点/AX、时间、owner PID）→ Client 原生固定公开清单 release ready + unsigned 提示（如适用）→ 下载进度和完成 → 原生 DiskImageMounter/NSIS 窗口，绑定到实际已校验缓存包 → 使用实际原生 UI 完成安装 → Client 真实“重新检测”回到精确 installed/version/path → 真实“打开 Anywhere” → 真实新 Anywhere PID/路径/hash/架构/窗口。

下载取消与重试需另一个 fresh case，或在真实 downloading 期间完成真实点击；若公网下载太快没捕获到取消窗口，标为未测，不注入假 feed 或代理拖慢。缓存重用/去重、取消后不打开、坏包拒绝、模型目录/对话/重连分别列独立覆盖，不用 installer 成功推导。后续安装验收需要真实 Client 授权作为前置条件，但不扩大为账户功能验收或模型请求；不复制账户密钥，不扫描 runner 外部 home，不手工伪造 binding descriptor。证据不得上传账户页、令牌或账户/浏览器 profile；原始应用日志需筛选，仅保留脱敏安装事件。

反思：当前最先要解决的门槛是真实 Client 授权，其次是最终公开包的内容身份及原生安装 UI；invoke DTO 已固定，不能用已有 Rust fixture 或 headless IPC 替代真正用户路径。
