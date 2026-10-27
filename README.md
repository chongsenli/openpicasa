# picasa-rust

用 Rust 1:1 复刻 Google Picasa 的桌面照片管理器。

- **基准版本**：Picasa 3.9（Windows 最终版）为功能与交互的 1:1 基准。
- **当前阶段目标（M1）**：**在 Apple Silicon（M 系列）Mac 上安装一个能跑起来的 Picasa 界面复刻版，用于浏览照片。**
- **M1 目标平台**：仅 macOS 14+ / arm64。Windows / Linux / Intel Mac 在 M3。

> ### 📌 先读这条：M1 分期策略
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

> 文档状态：本仓库当前处于**方案设计阶段**，尚未产出可编译代码（**且本机没有 Rust 工具链**）。请先读「前置条件」，再读 `docs/` 下的设计文档。

---

## 目录

| 路径 | 内容 | 负责人 |
| --- | --- | --- |
| `docs/01-产品功能清单.md` | 产品功能域、功能点、优先级、验收要点（PRD）；**v3.0**（数据自立：不读写 `.picasa.ini`；M1 = 界面复刻 + 浏览，43 条冻结 + 44 项界面还原） | PM |
| `docs/02-技术架构与选型.md` | 技术架构、crate 选型、**§5 自有元数据模型**、**§7 macOS 打包与 TCC**、**§8 界面壳层**；**v3.0**（数据自立，ini 契约已作废归档） | Dev / Tech Lead |
| `docs/03-测试策略与验收计划.md` | 测试分层、**界面还原度截图对拍**、**macOS 安装运行测试**、**照片目录零写入测试**、**自有数据模型测试**、性能门禁；**v2.1**（173 条用例，M1 有效 95） | QA |
| `README.md` | 项目总览、前置条件、环境准备、仓库骨架 | Dev / Tech Lead |

> **M1 范围指针（新人先读这条）**：M1 的交付范围见 [`docs/01-产品功能清单.md`](docs/01-产品功能清单.md) **§11.5「M1 范围冻结与界面还原清单」**——**43 条冻结功能点 + 174 条明确不进 M1 + 44 项界面还原清单 + 3 条红线**。**§11.5 具约束力**，与 01 §11 的里程碑建议冲突时以它为准；M1 期间任何新增范围须走变更评审。
>
> **技术方案对应**：[`docs/02-技术架构与选型.md`](docs/02-技术架构与选型.md) 的 **§7（macOS 交付与打包）**、**§8（界面壳层）**、**§9.3（M1 技术拆解）**。

---

## ⚠️ 前置条件

### 0. 🚨 头号阻塞：本机没有 Rust 工具链

> ## 不装 rustup，本项目的**一切讨论都是纸上谈兵**。
>
> **这不是「无法编译验证」那么轻。** 本项目 M1 的**唯一交付物是一个能双击运行的 `Picasa.app`**。没有 `cargo`，就没有 `.app`；没有 `.app`，就没有任何可演示的东西。文档写得再完整、评审开得再多，**屏幕上一个像素都产不出来**。
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
| Linux | `x86_64-unknown-linux-gnu` | ⏸ M3 |

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

> 这是**目录与 Cargo 配置的规划**，不是已生成的项目。实际代码在工具链就绪、方案评审通过后再落地。
>
> **M1 的一个重要决策：减少 crate 数量。** v1.x 规划了 9 个 crate，M1 只建 5 个——`picasa-edit`、`picasa-face`、`picasa-ipc`、`picasa-cli` 都不创建。**少一个 crate 就少一处可能出错的地方**，这是降低 M1 风险最直接的手段。

```text
picasa-rust/
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
│   ├── adr/                   # 架构决策记录（0001-0009）
│   └── ui-baseline/           # 界面还原基准截图（01 §11.5.6 要求）
├── packaging/                 # 🆕 macOS 打包
│   ├── Info.plist             # 模板
│   ├── bundle.sh              # cargo-bundle + PlistBuddy + codesign
│   └── Picasa.iconset/        # 图标源（16..1024）
├── crates/
│   ├── picasa-db/             # ✅ M1：SQLite 索引 + 缩略图 BLOB + 迁移（叶子 crate）
│   ├── picasa-db/             # ✅ M1：SQLite 索引与缩略图
│   ├── picasa-imaging/        # ✅ M1：解码 + Orientation + 缩略图（无效果算子）
│   ├── picasa-core/           # ✅ M1：扫描/监视/库会话/选择集/搜索/任务队列
│   ├── picasa-ui/             # ✅ M1：eframe 应用（五区面板 + 栅格 + 查看器）
│   │   └── src/
│   │       ├── theme.rs       # 配色常量
│   │       ├── fonts.rs       # 内嵌 Noto SC 子集 + 回退链
│   │       ├── grid/          # 虚拟布局 + sticky 头 + 纹理缓存
│   │       ├── panels/        # toolbar / library_tree / tray / statusbar
│   │       ├── viewer/        # 暗色全屏查看器
│   │       └── macos/         # rfd 对话框 + TCC 流程 + 全屏
│   ├── picasa-edit/           # ⛔ M1 不创建（M2）
│   ├── picasa-face/           # ⛔ M1 不创建（M2）
│   ├── picasa-ipc/            # ⛔ M1 不创建（M3）
│   └── picasa-cli/            # ⛔ M1 不创建（M2 调试期）
├── assets/
│   ├── icons/                 # Lucide SVG（ISC）
│   ├── fonts/                 # Noto Sans SC 3500 字子集（OFL-1.1）
│   ├── Picasa.icns
│   ├── models/                # M2
│   └── geonames/              # M2
├── tests/
│   ├── golden/                # 黄金图片（Git LFS）
│   └── perf/                  # 性能基准场景脚本
└── xtask/                     # 打包 / 字体子集生成 / mem-report
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

## 快速开始（工具链就绪后）

```bash
# 1) 开发运行
cargo run --release -p picasa-ui

