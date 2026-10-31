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
//!
//! ⚠️ **M2（A 线索引底座）没有动 schema**，`SCHEMA_VERSION` 一直是 1。
//! 所以 M2.1 的 `schema_version() == 1` 就是「这是 M2 的库」的判据
//! （`docs/05` §1.1：本项目**没有**用 `PRAGMA user_version`）。

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 当前程序支持的 schema 版本 = 迁移数组里最大的 version。
pub const SCHEMA_VERSION: i64 = 3;

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
];

/// 数据步骤名（`data_migrations` 的主键）。写成常量，避免调用方手打字符串写错。
pub const DATA_V2_ROOTS_FOLDERS: &str = "v2_roots_folders";

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
}

impl TaskKind {
    pub fn code(self) -> i64 {
        self as i64
    }
}
