# OpenPicasa

用 Rust 写的桌面照片管理器，**界面与交互对齐 Google Picasa 3.9**，但**数据格式完全自立**——不读、不写、不解析 `.picasa.ini`。

一句话说明它与 Picasa 的关系：**像，但不通。**
视觉布局、栅格密度、操作手感、快捷键向 3.9 靠拢；而照片库用什么格式记，是我们自己的事。

- **界面基准**：Picasa 3.9（Windows 最终版）——仅作视觉与交互参照，不追求数据互通。
- **当前阶段目标（M1）**：**在 Apple Silicon（M 系列）Mac 上安装一个能跑起来的 OpenPicasa，用于浏览照片。**
- **M1 目标平台**：仅 macOS 14+ / arm64。Windows / Intel Mac 在 M3 之后。
  **Linux x86_64 已提前打通**（0.1.0 起可直接构建并产出 .deb，见「快速开始」），
  但只在 Ubuntu 26.10 上验证过，不作为 M1 的交付承诺。

> ### 📌 先读这条：产品定位与三层承诺
>
> | | 内容 |
> | --- | --- |
> | ✅ **承诺** | 界面布局、栅格密度、交互手感、快捷键与 Picasa 3.9 对齐 |
> | ✅ **承诺** | **照片目录零写入**——不创建、不修改、不删除、不改权限、不改 mtime 任何文件 |
> | ❌ **不承诺** | `.picasa.ini` 兼容、与原版数据互通、读取原版的编辑/星标/说明 |
>
> 第三行不是「暂时没做」，是**产品定义本身**。如果有人期待「装上就能看到我原来的星标」，那是对本项目的误解。

> ### 📌 M1 分期策略
>
> **M1 = 界面复刻 + 浏览；M2 = 编辑内核；M3 = 长尾与跨平台。**
>
> 这不是「砍功能」，是**刻意的排序**：用户判断一个复刻品「像不像」只花 3 秒，而这 3 秒里他能看到的只有界面。所以先把「看起来是 Picasa」做出来，再把「用起来是 Picasa」补上。详见 `docs/01` §11.5.0。
>
> **M1 交付物是一个 `.app`**，不是「一堆能跑的功能」。这意味着打包、签名、TCC 授权这些「能不能装上去」的问题，在 M1 里的优先级高于任何算法。

### M1 的三条红线（不可协商）

| 红线 | 含义 |
| --- | --- |
| **R-1** | **照片目录零写入**。v3.0 起**不再读写 `.picasa.ini`**（用户决策：「做一个新的程序」）——不是「小心不写」，而是**根本没有 ini 代码**（crate 已删除）。数据只落自家 `Application Support` 与 `Preferences`。 |
| **R-2** | **编辑室入口不存在**（不是置灰）。M1 的 `picasa-edit` crate 不创建。 |
| **R-3** | **装得上、打得开、首次授权走得通**。干净 Mac 上双击可运行、拖进「应用程序」可运行、TCC 授权流程正确。 |

> R-1 的产品含义：「你可以把任何照片目录直接拿进来试，**我们只读、不改**。退出后你的文件、文件夹、任何隐藏文件都不会有任何变化。」详见 `docs/02` §5.2、`docs/01` §11.5.3。
>
> **v3.0 的额外收益**：不再需要为「字节级 ini 往返保真」写任何代码。02 §0.1.4 有完整的「风险下降」说明（可直接对外汇报）。

> 文档状态：三份设计文档已冻结（01 v3.0 / 02 v3.0 / 03 v2.1），**M1 代码已落地并可打包**（见下节）。
> 「还差什么」放在文末的进度表里，不在这里重复。

---

## 当前界面（M1 实机渲染，1280×820）

下面两张不是设计稿，是**本程序自己窗口帧缓冲的导出**（`crates/picasa-ui/src/capture.rs`），
所以有排版问题就会直接暴露出来，不能靠「示意图」蒙混。

<img src="docs/ui-baseline/m1-library-1280x820.png" width="720" alt="图库视图：顶部工具栏 + 左栏收藏/文件夹树/托盘 + 日期分组 6 列栅格 + 底部状态栏">

<img src="docs/ui-baseline/m1-viewer-1280x820.png" width="720" alt="查看器：暗底、等比缩放、EXIF 面板、上下自动隐藏信息条">

