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
    /// **实际**格式编码（`photos.format`，`07-A` FORMAT-02）。
    ///
    /// `0`（`PhotoFormat::Unknown`）= 「这次没判」——落库侧会**保留库里已有的值**，
    /// 不会把已知格式打回未知。缓存命中的路径就靠这个语义：它不重读文件头，
    /// 但也不该把上次嗅探到的格式抹掉。
    pub format: i64,
}

/// 照片筛选条件（用户功能 #5/#9：按日期/类型/尺寸/收藏/标签筛选）。
///
/// 各字段是**与**关系；`None` = 不过滤该维度。设计成一个结构而不是 N 个参数：
/// 筛选面板一次传一份，新增维度不用改全链路签名。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhotoFilter {
    /// 拍摄日期范围（epoch 秒，含端点）。
    pub taken_range: Option<(i64, i64)>,
    /// 格式类：`jpeg` / `png` / `gif` / `webp` / `bmp` / `tiff` / `svg` / `heic` / `raw`。
    /// 空集 = 不过滤。RAW 与 JPEG 扩展名混在 `.JPG` 里靠 `photos.format` 列区分。
    pub formats: Vec<String>,
    /// 尺寸段（长边像素）：`Small` ≤640 / `Medium` ≤1920 / `Large` >1920。
    pub size_class: Option<SizeClass>,
    /// 只看收藏。
    pub starred_only: bool,
    /// 只看打这个标签的。
    pub tag: Option<String>,
}

/// 尺寸分类（按长边）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SizeClass {
    /// 长边 ≤ 640
    #[default]
    Small,
    /// 640 < 长边 ≤ 1920
    Medium,
    /// 长边 > 1920
    Large,
}

impl SizeClass {
    pub fn label(self) -> &'static str {
        match self {
            SizeClass::Small => "小图 (≤640px)",
            SizeClass::Medium => "中图 (640–1920px)",
            SizeClass::Large => "大图 (>1920px)",
        }
    }

    /// 该类的 SQL 长边条件（`MAX(width,height)`）。
    pub fn sql_cond(self) -> &'static str {
        match self {
            SizeClass::Small => "MAX(COALESCE(p.width,0), COALESCE(p.height,0)) BETWEEN 1 AND 640",
            SizeClass::Medium => "MAX(COALESCE(p.width,0), COALESCE(p.height,0)) > 640 AND MAX(COALESCE(p.width,0), COALESCE(p.height,0)) <= 1920",
            SizeClass::Large => "MAX(COALESCE(p.width,0), COALESCE(p.height,0)) > 1920",
        }
    }
}

/// 一首照片记录（含它所在文件夹的路径，省一次 join 查询）。
#[derive(Clone, Debug, Default)]
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
    /// 收藏（星标）。只存应用数据库（v7），不写照片文件。
    pub starred: bool,
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

/// `apply_scan` 的结果。
///
/// `touched` 是**本次真正需要重做缩略图**的照片 id（新增 + 缓存键变化），
/// 与「库里所有待补缩略图的照片」是两件事：25.9 万张的库里有 22 万张历史欠账，
/// 启动时把它们全补一遍要几小时。扫描只补 `touched`，
/// 欠账由用户显式点「重试 / 补全」再动。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanApply {
    pub added: u32,
    pub updated: u32,
    pub removed: u32,
    pub unchanged: u32,
    pub touched: Vec<i64>,
}

/// 一个目录里已入库照片的「缓存键 + 已解出的元数据」。
///
/// 扫描器用它判断「这个文件没变过」——没变过就不必再读盘解析 EXIF。
/// 25.9 万张的库，全量探测一次要几十分钟；增量发现应该只碰真正变了的文件。
#[derive(Clone, Debug)]
pub struct CachedMeta {
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub decoder_ver: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub taken_at: Option<i64>,
    pub taken_at_src: i64,
    /// 宽高的状态位（0 待回填 / 1 已完成 / 2 读取失败即终态）。
    ///
    /// 单看 `width` 分不清「还没读过」和「读过、读不出来」——两者都是 NULL。
    /// 这个字段就是为了把这两件事分开，否则 903 张读不出宽高的照片每次启动
    /// 都会被重新 `probe()` 一遍，然后再失败一遍。
    pub dims_status: i64,
}

