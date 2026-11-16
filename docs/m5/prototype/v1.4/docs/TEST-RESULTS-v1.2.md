# 交互测试结果 · OpenPicasa 原型 v1.2

- 日期：2026-11-16（Asia/Shanghai）
- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`
- 本文件为 v1.2「索引管理」页面与入口用例。v1 核心 39 项与 v1.1 滑块/进度 28 项另见对应文档。
- 合计：21 项，通过 21，未通过 0

| 结果 | 用例 | 说明 |
| --- | --- | --- |
| 通过 | sidebar has settings group |  |
| 通过 | status bar opens index manager |  |
| 通过 | index page has back to settings |  |
| 通过 | index page details + sources |  |
| 通过 | crumb returns to settings |  |
| 通过 | settings has index entry + summary |  |
| 通过 | settings button same route |  |
| 通过 | keyboard on status bar opens index |  |
| 通过 | sidebar subitem same route |  |
| 通过 | slider 97 still exact |  |
| 通过 | slider persists on index page |  |
| 通过 | slider persists back to library |  |
| 通过 | running job opens manager without reset | {"midBar":{"s":"running","p":17,"d":1,"t":8},"midPage":{"hash":"#/settings/index","page":{"s":"running","p":17,"d":1,"t":8},"bar":{"s":"running","p":17},"title":"索引管理","task":true}} |
| 通过 | progress continues on manager page | p1=17 p2=46 |
| 通过 | settings summary matches bar mid-job | {"s":"running","bp":59,"dp":59} |
| 通过 | return to library mid-job still running |  |
| 通过 | job completes after navigation |  |
| 通过 | rebuild starts from manager |  |
| 通过 | rebuild completes on manager |  |
| 通过 | dark theme on manager |  |
| 通过 | theme restore light |  |

## 说明

- 索引管理路由为 `#/settings/index`，属于设置下级。底栏索引进度整区为链接，可用鼠标或键盘（Tab + Enter）打开。
- 导入/重建进度与底栏、设置摘要、管理页共用 `indexJob`，切页不会中断。
- 暂停/继续为管理页提供的演示控制，会真实停止或恢复同一任务。