更多基线（含空库首屏）与**实测几何数值表**见 [`docs/ui-baseline/`](docs/ui-baseline/README.md)。
注意其中区分：**本程序自渲染的截图入库**，**原版 Picasa 的对照截图永远不入库**（版权，走内部渠道，见 03 §5A）。

---

## 目录

| 路径 | 内容 | 负责人 |
| --- | --- | --- |
| `docs/01-产品功能清单.md` | 产品功能域、功能点、优先级、验收要点（PRD）；**v3.2**（数据自立：不读写 `.picasa.ini`；M1 = 界面复刻 + 浏览，43 条冻结 + 44 项界面还原；**M2 = 索引底座 + 编辑内核双主线**） | PM |
| `docs/02-技术架构与选型.md` | 技术架构、crate 选型、**§5 自有元数据模型**、**§7 macOS 打包与 TCC**、**§8 界面壳层**；**v3.0**（数据自立，ini 契约已作废归档） | Dev / Tech Lead |
| `docs/03-测试策略与验收计划.md` | 测试分层、**界面还原度截图对拍**、**macOS 安装运行测试**、**照片目录零写入测试**、**自有数据模型测试**、性能门禁；**v2.2**（202 条用例：M1 有效 95 / M2 恢复 60 / 前提消除 26 / M2 新增 21） | QA |
| `docs/04-M2-需求说明书-关注文件夹与索引.md` | M2「关注文件夹 + 后台异步缩略图索引」需求说明书：基线 M2-FD-01～28，本轮增补 **右下角进度弹框 + 索引进度查看入口**（M2-FD-29～44）、**四处状态一致性**、6 步验收脚本、需求→`TC-IDX-001～021` 追溯；**v1.3** | PM |
| `README.md` | 项目总览、前置条件、环境准备、仓库骨架 | Dev / Tech Lead |

> **M1 范围指针（新人先读这条）**：M1 的交付范围见 [`docs/01-产品功能清单.md`](docs/01-产品功能清单.md) **§11.5「M1 范围冻结与界面还原清单」**——**43 条冻结功能点 + 174 条明确不进 M1 + 44 项界面还原清单 + 3 条红线**。**§11.5 具约束力**，与 01 §11 的里程碑建议冲突时以它为准；M1 期间任何新增范围须走变更评审。
>
> **技术方案对应**：[`docs/02-技术架构与选型.md`](docs/02-技术架构与选型.md) 的 **§7（macOS 交付与打包）**、**§8（界面壳层）**、**§9.3（M1 技术拆解）**。
>
> **M2 索引范围指针**：[`docs/04-M2-需求说明书-关注文件夹与索引.md`](docs/04-M2-需求说明书-关注文件夹与索引.md) —— 关注文件夹 + 后台异步缩略图索引（M2-FD-01～28）与**索引进度的可见性**（右下角弹框、进度查看面板，M2-FD-29～44）。基线红线不变：照片目录零写入、不读写 `.picasa.ini`、异步不阻塞界面。

---

## ⚠️ 前置条件

### 0. 🚨 头号阻塞：本机没有 Rust 工具链

> ## 不装 rustup，本项目的**一切讨论都是纸上谈兵**。
>
> **这不是「无法编译验证」那么轻。** 本项目 M1 的**唯一交付物是一个能双击运行的 `OpenPicasa.app`**。没有 `cargo`，就没有 `.app`；没有 `.app`，就没有任何可演示的东西。文档写得再完整、评审开得再多，**屏幕上一个像素都产不出来**。
>
> 当前状态（2026-10-27 实测）：`cargo` / `rustc` / `rustup` / `~/.cargo` **全部不存在**。
>
> 所以：**在 rustup 装上之前，任何关于进度、排期、演示的承诺都没有依据。** 这是唯一一个「不解决就无法向前」的事项，其余所有问题（选型、范围、还原度）都排在它后面。
>
> 解除方式：见下方安装命令，**需 Lead / 用户批准**（安装系统级工具不由 dev 自行决定）。
>
> 配套的第二项：`xcode-select -p` **尚未确认**。缺 Command Line Tools 时连 `cc`/`ld` 都没有，`.app` 链接不出来——**这项和 rustup 一样是硬门槛**。

### 1. Rust 工具链详解

**当前这台开发机的实际状态（已核实）**：

