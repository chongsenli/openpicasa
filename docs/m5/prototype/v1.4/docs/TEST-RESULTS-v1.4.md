# 原型 v1.4 测试结果

- 时间：2026-11-16（Asia/Shanghai）
- 环境：Puppeteer-core + Google Chrome，离线 `file://`，未运行 Rust 桌面应用。
- **本轮新用例：通过 97 / 共 97，失败 0。** 不凑分。

## 相对 v1.3 与更早版本的断言修订

v1.4 按用户最新明确改动覆盖了此前已确认的入口布局，因此下列旧断言已修订，不再按 v1.3 原文判定：侧栏必须有设置组与索引管理子项；底栏必须常驻「索引管理」按钮且点击打开索引页；更多菜单必须有「添加文件夹…」。新断言改为：侧栏顺序为图库→标签→相册→文件夹→导入的索引且无设置组；底栏无索引管理按钮/角标/隐形入口，滑块+像素值贴最右；更多菜单有「设置…」而无「添加文件夹…」；空库提示改指顶栏导入；设置页「打开索引管理」仍可达。更早版本中已过时的还包括：空闲 `20 / 20` 与百分比；缩略图 80–280；布局仅三档旧方案；查看器仅三档缩放。仍适用的部分（file:// 打开、亮暗主题、滑块实时改尺寸并跨页持久化、展厅查看器、不读真实磁盘、索引任务不因切页中断）已在本清单重跑。

## 逐条

