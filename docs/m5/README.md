# OpenPicasa M5 需求与原型材料

整理日期：2026-11-16 22:44 (Asia/Shanghai)

这是一份供后续提交到 GitHub 的**自包含文档与原型包**。它用来交接已确认的 v1.4 视觉 / 交互、M5 需求说明书草案，以及作为事实基线的源码审计。

**请勿把本包理解成：生产环境的 Rust / egui 应用已经被改成这个 HTML 原型，或原型已经落地发版。** 真实产品仍是仓库 `chongsenli/openpicasa` 上的桌面应用。本包里的 `prototype/v1.4/` 是离线 `file://` 交互原型，用于评审和对照，不读写用户磁盘上的真实照片，也不等于桌面测试已经通过。

## 事实基线

- 产品仓库：`https://github.com/chongsenli/openpicasa`
- 审计提交：`c845cf0363eed237ef51be1ca379a4aec4cbe446`（`main`），版本 `0.4.2-beta.1`
- 审计方式：**只做源码核验，没有运行桌面应用**
- 设计基线：已确认交互原型 v1.4（亮色方案 A 清场 + HeroUI Pro 视觉语言，暗色方案 B 暗房 + beUI Pro 视觉语言；查看器为展厅结构）。不要求把这些 Web 组件库移植进 egui，只转译视觉语言。
- 需求文档：`requirements/M5-需求说明书-v0.1.md`（草案，2026-11-16 经审阅修订）。其中的待确认事项见该文档第 11 章；已从待定收回的数据保护修复（废纸篓确认范围必须与执行集合一致、关于文案不得再承诺「只读不改原文件」、底栏无可点索引入口）以说明书正文为准。

## 模拟 ≠ 实装（务必分开）

| 原型（本包） | 产品（审计中的事实） |
| --- | --- |
| 离线 HTML，演示目录选择器，不打开系统文件对话框 | 系统选目录、真实扫描与索引库 |
| 「移到废纸篓」只从演示库移除条目，确认框写明差别 | 会 `rename` 原文件到系统废纸篓 |
| 导出演示 JSON，不能当正式 `.opicasaidx` | 导入 / 导出使用 `opicasaidx` / `opicasaidx.zip` |
| 拍摄信息多为界面示例 | 查看器打开时读取原文件 EXIF |
| 自动化 97/97 通过（2026-11-16，file://，Puppeteer + Chrome） | **不等于**桌面回归已通过 |
| 评分、列表布局、可播幻灯片、从相册移出、最近导入、恢复演示数据 | 演示扩展 / 非默认 M5，不是已实现产品能力 |

更完整的边界见 [prototype/v1.4/README.md](prototype/v1.4/README.md) 与 [validation/FEATURE-MAP-v1.4.md](validation/FEATURE-MAP-v1.4.md)。

## 目录

- [requirements/M5-需求说明书-v0.1.md](requirements/M5-需求说明书-v0.1.md) — M5 需求说明书（最新修订）
- [requirements/SELF-CHECK.md](requirements/SELF-CHECK.md) — 编号覆盖自检
- [PATHS.md](PATHS.md) — 说明书中的绝对路径 → 本仓库相对路径
- [audits/](audits/) — 全应用审计、索引审计、v1.2 / v1.3 功能映射；`historical/` 中为审计点名不要误抄的旧 HTML
- [prototype/v1.4/index.html](prototype/v1.4/index.html) — 可离线打开的 v1.4 原型（双击或拖进浏览器即可，不需要本地服务器）
- [prototype/openpicasa-prototype-v1.4-offline.zip](prototype/openpicasa-prototype-v1.4-offline.zip) — 原始官方离线 zip
- [validation/](validation/) — 功能映射、测试脚本与测试报告（与原型 `docs/` 同步的一份平铺副本）
- [screenshots/v1.4/](screenshots/v1.4/) — v1.4 全套关键截图
- [screenshots/v1.3-index-viewer/](screenshots/v1.3-index-viewer/) — 理解索引 / 查看器所需的 v1.3 截图
- [MANIFEST.md](MANIFEST.md) — 源路径、仓库路径、字节数、SHA-256

## 如何打开原型

1. 打开 [prototype/v1.4/index.html](prototype/v1.4/index.html)，或解压 `prototype/openpicasa-prototype-v1.4-offline.zip` 后打开其中的 `index.html`。
2. 不需要网络、不需要构建 Rust、不要用这份 HTML 替换生产应用。

## 草案中仍待确认的事项

以需求说明书第 11 章的编号为准，主要包括：默认缩略图边长 96 与原型 144、查看器背景档位、是否做独立帮助页、主题切换入口位置、文件名开关是否持久化、去掉底栏角标后的注意力提示、暗色相册缩略图卡、关于入口的最终用词（准确含义已定）、Linux 上 Finder 用词、设置页除入口壳外的段落。不要把已确认的 v1.4 入口或已实装能力重新当成待定。