impl CachedMeta {
    /// 缓存键是否与磁盘上这个文件一致（一致就可以直接复用元数据）。
    pub fn matches(&self, size_bytes: i64, mtime_ns: i64, decoder_ver: i64) -> bool {
        self.size_bytes == size_bytes
            && self.mtime_ns == mtime_ns
            && self.decoder_ver == decoder_ver
    }

    /// 宽高这件事**已经出过结论**了吗（读到值，或已判终态失败）。
    ///
    /// 出过结论就不必再碰文件内容。`dims_status = 2` 也算——它的宽高会永远是
    /// NULL，若拿「宽高非空」当唯一判据，这批文件就会被无限重试。
    pub fn dims_settled(&self) -> bool {
        (self.width.is_some() && self.height.is_some()) || self.dims_status == 2
    }
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
///
/// ⚠️ `Failed` 与 `Corrupt` 的区别是**「还会不会再试」**，不是「坏得严不严重」：
/// `Failed`(2) 会在下一轮自动再试（卷可能回来了），`Corrupt`(3) 是终态。
/// M2.2 §4.5 把 `FailureKind` 映射到这两个值，见 [`FailureKind::thumb_state`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum ThumbState {
    Pending = 0,
    Ok = 1,
    Failed = 2,
    Corrupt = 3,
}

/// 任务结局（`index_tasks.outcome`，M2.2 §4.7）。
///
/// 与 `TaskStatus` 是两件事：**状态**说「现在在干嘛」，**结局**说「最后怎么样了」。
/// 七态里没有「取消」这一态（取消是动作，落地表现为回到某个终态），
/// 但历史列表必须能区分「跑完了」和「用户半路撤了」——所以结局单独一列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum TaskOutcome {
    /// 全部成功
    Success = 0,
    /// 部分失败
    PartialFailed = 1,
    /// 失败（整任务没跑起来，如目录不可访问）
    Failed = 2,
    /// 被取消
    Cancelled = 3,
}

impl TaskOutcome {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Success,
            1 => Self::PartialFailed,
            2 => Self::Failed,
            3 => Self::Cancelled,
            _ => return None,
        })
    }

    /// 界面文案。与 `TaskStatus::label` 一样是**唯一措辞来源**。
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "成功",
            Self::PartialFailed => "部分失败",
            Self::Failed => "失败",
            Self::Cancelled => "已取消",
        }
    }
}

/// 失败分类（`index_task_failures.kind`，M2.2 §4.5）。
///
/// ## 为什么必须分类
///
/// 不分类的失败只有一串文本。于是「重试全部」会把**格式不支持**这种永久失败
/// 也重跑一遍——用户看到的是"点了没用，又失败一次"。分类之后：
/// 可重试的才给「重试全部」，永久失败的只给「忽略」，忽略后仍然可见（R-M2-4）。
///
/// ## ⚠️ 编码不能重排
///
/// 整数值**落库**了（`kind` 列），重排等于把用户库里的分类含义改掉。
/// 新增分类只能往后追加。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i64)]
pub enum FailureKind {
    /// 1 · 无权限（`EACCES` / `EPERM`）。权限变更后可重试。
    NoPermission = 1,
    /// 2 · 文件损坏（解码器报格式错误）。手动重试。
    Corrupt = 2,
    /// 3 · 目录/文件失联（`ENOENT` / `EIO` / 卷未挂载）。卷回来后可重试，指数退避。
    Unreachable = 3,
    /// 4 · 格式不支持（扩展名/魔数不在支持列表）。**不可重试**。
    Unsupported = 4,
    /// 5 · 超时（单张解码超过 `DECODE_TIMEOUT_MS`）。手动重试。
    Timeout = 5,
}