| 检查项 | 结果 |
| --- | --- |
| `cargo` / `rustc` | ❌ 不存在（`which` 返回空） |
| `rustup` | ❌ 不存在 |
| `~/.cargo/` | ❌ 不存在 |
| `$HOME/.rustup/` | ❌ 不存在 |
| Xcode Command Line Tools | 需先确认 `xcode-select -p` 有输出 |
| Node.js | ✅ v24.18.0 可用 |
| Homebrew | ✅ `/opt/homebrew/bin/brew` |
| 系统 | macOS 27.2，Apple Silicon（arm64） |

**因此：当前所有文档都不包含「已编译验证」的结论。** 任何 crate 版本、API 用法、性能数字都必须在工具链就绪后重新验证。文档中出现的 `Cargo.toml` 片段是**示例片段**，不是可编译的最终配置。

### 2. 安装方式（需 Lead 批准后执行）

> 约定：**不自行安装系统级工具**。安装 rustup 属于系统级环境变更，执行前必须经 Tech Lead 明确批准，并由批准者确认是否走 `brew` 还是官方 `rustup` 脚本。

推荐使用官方 rustup（不污染 Homebrew 的 prefix，便于 `rustup update` 与多工具链切换）：

```bash
# 1) 安装 rustup + 稳定版工具链（默认 host triple = aarch64-apple-darwin）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile default

# 2) 让当前 shell 生效
source "$HOME/.cargo/env"

# 3) 校验
rustc -V && cargo -V && rustup show

# 4) 补齐常用目标与组件（跨平台发布需要）
rustup target add aarch64-apple-darwin x86_64-apple-darwin \
                  x86_64-pc-windows-msvc x86_64-unknown-linux-gnu
rustup component add rustfmt clippy rust-src llvm-tools-preview
```

备选（若组织策略要求统一用 Homebrew 管理）：

```bash
/opt/homebrew/bin/brew install rustup-init && rustup-init -y
# 注意：brew 的 rust 包与 rustup 会争抢 $PATH，二者只装其一
```

**工具链固定策略**：workspace 根目录放 `rust-toolchain.toml`，锁定 channel 与组件，保证本机/CI/其他开发者一致。

```toml
# rust-toolchain.toml（示例）
[toolchain]
channel    = "stable"          # 首次可用后改为具体版本，如 "1.95.0"，避免漂移
components = ["rustfmt", "clippy", "llvm-tools"]
targets    = ["aarch64-apple-darwin"]
profile    = "default"
```

> 关于 channel 的说明：调研时发现 egui/eframe 0.37 与 ort 2.0 的 MSRV 已到 **Rust 1.92 / 1.95**（见 `docs/02` 选型表），因此不要用太旧的 stable。首次安装后请把 `channel` 钉到实际可用的具体版本号。

### 3. 目标三元组（M1 只需一个）

| 用途 | Triple | M1 |
| --- | --- | --- |
| **M1 唯一目标**：Apple Silicon Mac | **`aarch64-apple-darwin`** | ✅ **只装这个** |
| macOS Intel / Universal | `x86_64-apple-darwin` or `lipo` | ⏸ M3 |
| Windows | `x86_64-pc-windows-msvc` | ⏸ M3 |
| Linux | `x86_64-unknown-linux-gnu` | ✅ 可构建（0.1.0 起产出 .deb，见「快速开始」） |

```bash
# M1 只需要这一条
rustup target add aarch64-apple-darwin
```

> **M1 出单架构的理由**：用户目标是 M 系列 Mac。universal2 会让构建时间与体积翻倍，对 M1 无收益。M3 若需要，`lipo -create` 合并即可。详见 `docs/02` §7.5。

### 4. 系统级依赖（非 Rust，需单独评估）

部分选型需要 C/C++ 库或额外二进制，**安装前同样需要批准**：

| 依赖 | 用途 | 备注 |
| --- | --- | --- |
| Xcode CLT | `cc` / `ld` / 链接 macOS framework | 必需 |
| `pkg-config` + `libheif` | HEIC 解码（`libheif-rs`） | 可选，M3 之后再上 |
| `exiftool`（外部二进制） | IPTC/XMP 写入的兜底路径 | 可选，替代方案见 `docs/02` |
| `libvips` | 超大图缩放（如走 `vips` 绑定路线） | **不推荐作为主路径**，见 `docs/02` |

---

## 建议的 workspace 骨架（**M1 只建 4 个 crate**）

