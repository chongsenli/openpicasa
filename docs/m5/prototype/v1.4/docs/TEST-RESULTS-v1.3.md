# 原型 v1.3 测试结果

- 时间：2026-11-16（Asia/Shanghai）
- 环境：Puppeteer-core + Google Chrome，离线 `file://`，未运行 Rust 桌面应用。
- **本轮新用例：通过 85 / 共 85，失败 0。** 不凑分。

## 旧 39+28+21 回归说明

不能再称 v1.0（39）、v1.1（28）、v1.2（21）「全部通过」。被正确语义修正、旧断言会失败或已过时的包括：空闲 `20 / 20` 与百分比 / `data-pct`；缩略图 80–280；布局仅「方图 / 原比例 / 列表」；主危险操作写成「从资料库移除」；查看器仅三档缩放；帮助把索引导出写成规划。仍适用的部分（file:// 打开、亮暗主题、设置与索引入口可点、滑块 input 实时改尺寸并持久化、展厅查看器、不读真实磁盘）已在本清单重跑。

## 逐条

| 结果 | 用例 | 说明 |
| --- | --- | --- |
| PASS | file protocol loads |  |
| PASS | OPApp and OPMgmt present |  |
| PASS | home grid has photos | cells=22 |
| PASS | sidebar 最近更改 |  |
| PASS | sidebar 最近导入 kept as extension |  |
| PASS | sidebar settings + index entries |  |
| PASS | sidebar year rows | years=2 |
| PASS | sidebar 导入的索引 |  |
| PASS | thumb slider 22-512 | {"min":"22","max":"512"} |
| PASS | three real layouts in toolbar | grid,waterfall-v,waterfall-h |
| PASS | status button always 索引管理 | 缩略图 144 px 索引管理 |
| PASS | idle status has no 20/20 | 缩略图 144 px 索引管理 |
| PASS | idle status has no percent | 缩略图 144 px 索引管理 |
| PASS | default library has no placeholder cells | ph=0 |
| PASS | slider input updates css + storage | {"before":"144px","after":{"css":"48px","stored":48,"sm":"1"}} |
| PASS | small thumb tight radius flag |  |
| PASS | vertical waterfall class |  |
| PASS | horizontal waterfall / justified |  |
| PASS | list layout not a primary toolbar button |  |
| PASS | search scope control present |  |
| PASS | filter has full types, tag input, apply, size labels | 筛选（可叠加）  只看已收藏  无日期  无拍摄信息 类型 全部 JPEG PNG GIF WebP TIFF SVG HEIC AVIF RAW 尺寸 全部 小图 (≤640px) 中图 (640–1920px) 大图 (>1920px) 标签 按此标签筛 相机 / 镜头（示例元数据，须点应用） 应用 文件大小 (KB) ～ 应用 清除全部筛选 |
| PASS | sort has date/name/mtime/size/dim + dir + thumbsFirst | 排序 拍摄日期 ✓ 文件名 修改日期 文件大小 图片尺寸 改为降序 ✓ 改为升序 有缩略图的排在前面 |
| PASS | more menu has 添加文件夹 and list extension | 添加文件夹… 包含子文件夹 ✓ 显示文件名 ✓ 列表布局（演示扩展） 设置… ⌘, 索引管理… 帮助与快捷键 ? 切换到暗色 关于 OpenPicasa 恢复演示数据 |
| PASS | 最近更改 route |  |
| PASS | status bar opens index manager |  |
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
| PASS | running status shows 已处理 without percent/ETA | 缩略图 144 px 索引管理 3 已处理 2 张 · 已用 00:00 |
| PASS | system-pause copy present and no per-task continue |  |
| PASS | folder picker is demo list not OS dialog |  |
| PASS | duplicate folder toast, no second root |  |
| PASS | denied folder does not ingest (IM-03) |  |
| PASS | trash confirm mentions real product moves files and prototype simulates |  |
| PASS | folder context available |  |
| PASS | dblclick opens exhibition viewer |  |
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

- `shots-v1.3/v13-light-home.png` / `v13-dark-home.png` 首页亮暗
- `shots-v1.3/v13-light-index-tasks.png` 等五标签（另有 dark 套）
- `shots-v1.3/v13-light-viewer-info.png` / `v13-light-viewer-compare.png` 信息栏与并排比较
- `shots-v1.3/v13-light-filters.png` 筛选
- `shots-v1.3/v13-light-import-picker.png` 添加文件夹演示选择器
- `shots-v1.3/v13-light-trash-confirm.png` 废纸篓确认
- `shots-v1.3/v13-light-export-confirm.png` 导出示范 JSON 确认
- `shots-v1.3/v13-light-home-running.png` 索引进行中底栏（无假百分比）
- 总览联系表将另存 `shots-v1.3/v13-contact-sheet.png`（若生成成功）
