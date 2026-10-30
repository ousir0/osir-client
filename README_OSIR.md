# OSIR API · AI 工作台

本仓库基于 [CC Switch](https://github.com/farion1231/cc-switch)，当前分支用于构建 OSIR API 桌面客户端。上游 README、用户手册和发布说明描述的是 CC Switch 原产品，不能直接当作本客户端的能力说明。

## 开始开发

```bash
pnpm install
pnpm dev:osir
```

`pnpm dev:osir` 使用 `.osir-dev-home/` 中的隔离环境，不会覆盖本机真实的 Codex、Claude Code、OpenCode 或 Gemini CLI 配置。macOS 默认使用独立应用包，关闭 Rust 文件监视重启，前端仍支持热更新；修改 Rust 后手动重启即可。Cargo 只用一个编译任务，开发配置关闭大体积调试符号和增量对象缓存。日常看效果与维护使用：

```bash
pnpm start:osir        # 复用已构建原生应用，只启动界面热更新；首次先 dev:osir
pnpm stop:osir         # 停止当前项目开发实例及其子进程
pnpm diagnose:osir     # 查看缓存占用和开发启动器
pnpm clean:osir:cache  # 停止开发后清理增量缓存；加 --all 清理整个 debug 构建缓存
```

纯浏览器预览可用：

```bash
pnpm preview:osir
```

macOS 的 `--app` 保留兼容，但已是默认模式。runner 只在程序或包配置变化时复制并签名 `.osir-dev-home/Applications/OSIR Client Dev.app`；内容不变时保留原签名。不要双击此热更新包，应通过启动器启动。需要自动重编译原生代码时可显式用 `pnpm dev:osir --watch`。开发与打包流程互斥；开发版默认关闭窗口即退出，随后清理开发服务，需要验证托盘行为时可设置 `OSIR_DEV_KEEP_TRAY=1`。Beta 与正式版仍遵循用户的托盘设置。

macOS 隔离开发启动在切换 HOME 前解析已有默认钥匙串，开发凭据使用独立命名空间；不会重置系统默认钥匙串。工具配置继续保存在隔离目录。

后台账户/模型凭据读写均禁止主动弹出系统密码框；保存登录或点击恢复已保存登录时才允许系统确认。临时签名的程序重新编译后仍可能需要用户确认；稳定签名可设置已有 `OSIR_DEV_SIGNING_IDENTITY`，发布则必须使用 Developer ID 与公证。不会自动创建证书、放宽钥匙串权限或明文保存凭据。

开发、Beta、正式包的命令、运行目录、签名与验收边界见 [构建与发布流程](docs/OSIR-BUILD-RELEASE.md)。

## 文档

- [用户接口、安全、用量与同步方案](docs/OSIR-API-INTEGRATION.md)

- [客户端设计规范](docs/OSIR-DESIGN-SYSTEM.md)
- [当前实现状态与剩余工作](docs/OSIR-IMPLEMENTATION.md)
- [开发、测试与正式构建](docs/OSIR-BUILD-RELEASE.md)
- [产品与落地方案](docs/osir-client-delivery-2026-10-26.md)
- [CC Switch 与 OpenCodex 集成调研](docs/osir-client-ccswitch-integration-2026-10-26.md)
- [选定的 UI 设计画册](docs/design/osir-client-focus-2026-10-26/index.html#06)
- [开发规则](AGENTS.md)

## 当前范围

四种工具都可保存多个供应商并切换使用；只有 Codex 使用内嵌多供应商模型聚合和唯一默认模型。OSIR 账户授权、余额、密钥、分组、订阅、用量、Codex Desktop 安装更新及原生生图仍按实施文档分阶段接入，界面不会用演示数据代替真实接口。
