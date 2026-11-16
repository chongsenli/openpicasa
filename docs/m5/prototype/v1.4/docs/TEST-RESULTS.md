# 交互测试结果 · OpenPicasa 原型 v1

- 日期：2026-11-16（Asia/Shanghai）
- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`
- 合计：39 项，通过 39，未通过 0（已在 **v1.2** 上回归）

| 结果 | 用例 | 说明 |
| --- | --- | --- |
| 通过 | load home | cells |
| 通过 | nav recent | 最近导入 |
| 通过 | nav stars |  |
| 通过 | nav folder May |  |
| 通过 | search 河谷 | n=2 |
| 通过 | clear search |  |
| 通过 | search no results |  |
| 通过 | clear filters restores grid |  |
| 通过 | single select |  |
| 通过 | multi select 3 | 21-spring,08-flower,07-city |
| 通过 | add to album 海边 | 4->7 |
| 通过 | create album dialog |  |
| 通过 | create album named 原型相册 | albums=京都之行,海边,静物,原型相册 |
| 通过 | import step1 |  |
| 通过 | import step2 confirm |  |
| 通过 | import index complete |  |
| 通过 | import result in recent |  |
| 通过 | viewer open |  |
| 通过 | viewer next | #/viewer/19-fog -> #/viewer/02-lake |
| 通过 | viewer prev |  |
| 通过 | info closed by default |  |
| 通过 | info panel opens and pushes |  |
| 通过 | info panel closes |  |
| 通过 | viewer back to library |  |
| 通过 | rating 5 persists in store |  |
| 通过 | rating persists after reload |  |
| 通过 | theme dark |  |
| 通过 | theme persists on settings |  |
| 通过 | theme persists on help |  |
| 通过 | theme persists on library |  |
| 通过 | theme back to light |  |
| 通过 | settings page |  |
| 通过 | empty library state |  |
| 通过 | reset confirm dialog |  |
| 通过 | reset restores photos |  |
| 通过 | offline / disconnected state |  |
| 通过 | permission denied state |  |
| 通过 | unimplemented explains itself |  |
| 通过 | no broken images at end |  |

## 未覆盖或限制

- 触控板框选与边缘自动滚动未实现（文档登记为非目标/原型未做）。
- 索引导出包、真实磁盘监视、备份恢复未作为可运行能力提供；点击会提示说明。
- 导入向导使用演示来源，不读取真实用户文件。
- 拍摄信息除本地文件宽高与体积外均为界面示例。
- 本测试在无障碍/屏幕阅读器、多窗口尺寸方面未做完整覆盖。