| 结果 | 用例 | 说明 |
| --- | --- | --- |
| PASS | file protocol loads |  |
| PASS | OPApp and OPMgmt present |  |
| PASS | home grid has photos | cells=22 |
| PASS | sidebar 最近更改 |  |
| PASS | sidebar 最近导入 kept as extension |  |
| PASS | sidebar year rows | years=2 |
| PASS | sidebar 导入的索引 |  |
| PASS | thumb slider 22-512 | {"min":"22","max":"512"} |
| PASS | three real layouts in toolbar | grid,waterfall-v,waterfall-h |
| PASS | idle status has no 20/20 | 缩略图 144 px |
| PASS | idle status has no percent | 缩略图 144 px |
| PASS | default library has no placeholder cells | ph=0 |
| PASS | sidebar group order is 图库,标签,相册,文件夹,导入的索引 | lib,tags,albums,folders,impsrc |
| PASS | sidebar has no settings group or index entry | {"secs":["lib","tags","albums","folders","impsrc"],"sideSettingsSec":false,"sideSettingsLink":false,"sideIndexLink":false,"sideHasSettingsHead":false,"statusText":"缩略图 144 px","statusHasIndexWord":false,"statusLinks":[],"statusPadRight":12,"sliderAtRight":true,"topThumbBtns":false,"viewerZoomKept":true,"topImport":true} |
| PASS | status bar has no 索引管理 button, badge, or link | {"text":"缩略图 144 px","links":[]} |
| PASS | thumb slider + px group flush right | {"padRight":12} |
| PASS | topbar has no thumb minus/plus |  |
| PASS | topbar import entry kept |  |
| PASS | slider input updates css + storage | {"before":"144px","after":{"css":"48px","stored":48,"sm":"1"}} |
| PASS | small thumb tight radius flag |  |
| PASS | slider live values 22/80/144/256/512 | [{"css":"22px","stored":22,"label":"22 px"},{"css":"80px","stored":80,"label":"80 px"},{"css":"144px","stored":144,"label":"144 px"},{"css":"256px","stored":256,"label":"256 px"},{"css":"512px","stored":512,"label":"512 px"}] |
| PASS | slider persists across settings and library pages | {"persistSet":{"stored":200,"label":"200 px","css":"200px"},"persistAll":{"stored":200,"label":"200 px","css":"200px"}} |
| PASS | keyboard shortcut still resizes thumbs without topbar ± | size=160 |
| PASS | vertical waterfall class |  |
| PASS | horizontal waterfall / justified |  |
| PASS | list layout not a primary toolbar button |  |
| PASS | search scope control present |  |
| PASS | filter has full types, tag input, apply, size labels | 筛选（可叠加）  只看已收藏  无日期  无拍摄信息 类型 全部 JPEG PNG GIF WebP TIFF SVG HEIC AVIF RAW 尺寸 全部 小图 (≤640px) 中图 (640–1920px) 大图 (>1920px) 标签 按此标签筛 相机 / 镜头（示例元数据，须点应用） 应用 文件大小 (KB) ～ 应用 清除全部筛选 |
| PASS | sort has date/name/mtime/size/dim + dir + thumbsFirst | 排序 拍摄日期 ✓ 文件名 修改日期 文件大小 图片尺寸 改为降序 ✓ 改为升序 有缩略图的排在前面 |
| PASS | more menu has 设置… and list extension, no 添加文件夹 | 包含子文件夹 ✓ 显示文件名 ✓ 列表布局（演示扩展） 设置… ⌘, 索引管理… 帮助与快捷键 ? 切换到暗色 关于 OpenPicasa 恢复演示数据 |
| PASS | more menu keeps existing 设置… entry |  |
| PASS | 最近更改 route |  |
| PASS | status bar click does not open index manager | #/all |
| PASS | empty library hint points to top-bar import, not more-menu 添加文件夹 | 这里还没有照片 点顶栏的导入按钮，选一个放照片的目录（只读，不改你的文件）。本原型使用演示目录，不会读取你磁盘上的真实文件。 导入…恢复演示数据（演示扩展） |
| PASS | settings page has 打开索引管理 |  |
| PASS | settings page opens index manager |  |
| PASS | five index tabs present | tasks,folders,exclusions,storage,io |
| PASS | index tab tasks | pane=tasks 失败分类 没有失败项 任务 清空任务历史… ~/Pictures 首次索引 已完成 成功 完成 19 · 跳过 0 · 失败 0 /Volumes/Trav |
| PASS | index tab folders | pane=folders ~/Pictures 正常 正常 · 20 张 · 缩略图 17.6 MB 最近索引：1 小时前 监视 实时 刷新 重新索引 强制重扫 全量重建 关闭 在 Fi |
| PASS | index tab exclusions | pane=exclusions 全部恢复 清理失效项 ~/Pictures/2024/私密 恢复 ~/Pictures/2024/导出暂存 恢复 |
| PASS | index tab storage | pane=storage 占用 索引库文件 8.4 MB WAL 1.2 MB 缩略图 22 条 · 17.6 MB 照片 23 张 按根目录分布 ~/Pictures 17.6 MB  |
| PASS | index tab io | pane=io 导出索引文件 把索引记录与整理关系存成文件 · 不含原始照片。本原型只生成标清演示 JSON，不是真实的 .opicasaidx 数据库格式。 包含缩略图 |
| PASS | failure kinds visible |  |
| PASS | demo injects placeholder cells |  |
| PASS | root four states on folders tab |  |
| PASS | single-root delete confirm is index-only |  |
| PASS | storage tab has vacuum/logs/thumbs/diag | 占用 索引库文件 8.4 MB WAL 1.2 MB 缩略图 22 条 · 17.6 MB 照片 23 张 按根目录分布 ~/Pictures 17.6 MB /Volumes/TravelSD 212 KB ~/Documents/扫描旧照 3.1 MB 操作 清理缩略图 VACUUM 清空运行日志 导出诊断日志  |
| PASS | io tab labels demo format not real db |  |
| PASS | settings has demo scenarios and index entry |  |
| PASS | settings slider 22-512 |  |
| PASS | running status shows 已处理 without percent/ETA | 已处理 2 张 · 已用 00:00 缩略图 144 px |
| PASS | system-pause copy present and no per-task continue |  |
| PASS | folder picker is demo list not OS dialog |  |
| PASS | duplicate folder toast, no second root |  |
| PASS | denied folder does not ingest (IM-03) |  |
| PASS | trash confirm mentions real product moves files and prototype simulates |  |
| PASS | folder context available |  |
| PASS | dblclick opens exhibition viewer |  |
| PASS | viewer zoom minus/plus kept |  |
| PASS | viewer has compare, locate, background |  |
| PASS | info panel opens with EXIF honesty |  |
| PASS | side-by-side compare stage |  |
| PASS | zoom 100 readout |  |
| PASS | zoom in leaves 100 toward 800 range | 110% |
| PASS | help lists real shortcuts and marks extensions |  |
| PASS | edit is next-version toast |  |
| PASS | day group headers when grid+date |  |
| PASS | tag panel three-state boxes |  |
| PASS | settings includes SH-01/SH-06 demo buttons | demo-empty,demo-offline,demo-denied,demo-loading,demo-lib-error,demo-resume-pending,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt,demo-mgmt |
| PASS | library error red bar is closable |  |
| PASS | error bar dismisses |  |
| PASS | SH-01 resume toast after refresh |  |
| PASS | title has 拍摄时间 range |  |
| PASS | picker title is 选择照片文件夹（只读…） |  |
| PASS | cancel picker has platform toast |  |
| PASS | duplicate folder starts reindex toast |  |
| PASS | root folder context has SB-10 and SB-11 | 图片 在 Finder 中打开 刷新此文件夹 关闭关注文件夹（保留索引） 从图库移除（删索引，不删文件） |
| PASS | subfolder context has 关闭此文件夹 | 5 月 在 Finder 中打开 刷新此文件夹 关闭此文件夹（不再显示） |
| PASS | import source ctx has hide + remove | 京都备份 仅从分组隐藏 从导入的索引中移除… |
| PASS | viewer shows 正在载入原图 or 正在解码 early |  |
| PASS | viewer back is 返回 |  |
| PASS | viewer vstat has i/n and zoom |  |
| PASS | viewer info button 照片信息 |  |
| PASS | info panel title 照片信息 and EXIF honesty fields |  |
| PASS | viewer chrome-hidden class applies |  |
| PASS | mousemove restores chrome |  |
| PASS | Home selects first non-placeholder |  |
| PASS | createTag requires selection |  |
| PASS | geometry light #/all | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry light #/settings | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry light #/settings/index?tab=tasks | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry light #/help | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry dark #/all | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry dark #/settings | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry dark #/settings/index?tab=tasks | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | geometry dark #/help | {"w":1280,"h":820,"x":60,"y":60,"display":"flex","overflow":"hidden"} side=228 |
| PASS | key screenshots written |  |

## 推荐关键截图（路径相对原型根目录）

- `shots-v1.4/v14-light-home.png` / `v14-dark-home.png` 正常首页亮暗（本轮必交）
- `shots-v1.4/v14-light-more-menu.png` / `v14-dark-more-menu.png` 展开的更多菜单亮暗（本轮必交）
- `shots-v1.4/v14-light-settings-to-index.png` 从设置进入索引管理（本轮必交）
- `shots-v1.4/v14-light-index-tasks.png` 等五标签（另有 dark 套）
- `shots-v1.4/v14-light-viewer-info.png` / `v14-light-viewer-compare.png` 信息栏与并排比较
- `shots-v1.4/v14-light-filters.png` 筛选
- `shots-v1.4/v14-light-import-picker.png` 添加文件夹演示选择器
- `shots-v1.4/v14-light-trash-confirm.png` 废纸篓确认
- `shots-v1.4/v14-light-export-confirm.png` 导出示范 JSON 确认
- `shots-v1.4/v14-light-home-running.png` 索引进行中底栏（无假百分比）
- 总览联系表将另存 `shots-v1.4/v14-contact-sheet.png`（若生成成功）
