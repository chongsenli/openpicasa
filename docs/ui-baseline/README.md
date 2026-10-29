# 界面还原基线（M1）

这三张是**当前实现的真实渲染**，不是设计稿、不是参考图。用途只有一个：
把「界面还原」从主观判断变成可比对的证据（03 §5A）。

| 文件 | 内容 | 生成方式 |
| --- | --- | --- |
| `m1-library-1280x820.png` | 图库视图（日期分组 + 6 列栅格 + 左栏树 + 状态栏） | `.app` 内可执行文件 + `OPENPICASA_SCREENSHOT` |
| `m1-viewer-1280x820.png` | 查看器（暗底、EXIF 面板、上下信息条） | 同上，另加 `OPENPICASA_OPEN_INDEX=4` |
| `m1-empty-1280x820.png` | 空库首屏（空态文案 + 置灰按钮） | 同上，不预置 roots |

## 生成与复核

```bash
# 1) 造可复现的样本（系统壁纸转 jpg，日期分组天然分散）
# 2) 预置偏好，指向样本目录
# 3) 截图（窗口帧缓冲，不含桌面其他窗口）
OPENPICASA_DATA_DIR=/tmp/op-data \
OPENPICASA_SCREENSHOT=$PWD/docs/ui-baseline/m1-library-1280x820.png \
OPENPICASA_SCREENSHOT_WARMUP=55 \
  dist/OpenPicasa.app/Contents/MacOS/OpenPicasa

# 4) 量数值（不要靠眼睛）
python3 tools/pngprobe.py docs/ui-baseline/m1-library-1280x820.png --row 300 --scale 2
```

## 已实测对齐的硬数值（1280×820，Retina 2x 截图）

| 项 | 实测 | 来源常量 |
| --- | --- | --- |
| 工具栏高度 / 底色 | 68 / `#F0F0F0` | `Metrics::TOOLBAR_H` / `Theme::chrome_bg` |
| 左栏宽度 / 底色 | 220 / `#F5F5F5` | `Metrics::SIDEBAR_W` / `Theme::sidebar_bg` |
| 选中行底色 | `#3875D7` | `Theme::sidebar_selected` |
| 栅格底色 / 起点 x | `#EBEBEB` / 236（=220+16） | `Theme::grid_bg` / `Metrics::GRID_PAD` |
| 单元格边长 / 间距 | 144 / 10（行距 154） | `Metrics::CELL_SIZES[1]` / `CELL_GAP` |
| 列数 | 6（content_w=1052） | 由宽度推导，非固定 |
| 分组头高度 / 底色 | 26 / `#D8D8D8` | `Metrics::GROUP_HEADER_H` / `Theme::header_bg` |
| 滚动条宽度 | 8 | `Metrics::SCROLLBAR` |
| 提示框底边距窗口底 | 48 | `draw_overlays` 内常量 |

## 还缺什么

1. **原版 Picasa 3.9 的对照截图**：M1 不做数据兼容，但仍然需要视觉基准图。
   目前 `theme.rs` 里的配色与尺寸是目测近似值，逐项标了 `TODO(U-xx)`，
   等 PM 提供原版截图后按上表逐项回填。
2. 这三张图只是**当前状态**，不是验收目标。回填原版数值后需要重新生成一次。
