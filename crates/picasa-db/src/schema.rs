//! Schema 定义与迁移脚本。
//!
//! 迁移框架从第 1 天就在（02 §5.5）：`schema_migrations` + 顺序迁移数组。
//! M2 加列/加表时只追加一个 `Migration`，不改已有脚本。
//!
//! ## 版本历史
//!
//! | 版本 | 名称 | 内容 |
//! | --- | --- | --- |
//! | 1 | `m1_minimal` | folders / photos / thumbs |
//! | 2 | `m21_data_layer` | roots、folders 层级、照片尺寸状态 + 生成列、排除表、任务与失败表 |
//! | 3 | `m21_sort_effective_taken` | `eff_taken_at` 虚拟生成列 + 排序索引（**让「按拍摄时间」真正走索引**） |
//! | 4 | `m21_pending_thumbs_index` | `(thumb_state, taken_at DESC)` —— 让「取一批待补缩略图」不再全表排序 |
//! | 5 | `m22_index_management` | 失败分类 `kind`/`ignored`、任务历史终态列、`folders.last_verified_at` |
//! | 6 | `m3_scale_realtime_formats` | L1 快筛 `folders.dir_mtime`、递归计数 `total_count`、监视配置、`photos.format`/`taken_from`、`settings` 表与规模索引 |

/// 一条迁移：版本号 + 名字 + 一段 SQL。**只追加，不改已有的**。
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 当前程序支持的 schema 版本 = 迁移数组里最大的 version。
///
/// ⚠️ **与 `docs/06`（M2.2 规格）的编号偏差**：规格 §5 把它写成「迁移 v3」，
/// 因为它成文时代码停在 v2；本仓库在 M2.1 收尾时又落了 v3（`eff_taken_at`）与
/// v4（待补缩略图索引）。**内容按规格做，版本号以代码为准 = 5**，
/// 已登记进 `docs/06` §12.1 冲突表。
pub const SCHEMA_VERSION: i64 = 8;

/// v1 · M1 最小集（02 §5.3）。
///
/// 与文档的两处**有意偏差**，已记录在 `docs/adr/0002-m1-schema-deviation.md`：
///
/// 1. **不建 `photos_fts`**。理由：① FTS5 的 `unicode61` 不切分中文，M1 的搜索输入大概率是中文，
///    主路径必须走 `LIKE`；② 外键级联删除时 `folder_path` 已不可得，FTS「delete 命令」需要旧值，
///    双写极易与主表不一致。按 02 §5.5 的同一原则「预留但不建表，用迁移脚本逐步加」处理。
/// 2. `photos` 不额外加冗余列。M1 不需要「为了查询方便」而复制 `folder_path`。
const V1: &str = r#"
CREATE TABLE folders (
    id            INTEGER PRIMARY KEY,
    path          TEXT NOT NULL UNIQUE,   -- 规范化绝对路径（NFC 存储）
    path_key      TEXT NOT NULL,          -- 比较键（小写 + NFC）
    volume_id     TEXT,                   -- 卷标识（外接盘重挂载后识别）
    is_online     INTEGER NOT NULL DEFAULT 1,
    last_scan_at  INTEGER,
    thumb_tier    INTEGER                 -- 每目录档位
);
CREATE INDEX idx_folders_key ON folders(path_key);

CREATE TABLE photos (
    id           INTEGER PRIMARY KEY,
    folder_id    INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    name_key     TEXT NOT NULL,
    size_bytes   INTEGER NOT NULL,
    mtime_ns     INTEGER NOT NULL,
    width        INTEGER,
    height       INTEGER,
    orientation  INTEGER,                 -- EXIF Orientation 1..8（只读，用于显示摆正）
    taken_at     INTEGER,                 -- 拍摄时间（秒；缺失回退 mtime）
    taken_at_src INTEGER,                 -- 0=EXIF 1=文件名 2=mtime
    thumb_state  INTEGER NOT NULL DEFAULT 0,  -- 0=未生成 1=OK 2=失败 3=损坏
    decoder_ver  INTEGER NOT NULL DEFAULT 0,  -- 解码器版本（缓存失效用）
    UNIQUE(folder_id, name_key)
);
CREATE INDEX idx_photos_taken  ON photos(taken_at DESC);
CREATE INDEX idx_photos_folder ON photos(folder_id, name_key);
CREATE INDEX idx_photos_mtime  ON photos(mtime_ns DESC);
CREATE INDEX idx_photos_thumb  ON photos(thumb_state);

