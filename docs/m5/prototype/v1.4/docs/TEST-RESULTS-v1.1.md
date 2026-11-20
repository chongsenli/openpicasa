# 交互测试结果 · OpenPicasa 原型 v1.1

- 日期：2026-11-16（Asia/Shanghai）
- 环境：puppeteer-core + Chrome，本地 `file://` 打开 `index.html`
- 本文件为 v1.1 新增用例（滑块、索引进度）。原 v1 核心用例见 `TEST-RESULTS.md`。
- 合计：28 项，通过 28，未通过 0（已在 **v1.2** 上回归）

| 结果 | 用例 | 说明 |
| --- | --- | --- |
| 通过 | idle index visible |  |
| 通过 | slider present 80-280 |  |
| 通过 | grid thumb 97px | {"thumb":97,"w":97,"h":97,"label":"97 px"} |
| 通过 | grid thumb 133px | {"thumb":133,"w":133,"h":133,"label":"133 px"} |
| 通过 | grid thumb 177px | {"thumb":177,"w":177,"h":177,"label":"177 px"} |
| 通过 | grid thumb 219px | {"thumb":219,"w":219,"h":219,"label":"219 px"} |
| 通过 | grid thumb 263px | {"thumb":263,"w":263,"h":263,"label":"263 px"} |
| 通过 | five irregular sizes all distinct |  |
| 通过 | size persists on stars |  |
| 通过 | size persists on album |  |
| 通过 | size persists after refresh |  |
| 通过 | light grid no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | light grid cell=177 |  |
| 通过 | light masonry no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | light masonry height follows slider |  |
| 通过 | light rows no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | light list thumb=177 |  |
| 通过 | dark grid no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | dark grid cell=177 |  |
| 通过 | dark masonry no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | dark masonry height follows slider |  |
| 通过 | dark rows no overflow @177 | {"ok":true,"overflowCells":0,"barH":32,"barOff":false,"winH":820} |
| 通过 | dark list thumb=177 |  |
| 通过 | import running on wizard+bar | {"bar":{"s":"running","p":12,"d":1,"t":8},"wiz":{"s":"running","p":12},"job":{"job":{"id":"job-1794823349796","kind":"import","presetId":"desk","label":"导入 桌面 / 2024 旅行","status":"running","pct":12,"done":1,"total":8,"added":0,"note":"演示索引进行中，不会读取真实文件，也不会写入原照片文件夹。"},"status":"running","total":8,"done":1,"pct":12,"label":"导入 桌面 / 2024 旅行"}} |
| 通过 | progress continues after leaving wizard | {"pctOnImport":12,"onAll":{"s":"running","p":12}} |
| 通过 | settings and status bar share job | {"bar":{"s":"running","p":23,"d":2,"t":8},"detail":{"s":"running","p":23,"d":2,"t":8}} |
| 通过 | theme switch does not reset job | {"theme":"dark","s":"running","p":23} |
| 通过 | complete state correct | {"bar":{"s":"done","p":100,"d":8,"t":8},"detail":{"s":"done","p":100},"text":"索引完成 8 / 8"} |

## 测量依据

- 方图网格使用 `grid-template-columns: repeat(auto-fill, var(--thumb-size))`，单元格宽高等于滑块值（允许 ±1px 取整误差）。已测 97 / 133 / 177 / 219 / 263，均非原 128 / 144 / 192 三档。
- 列表布局缩略图边长同步为滑块像素值。
- 原比例（两端对齐）行高以滑块值为目标高度，行被撑满时高度会按行宽缩放，因此不必等于精确像素，但会随滑块连续变化，而非三档。
- 底栏固定 32px，避免进度或尺寸变化造成跳动。
- 索引为演示数据，不扫描磁盘、不写入原照片。