> 这是**目录与 Cargo 配置的规划**。**现状：前四步已落地**（Cargo.toml / 四个 crate / packaging / tests），
> 差异只有两处，均为有意：①`picasa-ui/src` 用**平铺模块**（`app.rs` / `grid.rs` / `toolbar.rs` /
> `sidebar.rs` / `statusbar.rs` / `viewer.rs` / `theme.rs` / `icons.rs` / `textures.rs` /
> `shortcuts.rs` / `macos.rs` / `capture.rs`）而不是子目录——M1 每个模块只有一两个文件，
> 子目录只会多一层路径；②`deny.toml` 尚未落地（许可审计在 M1 交付前补，`cargo-deny` 未安装）。
>
> **M1 的一个重要决策：减少 crate 数量。** v1.x 规划了 9 个 crate，M1 只建 **4 个**。
> v3.0 起 `.picasa.ini` 兼容层整体移除，`picasa-ini` crate **已删除**；`picasa-edit`、`picasa-face`、`picasa-ipc`、`picasa-cli` 在 M1 也不创建。
> **少一个 crate 就少一处可能出错的地方**，这是降低 M1 风险最直接的手段。

```text
openpicasa/
├── Cargo.toml                 # workspace 根（virtual manifest）
├── Cargo.lock                 # 提交进仓库（应用型项目）
├── rust-toolchain.toml
├── rustfmt.toml
├── clippy.toml
├── deny.toml                  # cargo-deny：许可与安全审计
├── README.md
├── docs/
│   ├── 01-产品功能清单.md      # v3.0
│   ├── 02-技术架构与选型.md     # v3.0
│   ├── 03-测试策略与验收计划.md  # v2.1
│   ├── adr/                   # 架构决策记录（已写 0001-0006）
│   └── ui-baseline/           # 界面还原基准截图（3 张自渲染图已入库，见其 README）
├── packaging/                 # 手写打包（ADR-0003，不用 cargo-bundle）
│   ├── Info.plist             # 模板
│   ├── bundle.sh              # cargo build → 组 .app → icns → plutil → codesign
│   ├── logo.png               # 品牌图标源图（1024×1024 白底圆徽标）
│   ├── make_icons.py          # logo.png → 裁圆/透明/多尺寸 PNG（纯标准库）
│   └── make_icon.py           # .iconset + icns（无 logo.png 时回退自绘）→ iconutil
├── crates/
│   ├── picasa-db/             # ✅ M1：SQLite 索引 + 缩略图 BLOB + 迁移（叶子 crate）
│   ├── picasa-imaging/        # ✅ M1：解码 + Orientation + 缩略图（无效果算子）
│   ├── picasa-core/           # ✅ M1：扫描/监视/库会话/选择集/搜索/任务队列
│   ├── picasa-ui/             # ✅ M1：eframe 应用（五区面板 + 栅格 + 查看器）
│   │   ├── assets/logo-256.png  # 窗口图标（make_icons.py 从 packaging/logo.png 生成）
│   │   └── src/
│   │       ├── theme.rs       # 配色常量
│   │       ├── fonts.rs       # 系统字体（eframe system_fonts）；Plan B 才是内嵌子集
│   │       ├── grid/          # 虚拟布局 + sticky 头 + 纹理缓存
│   │       ├── panels/        # toolbar / library_tree / tray / statusbar
│   │       ├── viewer/        # 暗色全屏查看器
│   │       └── macos/         # rfd 对话框 + TCC 流程 + 全屏
│   ├── picasa-edit/           # ⛔ M1 不创建（M2）
│   ├── picasa-face/           # ⛔ M1 不创建（M2）
│   ├── picasa-ipc/            # ⛔ M1 不创建（M3）
│   └── picasa-cli/            # ⛔ M1 不创建（M2 调试期）
├── assets/
│   ├── icons/                 # （空：图标改为 egui 矢量自绘，见 docs/adr/0004）
│   ├── fonts/                 # （空：M1 不内嵌字体；仅 Plan B 时放 1500 字子集）
│   ├── OpenPicasa.icns
│   ├── models/                # M2
│   └── geonames/              # M2
├── tests/
│   ├── golden/                # 黄金图片（Git LFS）
│   └── perf/                  # 性能基准场景脚本
└── xtask/                     # 打包 / mem-report（字体子集生成仅 Plan B 需要）
```

### 根 `Cargo.toml` 关键片段（示例）