impl FailureKind {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            1 => Self::NoPermission,
            2 => Self::Corrupt,
            3 => Self::Unreachable,
            4 => Self::Unsupported,
            5 => Self::Timeout,
            _ => return None,
        })
    }

    /// 全部五类（界面分组与角标都按这个顺序遍历，`failures_by_kind` 的下标即 `code-1`）。
    pub const ALL: [Self; 5] = [
        Self::NoPermission,
        Self::Corrupt,
        Self::Unreachable,
        Self::Unsupported,
        Self::Timeout,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NoPermission => "无权限",
            Self::Corrupt => "文件损坏",
            Self::Unreachable => "目录失联",
            Self::Unsupported => "格式不支持",
            Self::Timeout => "超时",
        }
    }

    /// 能否重试。`Unsupported` 是唯一的永久失败——重跑一万次也是同一个结果。
    pub fn retryable(self) -> bool {
        !matches!(self, Self::Unsupported)
    }

    /// 是否**自动**退避重试（卷回来后自己恢复），而不是等用户点。
    pub fn auto_retry(self) -> bool {
        matches!(self, Self::Unreachable)
    }

    /// 落到 `photos.thumb_state` 的值（§4.5 的映射表）。
    ///
    /// `Unreachable → 2`（失败·可重试），其余 → 3（失败·终态）。
    /// 这两个值的语义差异写在 `docs/05` §1.12：2 会在下一轮自动再试，3 不会。
    pub fn thumb_state(self) -> ThumbState {
        if self == Self::Unreachable {
            ThumbState::Failed
        } else {
            ThumbState::Corrupt
        }
    }
}

/// 从一条历史 `error_code` 文本推断分类（v5 补数用，§5.3）。
///
/// 判据只有**文本包含**，够粗但不越权：早期记录的 `error_code` 是自由文本，
/// 没有更好的结构化信息可依。推断不出来就回落到 `Unreachable`（可重试），
/// 理由见 `schema.rs` V5 的注释——宁可让用户能重试，也不要把好照片判死。
pub fn classify_failure(error_code: &str) -> FailureKind {
    let lower = error_code.to_lowercase();
    if lower.contains("permission") || lower.contains("denied") || lower.contains("eacces") {
        FailureKind::NoPermission
    } else if lower.contains("unsupported") || lower.contains("unsupported_format") {
        FailureKind::Unsupported
    } else if lower.contains("timeout") || lower.contains("timed out") {
        FailureKind::Timeout
    } else if lower.contains("corrupt") || lower.contains("invalid") || lower.contains("decode") {
        FailureKind::Corrupt
    } else {
        FailureKind::Unreachable
    }
}

/// 一个失败分类的汇总（M2.2 §6.1 `FailureGroup`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FailureGroup {
    pub kind: Option<i64>,
    /// 未忽略的条数（**角标只算这个**）
    pub count: u64,
    /// 已忽略的条数（仍在列表可见，只是不进角标）
    pub ignored: u64,
}

/// 一个目录的「库内指纹」（§4.8 启动增量校验的比对基准）。
///
/// 校验时把 `read_dir` 的实时统计与它比：任一字段不等 → 用户在外部动过这个目录。
/// 只比这三个字段而不是逐文件比对，是因为**目录级**比对一次 `read_dir` 就够，
/// 逐文件比对在 25 万张上是 25 万次 `stat`，启动阶段付不起。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderFingerprint {
    pub folder_id: i64,
    /// 绝对路径（NFC）
    pub path: String,
    /// 库内该目录的照片数
    pub photo_count: u64,
    /// 库内该目录的字节总和
    pub size_sum: u64,
    /// 库内该目录的最大 mtime（纳秒）
    pub mtime_max: u64,
}

/// 一个关注根在「关注文件夹」标签里的一行（M2.2 §6.1 `RootView`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootStat {
    pub id: i64,
    pub path: String,
    /// 0 = active / 1 = closed
    pub state: i64,
    /// 该根（**含子目录**）的照片数
    pub photo_count: u64,
    /// 该根（含子目录）的失败照片数
    pub failed: u64,
    /// 最近一次索引时间（Unix 秒；`None` = 没扫过）
    pub last_indexed_at: Option<i64>,
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
    // --- v5（M2.2 §4.7 任务历史）---
    /// 结局（`TaskOutcome` 编码）；未终态为 `None`
    pub outcome: Option<i64>,
    /// 已处理张数
    pub processed: i64,
    /// 失败张数
    pub failed: i64,
    /// 耗时（毫秒）
    pub duration_ms: Option<i64>,
}

impl TaskRow {
    /// 结局（解码失败时回落 `None`，不猜）。
    pub fn outcome(&self) -> Option<TaskOutcome> {
        self.outcome.and_then(TaskOutcome::from_code)
    }