CREATE TABLE thumbs (
    photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
    tier     INTEGER NOT NULL,            -- 0=T0 栅格档(512) 1=T1 查看器档
    w        INTEGER NOT NULL,
    h        INTEGER NOT NULL,
    format   INTEGER NOT NULL,            -- 0=jpeg 1=webp
    data     BLOB NOT NULL,
    PRIMARY KEY (photo_id, tier)
) WITHOUT ROWID;
"#;

/// v2 · M2.1 数据层（`docs/05` §1.3）。
///
/// ## 只增不改
///
/// 只 `CREATE TABLE` / `ADD COLUMN` / `CREATE INDEX`，不 `DROP`、不改列类型、
/// 不重写已有行。**唯一的例外**是 `orientation` 的空值归一到 1，
/// 它不在 DDL 里而在数据步骤（`Db::apply_v2_data`）里，因为它要跟 roots 迁入同事务。
///
/// ## 两个容易写错的地方
///
/// 1. `ADD COLUMN` **不允许**加 STORED 生成列，只能 `VIRTUAL`（SQLite 限制）。
///    VIRTUAL 列**可以**建索引（v3 的 `eff_taken_at` 就是这么做的）；
///    M3 瀑布流只读不筛，够用。若 M5 要按显示宽高筛选，同样可加索引。
/// 2. 加 `REFERENCES` 列时默认值必须是 NULL（且开了 `foreign_keys`），
///    所以 `root_id` / `parent_id` 都不带 `NOT NULL DEFAULT 0`——
///    用 0 表示「无父」会在外键检查上直接炸。
const V2: &str = r#"
-- ---------------------------------------------------------------- 1. roots
-- 关注根目录的**唯一真相源**（`docs/05` §1.3.2）。
-- 迁入前它在 Preferences.roots 里；迁入后 prefs 只读不写，否则就是双真相源（P0）。
CREATE TABLE roots (
    id               INTEGER PRIMARY KEY,
    path             TEXT NOT NULL UNIQUE,        -- NFC 绝对路径
    path_key         TEXT NOT NULL,               -- 比较键（NFC + 大小写规则）
    state            INTEGER NOT NULL DEFAULT 0,  -- 0=active 1=closed（关闭只改状态，不删数据）
    added_at         INTEGER NOT NULL DEFAULT 0,
    closed_at        INTEGER,
    case_insensitive INTEGER NOT NULL DEFAULT 1   -- 该卷是否不区分大小写（探测一次后固化）
);
CREATE INDEX idx_roots_key ON roots(path_key);

-- -------------------------------------------------------------- 2. folders
-- 只扩展层级字段，不新建表（`docs/05` §1.2 第 3 条：folders 已存在，每目录一行）。
ALTER TABLE folders ADD COLUMN root_id      INTEGER REFERENCES roots(id);
ALTER TABLE folders ADD COLUMN parent_id    INTEGER REFERENCES folders(id);
ALTER TABLE folders ADD COLUMN name         TEXT;   -- 展示用末段；NULL = 还没推导
ALTER TABLE folders ADD COLUMN direct_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE folders ADD COLUMN has_children INTEGER NOT NULL DEFAULT 0;
ALTER TABLE folders ADD COLUMN index_status INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_folders_parent ON folders(parent_id);
CREATE INDEX idx_folders_root   ON folders(root_id);

-- --------------------------------------------------------------- 3. photos
-- width/height/orientation 都已存在，复用；只补一个「尺寸是否已回填」的状态位。
ALTER TABLE photos ADD COLUMN dims_status INTEGER NOT NULL DEFAULT 0; -- 0 待回填 1 已完成 2 读取失败
-- 显示宽高用生成列：读时计算、不占空间、不可能与 orientation 不一致。
-- orientation 为 NULL 时 `NULL BETWEEN 5 AND 8` 求值为 NULL → 走 ELSE 分支，
-- 取 width/height，不会变成 NULL 方向。
ALTER TABLE photos ADD COLUMN display_width INTEGER
    GENERATED ALWAYS AS (CASE WHEN orientation BETWEEN 5 AND 8 THEN height ELSE width END) VIRTUAL;