# 2) 构建 .app（M1 的交付物）
./packaging/bundle.sh
# 产物：target/release/bundle/osx/Picasa.app

# 3) 验证签名
codesign -dv --verbose=2 target/release/bundle/osx/Picasa.app

# 4) 质量门禁
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

---

## 📦 安装 Picasa（macOS，Apple Silicon）

> 这一节的内容**必须随 `.app` 一起交付**（01 v3.0 F10-26 要求）。打包时把本节复制为 `Picasa-安装说明.txt` 放进 `.app` 同级目录或 `Contents/Resources/`。

1. 把 `Picasa.app` 拖进「应用程序」文件夹。
2. **首次打开如果提示「无法打开，因为 Apple 无法验证此 App 是否包含恶意软件」**：
   - 到「应用程序」里**右键点击 Picasa → 打开**，在弹出的对话框里再点一次「打开」；
   - 或打开「系统设置 → 隐私与安全性」，在页面底部找到关于 Picasa 的提示，点「仍要打开」。
3. 以上只需做一次。

**为什么会有这个提示？** 这是团队内部测试版，使用**本地 ad-hoc 签名**、未向 Apple 申请公证（公证需要付费开发者账号）。**这不代表 App 有问题。**

**如果你是通过 U 盘或直接拷贝拿到它的，通常不会看到这个提示**——quarantine 属性取决于文件怎么传过来的，不是签名决定的。

仍然打不开时，可在「终端」执行：

```bash
xattr -d com.apple.quarantine /Applications/Picasa.app
```

> ⚠️ **不要**关闭系统完整性保护或全局关闭 Gatekeeper（`spctl --master-disable`）——那是安全风险，我们不建议也不支持。

### 首次运行会请求目录访问权限（macOS TCC）

macOS 会保护「图片」「桌面」「下载」「文稿」与外接盘。**第一次添加这些位置的文件夹时，系统会弹窗询问授权——这是正常且必需的。**

- 点「允许」后正常使用；**点「不允许」应用不会崩溃**，会提示你可以去「系统设置 → 隐私与安全性 → 文件与文件夹」重新开启。
- 我们**不请求全盘访问**，只在你自己选择某个目录时才请求那个目录。

### 卸载

把 `Picasa.app` 拖到废纸篓，然后删除数据目录：

```bash
rm -rf ~/Library/Application\ Support/com.picasarust.app
```

（索引与缩略图缓存在那里。删了不影响你的照片原文件。）

---

## 当前进度与下一步

| 阶段 | 状态 |
| --- | --- |
| 产品功能清单（docs/01） | ✅ **v3.0**（数据自立；M1 43 条冻结 + 174 条不进 + 44 项界面还原 + 3 红线） |
| 技术架构与选型（docs/02） | ✅ **v3.0**（§5 自有元数据模型 + §7 macOS 打包 + §8 界面壳层；ini 契约已归档） |
| 测试策略（docs/03） | ✅ **v2.1**（173 条用例：M1 有效 95 / M2 恢复 57 / 前提消除 21 / 永久废弃 0） |
| **Rust 工具链** | ❌ **未安装 —— 第 0 号事项**（不是「第 3 号风险」，而是「做不做得出来」的前提；其余全部风险都排在它后面） |
| Xcode Command Line Tools | 🔴 **未确认**（`xcode-select -p`，T19） |
| cargo workspace 落地 | ⏸ 等待工具链 + 方案评审 |
| macOS `.app` 打包链路 | ⏸ 等待工具链（M0.2 第一步验证） |

**M1 的 5 个 Go/No-Go 前置**（M0 阶段必须全部验证，详见 `docs/02` §9.2）：

| # | 验证项 | 不通过怎么办 |
| --- | --- | --- |
| M0.1 | rustup + `cargo build --release` 成功 | **M1 无法开工**，升级为 Lead/用户决策 |
| M0.2 | 空壳 `.app` 双击能打开 + `codesign -dv` 通过 | 排查签名/plist/可执行位；**硬门槛** |
| M0.3 | TCC 授权选目录能读到文件，**重启后仍有效** | 若重启失效 → 改用**固定签名身份**（自签根证书，`codesign --sign "PicasaTest"` 替代 `--sign -`）；仍失效才退回「每次更新重新授权」并由 PM 写进安装指引 |
| M0.4 | **字体首帧成本 < 200ms**（系统字体 vs 内嵌子集三配置对照；首帧成本 ∝ 首屏唯一字符数约 200–600，与字体文件大小无关） | 切 Plan B：内嵌 1500 字高频子集；仍超标则接受 300ms 并评估冷启动 |
| M0.5 | 栅格 `show_viewport` + sticky 头 **≥ 55fps** | 启动兜底（纯 wgpu 自绘，+2 周） |

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
  - **图标** Lucide → **ISC**，无需署名。
- **M2 起才需要处理的许可**：`rawler`（LGPL-2.1，需子进程隔离）、人脸模型权重（InsightFace 系明确非商用，**不能随商业包分发**）、GeoNames 地名库（CC BY 4.0，须署名）。

分发二进制前必须做许可合规审查，详见 `docs/02` §11「许可与合规」。这是本项目的**硬约束**。
