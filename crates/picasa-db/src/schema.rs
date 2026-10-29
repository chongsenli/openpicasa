//! Schema 定义与迁移脚本。
//!
//! 迁移框架从第 1 天就在（02 §5.5）：`schema_migrations` + 顺序迁移数组。
//! M2 加列/加表时只追加一个 `Migration`，不改已有脚本。

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 当前程序支持的 schema 版本 = 迁移数组里最大的 version。
pub const SCHEMA_VERSION: i64 = 1;

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

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "m1_minimal",
    sql: V1,
}];