ALTER TABLE photos ADD COLUMN display_height INTEGER
    GENERATED ALWAYS AS (CASE WHEN orientation BETWEEN 5 AND 8 THEN width ELSE height END) VIRTUAL;
-- 「选中节点按拍摄时间分页」的索引（name 排序已有 idx_photos_folder，不重复建）
CREATE INDEX idx_photos_folder_taken ON photos(folder_id, taken_at DESC);
-- 部分索引：后台回填只扫待处理行（`docs/05` §1.3.6）
CREATE INDEX idx_photos_dims_pending ON photos(dims_status) WHERE dims_status = 0;

-- ----------------------------------------------------------- 4. exclusions
CREATE TABLE exclusions (
    id            INTEGER PRIMARY KEY,
    root_id       INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,          -- 归一化相对路径（NFC，原样保留供展示）
    match_key     TEXT NOT NULL,          -- NFC + （roots.case_insensitive=1 时小写）
    created_at    INTEGER NOT NULL DEFAULT 0,
    stale         INTEGER NOT NULL DEFAULT 0,  -- 路径已不存在 → 1，供诊断页清理
    UNIQUE(root_id, match_key)
);
CREATE INDEX idx_exclusions_root ON exclusions(root_id, match_key);

-- ------------------------------------------------- 5. 索引任务与失败记录
-- ⚠️ 真相源边界：**实时进度**仍以内存的 IndexBoard 为准（04 §3.2 四处一致靠它），
-- 这里只存**历史任务与历史失败**。DB 不参与实时进度计算——跨过去就退化成两个计数器。
CREATE TABLE index_tasks (
    task_id     INTEGER PRIMARY KEY,
    root_id     INTEGER REFERENCES roots(id) ON DELETE CASCADE,
    scope_path  TEXT NOT NULL,
    kind        INTEGER NOT NULL DEFAULT 0,   -- 0 首次 1 刷新 2 重建 3 恢复 4 回填
    status      INTEGER NOT NULL DEFAULT 0,   -- 与 04 §3.1 七态同编码（见 TaskStatus）
    started_at  INTEGER,
    finished_at INTEGER,
    error_code  TEXT
);
CREATE INDEX idx_index_tasks_status ON index_tasks(status);

