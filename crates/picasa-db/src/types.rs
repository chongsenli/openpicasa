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
    /// 「有效拍摄时间」= `COALESCE(taken_at, mtime_ns/1e9)`，由 v3 的虚拟生成列算出。
    ///
    /// 重复保留了 `taken_at` 是**有意**的：界面要区分「EXIF 拍摄时间」与「回退值」
    /// （`taken_at_src` 决定标签怎么写），而排序与分组只认这一个口径。
    pub eff_taken_at: i64,
}

impl PhotoRow {
    pub fn abs_path(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.folder_path).join(&self.name)
    }

    /// 用于日期分组的「天」时间戳（本地时区由 UI 侧决定）。
    ///
    /// 直接返回库里算好的 `eff_taken_at`——与 SQL 的排序口径**同一份数据**，
    /// 不会再出现「SQL 按 A 排、分组按 B 分」导致同一天被拆成两段的情况。
    pub fn effective_taken_at(&self) -> i64 {
        self.eff_taken_at
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

// ---------------------------------------------------------------- M2.1 新增

/// 关注根目录（`roots` 表的一行）。
///
/// 迁入之后它是**唯一真相源**：`Preferences.roots` 降级为只读兼容字段，
/// 任何「再加一个根目录」的路径都必须走这里（`docs/05` §5 第 1 条）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootRow {
    pub id: i64,
    pub path: String,
    pub path_key: String,
    /// 0=active / 1=closed。**关闭只改状态，不删数据。**
    pub state: i64,
    pub added_at: i64,
    pub closed_at: Option<i64>,
    /// 该卷是否不区分大小写（探测一次后固化）。排除判定的 `match_key` 依赖它。
    pub case_insensitive: bool,
}

impl RootRow {
    pub fn is_active(&self) -> bool {
        self.state == 0
    }
}

/// 迁入 roots 时由上层（core）递进来的一条种子。
///
/// `added_at` 用偏好文件的 mtime——真实的「加入时间」在 M2 里没记录，**不编造**。
/// `case_insensitive` 由 core 的可移植探测给出；`None` 表示没探测成功，落库取默认 1。
#[derive(Clone, Debug)]
pub struct RootSeed {
    pub path: String,
    pub path_key: String,
    pub added_at: i64,
    pub case_insensitive: Option<bool>,
}

/// 排除项（`exclusions` 表的一行）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionRow {
    pub id: i64,
    pub root_id: i64,
    /// 原样保留供展示（NFC 归一后）
    pub relative_path: String,
    /// 比较键：NFC + （该卷不区分大小写时小写）
    pub match_key: String,
    pub created_at: i64,
    /// 路径已不存在，供诊断页清理
    pub stale: bool,
}

/// 文件夹树的一行（`folders` 层级字段全量）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderTreeRow {
    pub id: i64,
    pub path: String,
    pub name: Option<String>,
    pub parent_id: Option<i64>,
    pub root_id: Option<i64>,
    /// 本目录**直接**包含的照片数（物理事实，不受关闭状态影响 —— P-07）
    pub direct_count: i64,
    pub has_children: bool,
    /// 0 空闲 / 1 索引中 / 2 失败 / 3 部分失败
    pub index_status: i64,
    pub is_online: bool,
}

/// 尺寸回填进度（诊断页与「正在回填」提示用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DimsStats {
    /// 全部照片
    pub total: u64,
    /// 已完成（`dims_status = 1`）
    pub done: u64,
    /// 待回填（`dims_status = 0`）
    pub pending: u64,
    /// 读取失败（`dims_status = 2`）
    pub failed: u64,
}

/// 迁移框架跑完一次升级后的现场（给上层写日志与用户提示用）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpgradeReport {
    /// 升级前的 schema 版本（0 = 新库）
    pub from: i64,
    /// 升级后的 schema 版本
    pub to: i64,
    /// 备份文件。`None` = 没有做备份（新库或已是最新）
    pub backup: Option<std::path::PathBuf>,
    /// 迁移失败后是否已用备份恢复
    pub restored_from_backup: bool,
    /// 迁移失败的现场（`Some` 表示这次没升上去，应用以旧 schema 数据继续运行）
    pub error: Option<String>,
}

/// `apply_v2_data` 的结果计数（写进日志，便于对账）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V2DataReport {
    /// 是否真的执行了（false = 之前跑过，幂等跳过）
    pub applied: bool,
    /// 写入/复用的根目录条数
    pub roots: usize,
    /// 补出来的中间层目录（原先库里没有这些行）
    pub folders_created: usize,
    /// 回写层级字段的目录数
    pub folders_linked: usize,
    /// 尺寸状态被初始化的照片数
    pub photos_stamped: u64,
}

/// 一条索引任务记录（历史，不参与实时进度）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub task_id: i64,
    pub root_id: Option<i64>,
    pub scope_path: String,
    /// `schema::TaskKind` 的整数编码
    pub kind: i64,
    /// `schema::TaskStatus` 的整数编码（与 04 §3.1 七态同号）
    pub status: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error_code: Option<String>,
}

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

/// 索引库的「缓存占用」统计（F 加固的诊断页用）。
///
/// 用途只有一个：让用户知道「这东西占了我多少空间」，并能一键清掉重来。
/// `db_bytes` 是**主库文件大小**（不含 WAL），`thumb_bytes` 是缩略图 BLOB 的净和——
/// 两者不等是正常的：表、索引、未 VACUUM 的空闲页都算在文件里。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 主库文件字节数（`-wal` 未计）
    pub db_bytes: u64,
    /// 照片行数
    pub photo_count: u64,
    /// 缩略图条数（`thumbs` 行数）
    pub thumb_count: u64,
    /// 缩略图 BLOB 总字节数
    pub thumb_bytes: u64,
}