    /// 结局文案（未终态为「进行中」）。
    pub fn outcome_label(&self) -> &'static str {
        self.outcome().map(|o| o.label()).unwrap_or("进行中")
    }

    /// 「已用 mm:ss」（§4.2 M22-IDX-08：**不显示 ETA**，只显示已用时长）。
    pub fn elapsed_text(&self) -> String {
        match self.duration_ms {
            Some(ms) => {
                let secs = (ms / 1000).max(0);
                format!("{:02}:{:02}", secs / 60, secs % 60)
            }
            None => "--:--".to_string(),
        }
    }
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

/// 照片的**实际**格式（`photos.format`，`07-A` §4 / `08` §1 的最终清单）。
///
/// ## 为什么存魔数嗅探的结果，而不是扩展名
///
/// `07-A` FORMAT-01 的规则是「识别 = 扩展名白名单 ∧ 魔数嗅探」，两者不一致时
/// **按魔数解码**（一个叫 `.jpg` 的 PNG 是用户改错过后缀，不是坏文件）。
/// 于是这一列必须记录「解码器实际按什么解的」，否则 M4 的「类型筛选」会把
/// 那个 PNG 归到 JPEG 组里——那正是用户抱怨「筛出来不对」的来源。
///
/// ## 编号是**契约**，不是自增
///
/// 编号一旦发版就不能改：v6 升级时按扩展名预填（嗅探留给 L2），若后来给
/// `png` 换个号，老库里所有 PNG 都会变成未知格式。所以 20 起的 RAW 段留了空档，
/// 由 `08-B` §A-2 / `08-C` §A-2 追加（`rwl` / `rw2` 是后补的两个厂商容器）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i64)]
pub enum PhotoFormat {
    /// 0 · 还没识别（升级前的行、或嗅探失败）
    Unknown = 0,
    /// 1 · JPEG（含 `.jfif`）
    Jpeg = 1,
    /// 2 · PNG
    Png = 2,
    /// 3 · GIF（**只取第一帧**，动图不播放）
    Gif = 3,
    /// 4 · BMP / DIB
    Bmp = 4,
    /// 5 · TIFF（基线 + LZW + Deflate）
    Tiff = 5,
    /// 6 · WebP（动图取第一帧）
    Webp = 6,
    /// 7 · HEIC / HEIF（B 级，feature `heif`）
    Heic = 7,
    /// 8 · AVIF（B 级）
    Avif = 8,
    /// 9 · SVG（A+ 级，`resvg` 栅格化；矢量，不是照片）
    Svg = 9,
    /// 20 · DNG（Leica / 通用原始格式，TIFF 容器）
    Dng = 20,
    /// 21 · Canon CR2
    Cr2 = 21,
    /// 22 · Nikon NEF
    Nef = 22,
    /// 23 · Sony ARW
    Arw = 23,
    /// 24 · Canon CR3（ISOBMFF 容器，不是 TIFF）
    Cr3 = 24,
    /// 25 · Fuji RAF
    Raf = 25,
    /// 26 · Leica / Panasonic RWL
    Rwl = 26,
    /// 27 · Panasonic RW2（与 RWL 同一解析器）
    Rw2 = 27,
    /// 29 · 其他未单独支持的 RAW 厂商（进 `Unsupported(RawVendor)` 可见队列）
    RawOther = 29,
}

impl PhotoFormat {
    pub fn code(self) -> i64 {
        self as i64
    }

