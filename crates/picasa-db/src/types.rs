//! 数据行类型。全部是「纯数据 + 无行为」，方便上层自由组合。

/// 一个已索引的文件夹。
#[derive(Clone, Debug)]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub is_online: bool,
    /// 每目录缩略图档位（0/1/2，`None` = 跟随全局）。
    pub thumb_tier: Option<i64>,
    pub photo_count: u32,
}

/// 扫描器交给 DB 的单张照片元数据。
///
/// 注意：**不含缩略图**，也**不含任何用户数据**。缩略图由 `put_thumb` 单独写，
/// 因为它的体积比元数据大 3 个数量级，混在一起会让扫描事务变慢。
#[derive(Clone, Debug)]
pub struct PhotoInput {
    pub name: String,
    /// 比较键：小写 + NFC 归一化后的文件名。
    pub name_key: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub taken_at: Option<i64>,
    pub taken_at_src: i64,
}

/// 一首照片记录（含它所在文件夹的路径，省一次 join 查询）。
#[derive(Clone, Debug)]
pub struct PhotoRow {
    pub id: i64,
    pub folder_id: i64,
    pub folder_path: String,
    pub name: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub taken_at: Option<i64>,
    pub taken_at_src: i64,
    pub thumb_state: i64,
    pub decoder_ver: i64,
}

impl PhotoRow {
    pub fn abs_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.folder_path).join(&self.name)
    }

    /// 用于日期分组的「天」时间戳（本地时区由 UI 侧决定）。
    pub fn effective_taken_at(&self) -> i64 {
        self.taken_at.unwrap_or(self.mtime_ns / 1_000_000_000)
    }
}

/// `apply_scan` 的结果计数。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanApply {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
    pub unchanged: u32,
}

/// 缩略图 BLOB。
#[derive(Clone, Debug)]
pub struct ThumbBuf {
    pub w: i64,
    pub h: i64,
    /// 0=jpeg 1=webp
    pub format: i64,
    pub data: Vec<u8>,
}

/// 缩略图生成状态（与 `photos.thumb_state` 一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum ThumbState {
    Pending = 0,
    Ok = 1,
    Failed = 2,
    Corrupt = 3,
}

/// 便捷常量：`upsert` 时表示「缓存键未变」。
pub const UPSERT_UNCHANGED: i64 = -1;

/// 一个目录的缩略图状态计数（M2 索引进度面板用）。
///
/// ⚠️ 这四个数**只来自 SQL 聚合**，不由 UI 侧累加——「四处一致」的前提是同一处算。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbStats {
    /// 目录内照片总数（= 界面上的「已发现」）
    pub total: u64,
    /// 已就绪（`thumb_state = 1`，= 界面上的「已完成」）
    pub ready: u64,
    /// 失败条目数（`thumb_state ∈ {2,3}`）
    pub failed: u64,
    /// 待生成（`thumb_state = 0`）
    pub pending: u64,
}