```toml
# 示例片段，非最终配置
[workspace]
resolver = "3"
members = [
    "crates/picasa-db",
    "crates/picasa-imaging",
    "crates/picasa-core",
    "crates/picasa-ui",
    # M1 不含：picasa-edit / picasa-face / picasa-ipc / picasa-cli
]

[workspace.package]
version      = "0.1.0"
edition      = "2024"
rust-version = "1.95"          # 受 eframe 0.37 的 MSRV 约束
license      = "MIT OR Apache-2.0"

[workspace.dependencies]
# 依赖版本集中管理，子 crate 只写 { workspace = true }
image        = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "tiff", "bmp"] }
zune-jpeg    = "0.5"           # 缩略图 JPEG 快路径（缩放解码）
kamadak-exif = "0.6"           # 只读 EXIF（Orientation / 拍摄参数）
rusqlite     = { version = "0.41", features = ["bundled"] }
rayon        = "1.12"
notify       = "8.2"
serde        = { version = "1", features = ["derive"] }

# ---- GUI（M1 的核心）----
egui         = "0.37"
eframe       = { version = "0.37", default-features = false,
                 features = ["wgpu", "persistence", "default_fonts"] }
egui_extras  = "0.37"

# ---- macOS 专有（M1 新增）----
rfd          = "0.17"          # 原生 NSOpenPanel（TCC 必需）
objc2        = "0.6"
fontdb       = "0.24"          # 字体回退链探测

tracing      = "0.1"
thiserror    = "2"

# ---- M1 显式不引入 ----
# rawler / rawloader  (LGPL, M2)
# tract-onnx / ort    (M2)
# little_exif / img-parts (M2，M1 不写元数据)

[features]
# M1 无特殊 feature；ini 相关能力已随 crate 删除
[features]
default = []

[profile.release]
lto           = "thin"
codegen-units = 1
panic         = "abort"
strip         = "symbols"      # 控制 .app 体积（M1-P11 ≤ 30MB）

# 图像处理是计算密集型，debug 下也要能跑得动
[profile.dev.package."*"]
opt-level = 2
```

### 子 crate 依赖方向（**M1：5 个 crate，无编辑**）

```text
picasa-ui ──┬──> picasa-core ──> picasa-db
            │                └─> picasa-imaging
            └──> picasa-imaging
```

规则：

- **`picasa-db` 是叶子 crate**，不依赖任何其他本地 crate（保证可独立测试与迁移）。**`picasa-ini` 已删除**（v3.0）。
- `picasa-imaging` 不依赖 `picasa-db`；缓存与索引的协调放在 `picasa-core`。
- `picasa-ui` 是**唯一依赖 `eframe`** 的 crate。
- **依赖图里没有 `picasa-edit`——因为在 M1 它不存在。** 这是红线 R-2 的结构性保证。

---

## 快速开始

```bash
# 0) 国内网络提醒：rustup 走 USTC 镜像装工具链（官方源实测 ≈40KB/s，会卡死）
RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rust-static sh <(curl -sSf https://sh.rustup.rs) -y
# cargo 侧镜像已写进 .cargo/config.toml（本地加速用，不入库）

# 1) 开发运行
cargo run --release --bin OpenPicasa

# 2) 构建 .app（M1 的交付物）
./packaging/bundle.sh
# 产物：dist/OpenPicasa.app（arm64 + ad-hoc 签名）

# 3) 验证签名与体积
codesign -dv --verbose=2 dist/OpenPicasa.app   # 期望 Signature=adhoc / Identifier=app.openpicasa
du -sh dist/OpenPicasa.app                      # M1 目标 ≤ 30MB（当前 ≈12MB）

# 4) 质量门禁（当前全绿）
cargo test --workspace
cargo clippy --workspace --all-targets          # 0 warning
```

### 在 Linux 上构建（x86_64）

M1 的交付物是 macOS 的 `.app`，但代码本身在 Linux 上也能编通（2026-10 起已验证）。

```bash
# 依赖：构建期一个都不需要装。wayland / xkbcommon / vulkan 这些 GUI 库
# 全是运行时 dlopen 的，所以不需要 pkg-config，也不需要任何 -dev 包。
cargo build --release --bin OpenPicasa     # → target/release/OpenPicasa
./run.sh                                    # 直接启动

# 打成 .deb
./packaging/make_deb.sh                     # → dist/openpicasa_<version>-1_amd64.deb
sudo apt install ./dist/openpicasa_0.1.0-1_amd64.deb
```

三个平台差异要知道：