CREATE TABLE index_task_failures (
    task_id       INTEGER NOT NULL REFERENCES index_tasks(task_id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    error_code    TEXT NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 1,
    updated_at    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (task_id, relative_path)
) WITHOUT ROWID;

-- ------------------------------------------------- 6. 数据步骤的一次性标记
-- DDL 由 schema_migrations 管，**数据步骤**（roots 迁入、层级推导）需要另一个守卫：
-- 迁移框架只保证「迁移脚本跑过一次」，不保证「数据补数跑过一次」。
CREATE TABLE data_migrations (
    name       TEXT PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
"#;

/// v3 · 让「按拍摄时间」排序真正用上索引（`docs/05` §1.9）。
///
/// ## 为什么要加一列
///
/// M1 的排序口径是 `COALESCE(p.taken_at, p.mtime_ns/1000000000) DESC, p.name_key ASC`
/// ——`taken_at` 可能为 NULL（EXIF 缺失时回退到文件 mtime），所以用 COALESCE 兜。
/// 但 **COALESCE 是表达式，SQLite 无法用任何索引满足它**，于是每次排序都是
/// 「全表扫描 → 算 5 万次 COALESCE → 临时 B-tree 排序」。
///
/// 实测（5 万张，release）：`LIMIT 2000` 也要 **199 ms**——限行数一点忙都帮不上，
/// 因为排序必须在截断之前完成。用户点开一个文件夹就卡住 200 ms。
///
/// 把同一个表达式固化成**虚拟生成列**并在它上面建索引后，同一条查询降到 **1.8 ms**。
///
/// ## 为什么是 VIRTUAL 而不是 STORED
///
/// `ALTER TABLE ADD COLUMN` 不允许 STORED 生成列（SQLite 限制，v2 的 `display_*`
/// 也是因此选了 VIRTUAL）。VIRTUAL 不占存储、随源列自动更新、**且可以建索引**
/// ——v2 注释里"VIRTUAL 列不进索引"的说法是错的，已在 `docs/05` §1.7 更正。
///
/// 顺序必须与查询逐字一致（`eff_taken_at DESC, name_key ASC`）才命得中索引；
/// `OrderBy` 里那两条字符串与这里是一对，改一处必须改另一处。
const V3: &str = r#"
ALTER TABLE photos ADD COLUMN eff_taken_at INTEGER
    GENERATED ALWAYS AS (COALESCE(taken_at, mtime_ns / 1000000000)) VIRTUAL;
CREATE INDEX idx_photos_eff_taken ON photos(eff_taken_at DESC, name_key ASC);
"#;

/// v4 · 让「取一批待补缩略图」不再把整表排一遍（`docs/05` §1.12）。
///
/// ## 问题是这把锁，不是这条查询
///
/// `photos_pending_thumbs` 的形状是
/// `WHERE thumb_state = 0 AND folder_id IN (…) ORDER BY taken_at DESC LIMIT 50000`。
/// 原来的执行计划是「走 `idx_photos_thumb` 取全部待补行（实测 225,191 行）
/// → `USE TEMP B-TREE FOR ORDER BY` → 再截断」，**LIMIT 一点忙都帮不上**。
///
/// 25.9 万张的库上实测单次 **1851 ms**（release 关系统计，纯 CPU 排序）。
/// 致命的地方在于它是**每处理一个目录调一次**（`scanner.rs::generate_thumbs`），
/// 而这条查询**持着 `Inner.db` 那把全局 Mutex**——UI 每 150 ms 要重载照片列表，
/// 抢的就是同一把锁。于是界面每隔几秒被锁住近 2 秒，用户看到的就是「打开就死机」。
///
/// ## 索引怎么解
///
/// `(thumb_state, taken_at DESC)`：条件与排序**在索引里已经有序**，
/// SQLite 从索引头部顺序读出、边读边按 `folder_id` 过滤、凑够 LIMIT 就停，
/// 不再物化临时 B-tree。同样的库上降到个位数毫秒。
///
/// 为什么把 `folder_id` 放在 `taken_at` 前面反而不行：`folder_id IN (…)` 是多段区间，
/// 跨多个 folder 之后 `taken_at` 就不再全局有序了，SQLite 只能退回去做排序——
/// 那是我们刚修掉的那个问题。
const V4: &str = r#"
CREATE INDEX idx_photos_thumb_taken ON photos(thumb_state, taken_at DESC);
"#;

/// v5 · M2.2 索引管理（`docs/06` §4/§5）。
///
/// ## 三件事，全部只增列 / 只建索引
///
/// 1. **失败分类**（§4.5）：`index_task_failures` 加 `kind` / `ignored`。
///    以前失败只有一串 `error_code` 文本，`NoPermission` 与 `Corrupt` 在界面上长得一样，
///    于是「重试全部」会把注定失败的格式不支持项也重跑一遍。分类后每组可单独处置，
///    `ignored` 是**标记不是删除**（R-M2-4：忽略的失败项必须仍在列表可见）。
/// 2. **任务历史**（§4.7）：`index_tasks` 加 `outcome` / `processed` / `failed` /
///    `duration_ms`。`finished_at` 在 v2 就有了，不重复加。
/// 3. **目录级校验时间戳**（P-16 预留）：`folders.last_verified_at`。本版**只写不依赖**，
///    避免"按上次扫描时间跳过目录"这个策略问题没定却先把行为改了。
///
/// ## 为什么 `kind` 的默认是 3（Unreachable）而不是 0 或 1
///
/// 历史失败项没有分类信息。M2.1 实测那 970 张「解码失败」经核实是**瞬时失败被固化**
/// （`docs/05` §1.12），不是文件坏了。默认标成 `Unreachable`（可重试）才符合事实；
/// 标成 `Corrupt` 等于把 970 张好照片判了死刑。
///
/// ## ⚠️ 不建 `idx_thumbs_photo`
///
/// 规格 §5.2 里有一条 `CREATE INDEX idx_thumbs_photo ON thumbs(photo_id)`。
/// `thumbs` 是 `WITHOUT ROWID` 且主键就是 `(photo_id, tier)`，photo_id 前缀已由主键
/// 索引完全覆盖 —— 再建一条是纯粹的写放大（每次落库多维护一棵 B 树）。
/// 这条**不做**，已登记进 `docs/06` §12.1。
const V5: &str = r#"
-- ------------------------------------------------------- 1. 失败分类（§4.5）
ALTER TABLE index_task_failures ADD COLUMN kind    INTEGER NOT NULL DEFAULT 3;
ALTER TABLE index_task_failures ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_failures_kind ON index_task_failures(kind, ignored);

-- ------------------------------------------------------- 2. 任务历史（§4.7）
-- finished_at 在 v2 已存在，这里只补「结局与账目」四列。
ALTER TABLE index_tasks ADD COLUMN outcome     INTEGER;
ALTER TABLE index_tasks ADD COLUMN processed   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE index_tasks ADD COLUMN failed      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE index_tasks ADD COLUMN duration_ms INTEGER;
CREATE INDEX IF NOT EXISTS idx_tasks_finished ON index_tasks(finished_at);

-- ------------------------------------------- 3. 目录级校验时间戳（P-16 预留）
ALTER TABLE folders ADD COLUMN last_verified_at INTEGER;
"#;

/// v6 · M3「规模与实时性」（`07` §8.1 + `07-A` §6 + `08` §1 合并）。
///
/// ## 这一版要解决的四个问题
///
/// 1. **启动校验从「全树 read_dir + 逐文件 stat」变成「每目录一次 stat」**
///    （`07` §3 的 L1 快筛）。判据是目录 `st_mtime`，所以要把它存下来 →
///    `folders.dir_mtime`。**留空**表示「还没量过」，等价于 M2.2 行为（走 L2），
///    于是升级后的首次启动仍然是一次老实全校，之后才变快。
/// 2. **递归计数**（`07` §4）。`total_count` 是缓存列：快照里的 `folder_totals`
///    才是 UI 读的那份（同事务算出），这一列只写不读，供 M4+ 的查询用。
/// 3. **监视配置**（`07` §2）。注意**运行时状态不入库**——`WatchState` 每次启动
///    重新探测（装不装得上、是不是网络卷，换个网络环境就变了），入库只会说谎。
///    入库的**只有用户意图**：开不开、轮询间隔。
/// 4. **多格式**（`07-A` §4）。`format` 记录**实际**格式（魔数嗅探结果，
///    不是扩展名）；`taken_from` 区分拍摄时间是 EXIF 还是文件时间
///    （M4 的「无日期」筛选就是 `taken_from = 2`）。
///
/// ## ⚠️ 与规格的三处命名偏差（以代码为准，登记进 `docs/06` §12.1）
///
/// | 规格写法 | 本仓库 | 原因 |
/// | --- | --- | --- |
/// | `photos.filename` | `photos.name` | v1 就叫 `name`，改名要重建表 |
/// | `idx_photos_folder_name(folder_id, filename)` | **已存在** `idx_photos_folder(folder_id, name_key)` | 同一件事，不重复建（写放大） |
/// | `folders.rel_path` 范围索引 | `idx_folders_root_path(root_id, path_key)` | 本仓库存绝对路径，`path_key` 前缀范围同样走索引；加 `rel_path` 要整列回填，收益不抵风险 |
///
/// ## 为什么 `idx_photos_taken_id` 是 `(taken_at, id)` 而不是 `(eff_taken_at, name_key)`
///
/// v3 的 `idx_photos_eff_taken` 服务于「按拍摄时间排序」的 **OFFSET 分页**。
/// M3 的虚拟数据源改**键集分页**（`07` §5.2 SCALE-03）：`WHERE (taken_at, id) < (?, ?)`。
/// 键集分页要求排序键与索引列**逐字一致**才能做元组比较，而 `eff_taken_at` 是
/// COALESCE 生成列——拿它做键集边界要写两次 COALESCE，SQLite 不会把它优化成索引区间。
/// 所以另建一条 `(taken_at DESC, id DESC)`，让「所有照片」与「子树按日期」的
/// 键集查询都有落点。两条索引并存不是浪费：一个服务 OFFSET，一个服务键集。
const V6: &str = r#"
-- ------------------------------------------------- 1. 两级校验（07 §3，L1 快筛）
-- 目录 st_mtime（秒）。NULL = 还没量过 → 视为「需要 L2」，等价于 M2.2 行为。
ALTER TABLE folders ADD COLUMN dir_mtime   INTEGER;
-- 递归计数缓存（07 §4，只写不读，P-23 若判定不划算可裁掉这一列）
ALTER TABLE folders ADD COLUMN total_count INTEGER NOT NULL DEFAULT 0;

-- ------------------------------------------------------- 2. 监视配置（07 §2）
-- 只存**用户意图**；WatchState（Active/Polling/Overflow/Off）是运行时状态，不入库。
ALTER TABLE roots ADD COLUMN watch_enabled   INTEGER NOT NULL DEFAULT 1;
ALTER TABLE roots ADD COLUMN poll_interval_s INTEGER;   -- NULL = 用默认值

-- ------------------------------------- 3. 身份配对辅助（07 §2 WATCH-09/10）
-- blake3(前 64 KB)，**懒填**：只有 (size, mtime) 出现歧义时才写。
ALTER TABLE photos ADD COLUMN head_hash BLOB;

-- ------------------------------------------------- 4. 多格式（07-A §4 / 08 §1）
-- 实际格式（魔数嗅探结果）。枚举见 `picasa_db::PhotoFormat`。
ALTER TABLE photos ADD COLUMN format     INTEGER NOT NULL DEFAULT 0;
-- 拍摄时间来源：0 unknown / 1 exif / 2 mtime（枚举见 `TakenFrom`）。
ALTER TABLE photos ADD COLUMN taken_from INTEGER NOT NULL DEFAULT 0;

-- ------------------------------------------------------------ 5. 设置（key/value）
-- 07-A §6 假定「settings 表已有」——本仓库实际没有（M2 的设置全在 Preferences JSON 里）。
-- 两者的分工：**Preferences 存「这台机器的界面状态」**（窗口位置、折叠、上次视图），
-- **settings 存「跟库走的行为开关」**（怎么排序、要不要递归、缩放多少），
-- 因为换个窗口大小不该改变「包含子文件夹」这个语义，而它又必须能被迁移脚本预置。
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
) WITHOUT ROWID;

-- --------------------------------------------------------------- 6. 规模索引
-- 键集分页的两把钥匙（07 §5.2 SCALE-03）
CREATE INDEX IF NOT EXISTS idx_photos_taken_id ON photos(taken_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_photos_size_mtime ON photos(folder_id, size_bytes, mtime_ns);
-- L2 全校按 last_verified_at 升序取批次（07 §3.2 VERIFY-03）
CREATE INDEX IF NOT EXISTS idx_folders_verified ON folders(last_verified_at);
-- 失败标签按格式筛（07-A FORMAT-02）
CREATE INDEX IF NOT EXISTS idx_photos_format ON photos(format);
-- 子树的绝对路径前缀范围查询（07-A §3.3，替代 rel_path）
CREATE INDEX IF NOT EXISTS idx_folders_root_path ON folders(root_id, path_key);

-- ------------------------------------------------------------- 7. 设置预置行
-- 07-A §6。「预置成 NULL/0」而不是「等第一次写再插」：
-- 读取方要能区分「用户没设过」与「设成了默认值」，缺行会让这两件事混成一件。
INSERT OR IGNORE INTO settings(key, value) VALUES ('grid_cell_px', NULL);
INSERT OR IGNORE INTO settings(key, value) VALUES ('recurse_subfolders', '0');
"#;

pub const V7: &str = r#"
-- ============================================================== v7：收藏 / 标签 / 说明
-- 用户功能清单 #8/#9：收藏（星标）、标签、说明。数据边界（产品定义）：
-- **只存应用数据库**，不写照片文件、不写 IPTC/XMP；是否写回留到以后单独决策。
--
-- 设计取舍：
-- - `starred` 放 photos 列（高频布尔，筛选/排序都要走它，JOIN 一张表不值）；
-- - 标签与说明是「用户创作数据」，独立成表，`idx_rebuild` 之类重建索引的操作
--   **绝不能碰**（文档承诺「索引库可随时删掉重建」只对派生数据成立）；
-- - 标签 (photo_id, label) 主键去重，一张照片多标签、一个标签多照片。
ALTER TABLE photos ADD COLUMN starred INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS captions (
    photo_id INTEGER PRIMARY KEY REFERENCES photos(id) ON DELETE CASCADE,
    text     TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS tags (
    photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
    label    TEXT NOT NULL,
    PRIMARY KEY (photo_id, label)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_tags_label ON tags(label);
CREATE INDEX IF NOT EXISTS idx_photos_starred ON photos(starred) WHERE starred = 1;
"#;

/// v8 · M4 索引管理版（docs/11 §3/§5）。**只增不改**：全部是 ADD COLUMN / CREATE TABLE，
/// 不 DROP、不改列类型、不重写已有行（沿用 05 §4.5 的迁移纪律）。
///
/// 四块内容：
/// 1. **EXIF 摘要列**（`M4-META-07`）：把 `picasa_imaging::ExifSummary` 拍平成列，
///    供查看器信息面板与相机/镜头筛选使用。全部可空——「缺失就是缺失」，不编造。
///    `exif_status`：0=未读 / 1=OK / 2=缺失或读取失败（附 `exif_error` 原因短语）。
/// 2. **相册**（`M4-ORG-07～12`）：`albums` + `album_photos` 多对多，一张照片可属多个相册。
///    成员引用 `photos(id)`，但离线时**只标记不删行**（否则 CASCADE 会连带清掉标签）。
/// 3. **近期搜索**（`M4-SEARCH-07`）：存自有 DB，纳入迁移/备份/恢复，不写照片目录。
/// 4. **索引文件清单表**（`M4-IDX-10`）：导出时写入、导入时读取；本表本身也属于导出内容。
///    `format_version` 与 `schema_version` 分离：前者是文件格式契约，后者是 DB 迁移版本。
pub const V8: &str = r#"
-- ============================================================ v8：M4 索引管理版
-- ---------------------------------------------------------- 1. EXIF 摘要（只读，不写回照片）
ALTER TABLE photos ADD COLUMN camera_make    TEXT;
ALTER TABLE photos ADD COLUMN camera_model   TEXT;
ALTER TABLE photos ADD COLUMN lens_model     TEXT;
ALTER TABLE photos ADD COLUMN focal_length   TEXT;
ALTER TABLE photos ADD COLUMN f_number       TEXT;
ALTER TABLE photos ADD COLUMN exposure_time  TEXT;
ALTER TABLE photos ADD COLUMN iso            TEXT;
ALTER TABLE photos ADD COLUMN exif_status    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE photos ADD COLUMN exif_error     TEXT;

-- 相机/镜头组合筛选的辅助索引（M4-SEARCH-01/09，避免退化为全表扫描）
CREATE INDEX IF NOT EXISTS idx_photos_camera ON photos(camera_make, camera_model);
CREATE INDEX IF NOT EXISTS idx_photos_lens   ON photos(lens_model);
CREATE INDEX IF NOT EXISTS idx_photos_exif_status ON photos(exif_status);

-- ---------------------------------------------------------- 2. 相册（应用内虚拟集合）
CREATE TABLE IF NOT EXISTS albums (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS album_photos (
    album_id  INTEGER NOT NULL REFERENCES albums(id)     ON DELETE CASCADE,
    photo_id  INTEGER NOT NULL REFERENCES photos(id)    ON DELETE CASCADE,
    added_at  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (album_id, photo_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_album_photos_photo ON album_photos(photo_id);

-- ---------------------------------------------------------- 3. 近期搜索（用户创作数据，只存自有库）
CREATE TABLE IF NOT EXISTS recent_searches (
    id         INTEGER PRIMARY KEY,
    query_json TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT 0
);

-- ---------------------------------------------------------- 4. 索引文件清单（导出/导入共用）
-- 每行对应一个导出的索引文件；导入时读它做版本识别与完整性展示。
CREATE TABLE IF NOT EXISTS index_manifest (
    key   TEXT PRIMARY KEY,
    value TEXT
) WITHOUT ROWID;
INSERT OR IGNORE INTO index_manifest(key, value) VALUES ('format_version', '1');
INSERT OR IGNORE INTO index_manifest(key, value) VALUES ('min_reader_version', '0.4.0');
"#;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "m1_minimal",
        sql: V1,
    },
    Migration {
        version: 2,
        name: "m21_data_layer",
        sql: V2,
    },
    Migration {
        version: 3,
        name: "m21_sort_effective_taken",
        sql: V3,
    },
    Migration {
        version: 4,
        name: "m21_pending_thumbs_index",
        sql: V4,
    },
    Migration {
        version: 5,
        name: "m22_index_management",
        sql: V5,
    },
    Migration {
        version: 6,
        name: "m3_scale_realtime_formats",
        sql: V6,
    },
    Migration {
        version: 7,
        name: "m4_favorites_tags_captions",
        sql: V7,
    },
    Migration {
        version: 8,
        name: "m4_index_portability",
        sql: V8,
    },
];

/// 数据步骤名（`data_migrations` 的主键）。写成常量，避免调用方手打字符串写错。
pub const DATA_V2_ROOTS_FOLDERS: &str = "v2_roots_folders";
/// v5 的失败分类补数（历史 `error_code` 文本 → `kind`）。同样是「数据步骤」，
/// 用 `data_migrations` 单独守卫，与 DDL 的守卫分开（理由见 `upgrade.rs` 文件头）。
pub const DATA_V5_FAILURE_KINDS: &str = "v5_failure_kinds";
/// v6 的补数：`photos.format` 按扩展名预填、`photos.taken_from` 按 EXIF 源回填、
/// `folders.total_count` 一次递归聚合算出（`07` §8.2）。
///
/// 三项放**同一个数据步骤**，因为它们都要求「库已经到 v6」，且都是「每行都要动一次」
/// 的全表写——拆成三步就要全表扫三遍，100 万张库上差好几分钟。
/// 幂等靠 `data_migrations` 这一条记录。
pub const DATA_V6_FORMATS_L1: &str = "v6_formats_l1";

/// 索引任务的七态编码（与 `picasa-core::index::IndexState` 一一对应）。
///
/// 两边必须同号：`docs/04` §3.1 的七态要能直接对回历史任务行。
/// `picasa-core` 侧有一个测试断言两边一致，改一处漏另一处会红。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum TaskStatus {
    Waiting = 0,
    Scanning = 1,
    Thumbnailing = 2,
    Completed = 3,
    PartialFailed = 4,
    Paused = 5,
    Unreachable = 6,
}

impl TaskStatus {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Waiting,
            1 => Self::Scanning,
            2 => Self::Thumbnailing,
            3 => Self::Completed,
            4 => Self::PartialFailed,
            5 => Self::Paused,
            6 => Self::Unreachable,
            _ => return None,
        })
    }
}

/// 任务类型（`index_tasks.kind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum TaskKind {
    /// 首次索引（新关注文件夹）
    FirstScan = 0,
    /// 刷新（右键「刷新」）
    Refresh = 1,
    /// 重新索引（诊断页「全量重建」）
    Reindex = 2,
    /// 重启后恢复未完成任务
    Resume = 3,
    /// 宽高回填
    DimsBackfill = 4,
    /// 启动增量校验（M2.2 §4.8，P-12 落定后的**唯一**变化发现机制）
    Verify = 5,
}

impl TaskKind {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::FirstScan,
            1 => Self::Refresh,
            2 => Self::Reindex,
            3 => Self::Resume,
            4 => Self::DimsBackfill,
            5 => Self::Verify,
            _ => return None,
        })
    }

    /// 界面文案（**唯一措辞来源**：任务标签的「类型」列就用这个）。
    pub fn label(self) -> &'static str {
        match self {
            Self::FirstScan => "首次索引",
            Self::Refresh => "刷新",
            Self::Reindex => "重建",
            Self::Resume => "恢复",
            Self::DimsBackfill => "回填尺寸",
            Self::Verify => "校验",
        }
    }
}
