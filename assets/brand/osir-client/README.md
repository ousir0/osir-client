# OSIR Client 统一 Logo 与平台图标

唯一品牌母版为 `app-icon.png`：用户选中的轨道 O 卡通角色 B，双眼看向右上，完整1024px透明图。来源为[实际生图与眼神微调](../../../docs/design/osir-client-avatar-b-gaze-2026-10-30/README.md)。工作台、欢迎页、关于页、默认头像与桌面应用都使用这个角色；平台外壳和几何由生成器统一适配。

## 一键生成与替换

```bash
python3 -m pip install -r scripts/requirements-icons.txt
pnpm icons:osir
pnpm icons:osir:check
```

以后替换角色，只需替换本目录 `app-icon.png` 后运行生成，或一步导入新母版并生成：

```bash
pnpm icons:osir --source /absolute/new-transparent-logo.png
```

输入须为正方形、192–4096px、四角透明的 PNG，建议保留1024px高分辨率。生成器不重新生图；它保留彩色角色，按平台缩放与布局，从同源颜色/明暗提取简化单色托盘。`tray-master.png` 是自动生成物，不能独立替换为另一套标志。用户已自选的本地头像不受默认图更新影响。

## 平台标准与输出

2026-10-30 最新要求覆盖旧的“一律透明”规则，各用途分别处理：

| 用途 | 布局与背景 | 输出 |
| --- | --- | --- |
| 前端 Logo / 默认头像 | 透明角色，不添加应用图标底板 | `src/assets/brand/osir-client.png`128px、`osir-avatar.png`256px |
| macOS app / Dock / Finder |1024px画布，824px连续圆角矩形浅色底，四边100px透明外距；完整角色最大704px，轨道不裁切 | 各渠道 `icon.icns` 和 `macos/*.png` |
| Windows app / Explorer / taskbar | 透明彩色角色，边缘留白，无 macOS 底板 | 各渠道 `icon.ico`、根目录 PNG 和 Square/StoreLogo |
| macOS menu bar / tray | 同源简化单色，面部/眼白为负形；无底板，系统负责明暗色 |24pt、1x/2x/3x黑色 alpha 模板 |

生成目录为 `src-tauri/icons/osir/`，包括正式版、`dev/`、`beta/`：

- 各渠道 Windows PNG：16/20/24/32/40/48/64/96/128/256/512/1024px，`128x128@2x.png`；同尺寸 macOS 图位于 `macos/`。
- ICNS 显式包含16px1x/2x、32px1x/2x、128/256/512px1x/2x，最大1024px Retina 图。
- ICO 包含16/20/24/32/40/48/64/96/128/256px十个32位帧，供标准及高DPI缩放。
- Windows SquareLogo：30/44/71/89/107/142/150/284/310px，StoreLogo50px；无需复用上游图标。
- Dev/Beta 添加简洁渠道字样，正式版不添加文字。
- `brand-logo.png`、`brand-logo-128.png`、`brand-logo-256.png`保存透明共享品牌图；生成器同步前端 Logo 与默认头像。
- `tray-template@1x.png`、`@2x.png`、`@3x.png`及原生使用的 `tray-template.png`；`tray-master.png`为192px同源中间产物。
- `preview.png`展示各平台、渠道、深浅底、小尺寸及前端 Logo / 菜单栏模板；`manifest.json`记录几何、源文件和全部产物 SHA-256。

Tauri 正式/Beta/Dev 配置分别引用对应目录的 ICNS/ICO 和 PNG。macOS打包读取ICNS，Windows读取ICO；前端使用透明品牌 PNG，不把 macOS 图标底板带进页面。更改图标后必须原生重建/重新打包，页面热更新不会刷新已安装应用的 Dock 图标。不清理用户系统图标缓存。

`pnpm icons:osir:check` 在临时目录完整重建，检查全部文件哈希、前端 Logo / 默认头像、托盘母版、尺寸、透明度、ICNS及ICO帧，不修改现有产物。检查通过仅表示资源格式与来源一致；Windows任务栏/安装器仍需Windows真机验收。

历史生图与旧图标保留来源记录；`scripts/osir-brand-cutouts.py`仅重建历史素材，不再覆盖现行母版。上游 CC Switch 图标和许可在本目录以外独立保留。