- **glibc 下限跟着构建机走**。二进制在当前机器的 glibc（2.44）上链接，control 里写的是
  `Depends: libc6 (>= 2.44)`，**只能装 Ubuntu 26.10+**。要给老发行版用得换更旧的构建环境
  （容器）或 musl 目标——改 control 里的版本号是没用的，符号就是不够。
- **数据目录仍按 macOS 惯例**落在 `~/Library/Application Support/app.openpicasa/`，
  XDG 化排在 M3。属于已知差异，不影响功能。
- **别把 `TMPDIR` 留在 `/tmp`**：部分发行版把 `/tmp` 挂成 10MB tmpfs，
  `libsqlite3-sys` 编译 `sqlite3.c` 的中间文件会 ENOSPC（debug profile 带 `-g` 时尤其容易）。
  `packaging/make_deb.sh` 自己的临时目录也因此放在输出目录旁边而不是 `/tmp`。

### 调试与自动化入口（只读，不碰照片目录）

| 环境变量 | 作用 |
| --- | --- |
| `OPENPICASA_DATA_DIR=<dir>` | 把索引/偏好/日志重定向到 `<dir>`（便携模式、测试、截图基线都用它） |
| `OPENPICASA_SCREENSHOT=<png>` | 跑到第 N 帧后截取**本窗口**帧缓冲、写 PNG、自动退出（见 ADR-0006） |
| `OPENPICASA_SCREENSHOT_WARMUP=<n>` | 截图前预热帧数，默认 40（缩略图每帧只上传 3 张，首帧必是占位灰） |
| `OPENPICASA_OPEN_INDEX=<n>` | 启动后自动打开第 n 张的查看器（查看器只能双击进入，没有它就拍不到查看器） |
| `OPENPICASA_LOG=debug` | 打开调试日志（**默认 release 只记 info**：栅格每帧一条 debug，开着等于持续写盘） |

配套的像素探针（纯标准库，不需要装任何东西）：

```bash
python3 tools/pngprobe.py shot.png --row 600 --scale 2     # 沿一行量出栏宽/单元格/间距
python3 tools/pngprobe.py shot.png --rect 1180,1495,1380,1540  # 某区域主色
```

> ⚠️ **`open dist/OpenPicasa.app` 不会继承 shell 环境变量**（macOS 的 `open --env` 实测不生效），
> 所以上面这几个入口要作用在 `.app` 里那份可执行文件上：
> `dist/OpenPicasa.app/Contents/MacOS/OpenPicasa`。用 `open` 启动则是纯交互模式。

---

## 📦 安装 Picasa（macOS，Apple Silicon）

> 这一节的内容**必须随 `.app` 一起交付**（01 v3.0 F10-26 要求）。打包时把本节复制为 `Picasa-安装说明.txt` 放进 `.app` 同级目录或 `Contents/Resources/`。

1. 把 `OpenPicasa.app` 拖进「应用程序」文件夹。
2. **首次打开如果提示「无法打开，因为 Apple 无法验证此 App 是否包含恶意软件」**：
   - 到「应用程序」里**右键点击 Picasa → 打开**，在弹出的对话框里再点一次「打开」；
   - 或打开「系统设置 → 隐私与安全性」，在页面底部找到关于 Picasa 的提示，点「仍要打开」。
3. 以上只需做一次。

**为什么会有这个提示？** 这是团队内部测试版，使用**本地 ad-hoc 签名**、未向 Apple 申请公证（公证需要付费开发者账号）。**这不代表 App 有问题。**

**如果你是通过 U 盘或直接拷贝拿到它的，通常不会看到这个提示**——quarantine 属性取决于文件怎么传过来的，不是签名决定的。

仍然打不开时，可在「终端」执行：

```bash
xattr -d com.apple.quarantine /Applications/OpenPicasa.app
```

> ⚠️ **不要**关闭系统完整性保护或全局关闭 Gatekeeper（`spctl --master-disable`）——那是安全风险，我们不建议也不支持。

### 首次运行会请求目录访问权限（macOS TCC）

macOS 会保护「图片」「桌面」「下载」「文稿」与外接盘。**第一次添加这些位置的文件夹时，系统会弹窗询问授权——这是正常且必需的。**

- 点「允许」后正常使用；**点「不允许」应用不会崩溃**，会提示你可以去「系统设置 → 隐私与安全性 → 文件与文件夹」重新开启。
- 我们**不请求全盘访问**，只在你自己选择某个目录时才请求那个目录。