    /// 筛选面板的格式名 → 编码。`raw` 展开成 RAW 家族全体。
    /// （`photos_filtered` 的 `formats` 集合用小写 slug。）
    pub fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "jpeg" => Self::Jpeg,
            "png" => Self::Png,
            "gif" => Self::Gif,
            "bmp" => Self::Bmp,
            "tiff" => Self::Tiff,
            "webp" => Self::Webp,
            "svg" => Self::Svg,
            "heic" => Self::Heic,
            "avif" => Self::Avif,
            "dng" => Self::Dng,
            "cr2" => Self::Cr2,
            "nef" => Self::Nef,
            "arw" => Self::Arw,
            "cr3" => Self::Cr3,
            _ => return None,
        })
    }


    /// 从库里的整数还原。**不猜**：未知编码返回 `Unknown` 而不是随便归一，
    /// 这样「库比程序新」时会显示成未知格式，而不是错误地当成 JPEG。
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Jpeg,
            2 => Self::Png,
            3 => Self::Gif,
            4 => Self::Bmp,
            5 => Self::Tiff,
            6 => Self::Webp,
            7 => Self::Heic,
            8 => Self::Avif,
            9 => Self::Svg,
            20 => Self::Dng,
            21 => Self::Cr2,
            22 => Self::Nef,
            23 => Self::Arw,
            24 => Self::Cr3,
            25 => Self::Raf,
            26 => Self::Rwl,
            27 => Self::Rw2,
            29 => Self::RawOther,
            _ => Self::Unknown,
        }
    }

    /// 扩展名 → 格式（**迁移预填**与「按扩展名入候选」共用这一张表）。
    ///
    /// 注意这是**候选**不是结论：真正的格式以魔数为准（FORMAT-01）。
    pub fn from_extension(ext: &str) -> Option<Self> {
        let e = ext.trim_start_matches('.').to_ascii_lowercase();
        Some(match e.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "gif" => Self::Gif,
            "bmp" | "dib" => Self::Bmp,
            "tif" | "tiff" => Self::Tiff,
            "webp" => Self::Webp,
            "heic" | "heif" | "hif" => Self::Heic,
            "avif" => Self::Avif,
            "svg" | "svgz" => Self::Svg,
            "dng" => Self::Dng,
            "cr2" => Self::Cr2,
            "nef" => Self::Nef,
            "arw" => Self::Arw,
            "cr3" => Self::Cr3,
            "raf" => Self::Raf,
            "rwl" => Self::Rwl,
            "rw2" => Self::Rw2,
            _ => return None,
        })
    }

    pub fn extension_list(self) -> &'static [&'static str] {
        match self {
            Self::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Self::Png => &["png"],
            Self::Gif => &["gif"],
            Self::Bmp => &["bmp", "dib"],
            Self::Tiff => &["tif", "tiff"],
            Self::Webp => &["webp"],
            Self::Heic => &["heic", "heif", "hif"],
            Self::Avif => &["avif"],
            Self::Svg => &["svg", "svgz"],
            Self::Dng => &["dng"],
            Self::Cr2 => &["cr2"],
            Self::Nef => &["nef"],
            Self::Arw => &["arw"],
            Self::Cr3 => &["cr3"],
            Self::Raf => &["raf"],
            Self::Rwl => &["rwl"],
            Self::Rw2 => &["rw2"],
            Self::Unknown | Self::RawOther => &[],
        }
    }

    /// 界面文案（**唯一措辞来源**：筛选器与失败列表都用这个）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Gif => "GIF",
            Self::Bmp => "BMP",
            Self::Tiff => "TIFF",
            Self::Webp => "WebP",
            Self::Heic => "HEIC",
            Self::Avif => "AVIF",
            Self::Svg => "SVG",
            Self::Dng => "DNG",
            Self::Cr2 => "CR2",
            Self::Nef => "NEF",
            Self::Arw => "ARW",
            Self::Cr3 => "CR3",
            Self::Raf => "RAF",
            Self::Rwl => "RWL",
            Self::Rw2 => "RW2",
            Self::RawOther => "RAW",
        }
    }

    /// 是不是 RAW（筛选器「RAW」聚合项 = 20～29，`08-C` §A-2）。
    pub fn is_raw(self) -> bool {
        (20..=29).contains(&self.code())
    }

    /// 本版**是否真的能解码**。`false` 的格式仍然入库（用户能看见「有这个文件」），
    /// 只是缩略图进可见失败队列——**静默丢弃才是缺陷**（R-M2-4）。
    ///
    /// B/C 级按 P-27 裁决与编译 feature 决定，这里给的是「A 级 + JPEG」的确定答案。
    pub fn decodable(self) -> bool {
        matches!(
            self,
            Self::Jpeg
                | Self::Png
                | Self::Gif
                | Self::Bmp
                | Self::Tiff
                | Self::Webp
        )
    }
}