### 卸载

把 `OpenPicasa.app` 拖到废纸篓，然后删除数据目录：

```bash
rm -rf ~/Library/Application\ Support/app.openpicasa
```

（索引与缩略图缓存在那里。删了不影响你的照片原文件。）

---

## 当前进度与下一步

| 阶段 | 状态 |
| --- | --- |
| 产品功能清单（docs/01） | ✅ **v3.2**（数据自立；M1 43 条冻结 + 174 条不进 + 44 项界面还原 + 3 红线；**M2 改双主线：索引底座 + 编辑内核**） |
| 技术架构与选型（docs/02） | ✅ **v3.0**（§5 自有元数据模型 + §7 macOS 打包 + §8 界面壳层；ini 契约已归档） |
| 测试策略（docs/03） | ✅ **v2.2**（202 条用例：M1 有效 95 / M2 恢复 60 / 前提消除 26 / M2 新增 21 / 永久废弃 0；含索引进度可见性 21 条） |
| M2 需求说明书（docs/04） | ✅ **v1.3**（关注文件夹 + 异步索引；增补进度弹框与索引进度入口 `M2-FD-29～44`、四处状态一致性、6 步验收脚本、追溯到 `TC-IDX-001～021`） |
| **Rust 工具链** | ✅ **已就绪**：rustc/cargo 1.99.1（USTC 镜像安装，官方源会卡死） |
| Xcode Command Line Tools | ✅ 已确认（`iconutil`/`codesign`/`plutil` 均可用，打包链路依赖它们） |
| cargo workspace 落地 | ✅ 4 个 crate：`picasa-db` / `picasa-imaging` / `picasa-core` / `picasa-ui`（**未建** edit/face/ipc/cli） |
| macOS `.app` 打包链路 | ✅ `packaging/bundle.sh` → `dist/OpenPicasa.app`（arm64、ad-hoc、12MB） |
| 测试与门禁 | ✅ `cargo test --workspace` 62 passed；`cargo clippy` 0 warning |
| 界面还原数值 | 🟡 主几何已实测对齐（栏宽/单元格/间距/滚动条/分组头），**细节项仍待 PM 用原版截图回填**（`theme.rs` 里标 `TODO(U-xx)`） |
| M0.3 TCC 跨重启 / M0.4 字体首帧 / M0.5 栅格 ≥55fps | ⏸ 需真机交互验证（本轮只完成了「起窗口 + 首帧正常」） |

**已落地的架构决策**（`docs/adr/`）：0001 四 crate 分层 · 0002 schema 两处有意偏差（不建 FTS5、不加预留列）·
0003 手写 bundle 脚本 · 0004 图标矢量自绘（不提取原版位图）· 0005 栅格按行号虚拟化 ·
0006 界面还原用自截图 + 像素探针。

**M1 的 5 个 Go/No-Go 前置**（M0 阶段必须全部验证，详见 `docs/02` §9.2）：

| # | 验证项 | 现状 |
| --- | --- | --- |
| M0.1 | rustup + `cargo build --release` 成功 | ✅ 已通过（release 全量构建 41.5s） |
| M0.2 | 空壳 `.app` 双击能打开 + `codesign -dv` 通过 | ✅ 已通过（`Signature=adhoc`、`Identifier=app.openpicasa`、`Mach-O thin (arm64)`）；窗口自截图证明真的画出了五区界面 |
| M0.3 | TCC 授权选目录能读到文件，**重启后仍有效** | ⏸ 待真机：需人工点 NSOpenPanel。若重启失效 → 改用固定签名身份（自签根证书 `codesign --sign "PicasaTest"`） |
| M0.4 | **字体首帧成本 < 200ms**（系统字体 vs 内嵌子集三配置对照） | ⏸ 待实测；当前用系统字体，首屏中文字现实测无缺字（截图可见） |
| M0.5 | 栅格 `show_viewport` + sticky 头 **≥ 55fps** | 🟡 虚拟化已按行号裁剪（ADR-0005，1000 张同一天时可见区间 <200），帧率待真机采样 |

**✅ 已彻底关闭的事项（v3.0）**：原版 Picasa 3.9 样本（T14）**不再是任何阶段的阻塞项**——v3.0 决定**不做数据兼容**，不需要对拍样本。这**不是降级，是消失**。

> **v3.0 立场变更**：用户决策「**不需要读和写 picasa.ini，是做一个新的程序**」。定位从「1:1 复刻（含数据兼容）」改为「**界面与交互对等的全新程序**」——视觉手感对齐 3.9，**数据格式完全自立**。
>
> 这砍掉了本项目原本最大的技术风险（`.picasa.ini` 字节级往返保真）。省下约 1500-2500 行代码、一整套兼容性测试矩阵、一类**不可调试的 bug**，以及一条「拿不到 Windows 原版样本就卡住」的关键路径。**完整说明见 `docs/02` §0.1.4（可直接对外汇报）。**

---

## 协作约定（三份文档之间的一致性纪律）

> 这三条是从本轮评审的真实冲突里总结出来的，不是凭空立的规矩。新增/修改文档时请对照检查。

1. **编号纪律：跨文档引用必须一一映射，不允许"只是数量对得上"。**
   01 §11.5.6 的界面还原清单 `U-01`~`U-44` 与 03 的界面测试表是**同一批被测对象**，编号必须严格对应。**编号错位比缺项更糟**——缺项能被发现，错位不会，只会在验收时变成"你说测了 U-21，我说你测的不是那个"。

2. **"技术选型"类条目一律用「结论 + 保险条款」两层写法。**
   选型是技术决策，产品不拍板；产品只约束**视觉/行为结果**。写法示例见 01 §11.5.6 U-44：结论是「用 macOS 系统字体」，保险条款是「若 M0.4 实测不达标切 Plan B，但**视觉约束不变、无需再修订产品文档**」。这样把"实现可变、验收不变"写进文档，比赌某个选型永不更变更可靠。

3. **所有"零变化 / 零写入 / 零联网"型红线，必须同时给出 ①可证伪的反向断言 ②合规路径的指定 ③正向断言。**
   这类红线有两个共同漏洞：**一是靠"什么都不做"也能通过**（所以要正向断言"该发生的事确实发生了"）；**二是禁令不指定合规路径时，实现会顺手违规**（例如禁止写照片目录，却不说缩略图档位该存哪——见 01 §11.5.3.1 与 02 §5.2）。三处红线均如此配：
   - R-1 照片目录零写入：负向＝照片目录零变化（不新增、不修改任何文件，含不创建 `.picasa.ini`）；**合规路径＝索引与缩略图存 `~/Library/Application Support/<bundle-id>/`、偏好存 `~/Library/Preferences/`**（02 §5.2 有完整分工表）；**正向＝应用容器里的索引与偏好确实被创建并更新**
   - R-2 无编辑入口：负向＝无编辑按钮/菜单/快捷键；**正向＝浏览路径的入口齐全**
   - R-3 装得上打得开：负向＝Gatekeeper 能绕过；**正向＝绕过失败时也给出正确提示**

---

## 许可

原创代码：`MIT OR Apache-2.0`（双许可，Rust 生态惯例）。

**注意第三方许可**（M1 实际依赖与 M2+ 的差异）：

- **M1 的依赖全部是宽松许可**（MIT / Apache-2.0 / BSD / ISC / OFL-1.1）。**M1 不引入任何 LGPL 或 GPL 组件**——因为 M1 不做 RAW（`rawler` 是 LGPL-2.1）、不做元数据写入（`rexiv2` 是 GPL-3.0，已在选型阶段排除）。
- **M1 需要留意的两项**：
  - **字体**：**M1 使用 macOS 系统字体（PingFang SC / SF Pro），不内嵌字体**（结论见 `docs/02` §8.4）。因此**没有 OFL 字体许可负担**；若 M0.4 实测触发 Plan B 改为内嵌 1500 字子集，则需补 OFL-1.1 许可文件随包分发。
  - **图标**：**M1 不引入任何图标库与 SVG 光栅化器**——13 个图标全部用 `egui::Painter` 矢量自绘
    （`crates/picasa-ui/src/icons.rs`），因此既没有 Lucide 的 ISC 义务，也不需要 `resvg`（MPL-2.0）。
    这同时避免了「从原版 Picasa 提取位图」的版权风险。详见 `docs/adr/0004`。
- **M2 起才需要处理的许可**：`rawler`（LGPL-2.1，需子进程隔离）、人脸模型权重（InsightFace 系明确非商用，**不能随商业包分发**）、GeoNames 地名库（CC BY 4.0，须署名）。

分发二进制前必须做许可合规审查，详见 `docs/02` §11「许可与合规」。这是本项目的**硬约束**。