/// 拍摄时间从哪来（`photos.taken_from`，`07-A` FORMAT-06）。
///
/// M4 FILTER-04 的「无日期」筛选就是 `taken_from = 2`——它要找的是
/// 「EXIF 里没有拍摄时间、只能用文件修改时间凑数」的照片，这批照片的日期不可信。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum TakenFrom {
    Unknown = 0,
    Exif = 1,
    Mtime = 2,
}

impl TakenFrom {
    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::Exif,
            2 => Self::Mtime,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "未知",
            Self::Exif => "EXIF",
            Self::Mtime => "文件时间",
        }
    }
}

/// 「本层数 + 递归数 + 所有照片」的同事务快照（`07` §4.1）。
///
/// 三项一起给的理由是**五处一致**（R-M22-6 扩成六处）：
/// 树节点、树 tooltip、「关注文件夹」行、「所有照片」、组头 N、子树标题计数——
/// 只要有两个数字来自不同查询，就可能差 1，而用户一眼就能看出「这里 13、那里 12」。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CountSnapshot {
    /// 每目录**本层**照片数（`folders.direct_count`）
    pub direct: std::collections::HashMap<i64, u32>,
    /// 每目录**含子目录**的照片数（由 `direct` 在同一事务里自底向上聚合出来）
    pub totals: std::collections::HashMap<i64, u32>,
    /// 「所有照片」：只数 active 根，含失败项与离线项，不含已关闭根
    pub all_photos: u64,
}

/// 自底向上把 `direct` 聚合成 `totals`（`07` §4.1）。
///
/// 算法：按 `depth` 降序累加到父。SQLite 没有现成的闭包表，
/// 而递归 CTE 在「每个节点都要一个数」的场合要跑 N 次（每次从该节点向下展开），
/// 复杂度 O(N·depth)。这里的做法是**一次遍历**：对每个节点，
/// 沿 parent 链往上加——总代价 O(N·depth)，但常数小得多（纯 HashMap 查），
/// 10 万目录 × 平均深 4 层 ≈ 40 万次哈希查找，实测亚毫秒。
///
/// 为什么不按 depth 排序：那要先算深度（又一次遍历）。直接沿着 parent 链走，
/// 天然是「自底向上」的——叶子把数给父亲时，父亲自己的数已经收齐了它的所有孩子
/// （因为每个孩子都只有一条 parent 链，不存在漏加）。
///
/// ⚠️ parent 链成环会导致死循环。SQLite 的 `parent_id` 由 `derive_hierarchy` 推导，
/// 不会成环；但库被手工改过就可能。所以这里加了深度上限兜底——宁可数字不对，
/// 不能让 UI 线程卡死。
pub fn aggregate_totals(
    direct: &std::collections::HashMap<i64, u32>,
    parents: &std::collections::HashMap<i64, Option<i64>>,
) -> std::collections::HashMap<i64, u32> {
    /// 深度上限：正常目录树不超过 12 层（`filters::DEFAULT_MAX_DEPTH`），
    /// 留 8 倍余量。超了就当根处理，避免环导致死循环。
    const MAX_WALK: usize = 96;

    let mut totals: std::collections::HashMap<i64, u32> =
        direct.iter().map(|(id, n)| (*id, *n)).collect();

    // 对每个节点，把自己的数沿 parent 链往上加。
    for (id, own) in direct {
        if *own == 0 {
            continue; // 加 0 不改变任何东西，省掉整条链的遍历
        }
        let mut cur = *id;
        let mut hops = 0;
        while let Some(Some(parent)) = parents.get(&cur) {
            let parent = *parent;
            hops += 1;
            if hops > MAX_WALK {
                break;
            }
            if let Some(slot) = totals.get_mut(&parent) {
                *slot = slot.saturating_add(*own);
            }
            cur = parent;
        }
    }
    totals
}

/// 一个关注根的监视配置（`07` §8.1）。
///
/// 只有**用户意图**：开不开、轮询间隔。运行时状态
/// （`WatchState::{Active,Polling,Overflow,Off}`）不入库——
/// 换个网络环境就变的东西存进库只会说谎。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootWatchRow {
    pub root_id: i64,
    pub path: String,
    /// `roots.watch_enabled`
    pub enabled: bool,
    /// `roots.poll_interval_s`；`None` = 用默认值
    pub poll_interval_s: Option<i64>,
}
