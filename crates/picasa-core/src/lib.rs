//! # picasa-core · M1 的「引擎」
//!
//! 依赖方向（02 §6.2）：`picasa-ui → picasa-core → {picasa-db, picasa-imaging}`。
//! 这里是**唯一**决定「往哪儿写」的地方（见 [`paths`]），也是唯一碰文件系统的地方。
//!
//! ## R-1（照片目录零写入）在这一层怎么被守住
//!
//! 1. **可写路径只有一个来源**：[`paths::AppPaths`]。除此之外代码里没有第二处拼
//!    `Application Support` / `.xxx` 的地方，也**没有任何**「往照片目录写」的 API。
//! 2. 扫描与解码全部走 `File::open`（只读）。本 crate 没有 `OpenOptions::write`，
//!    也没有 `fs::create/write/rename/remove` 作用在用户目录上的调用点。
//! 3. 唯一的例外是「删除到废纸篓」（F2-08），它在 [`trash`] 里，独立成模块，
//!    **只能由用户的显式动作触发**，且目标是系统回收站而不是原地 unlink。

pub mod datasource;
pub mod dims;
pub mod filters;
pub mod index;
pub mod library;
pub mod mgmt;
pub mod migrate;
pub mod paths;
pub mod perf;
pub mod pipeline;
pub mod prefs;
pub mod scanner;
pub mod texbudget;
pub mod thumbs;
pub mod trash;
pub mod verify;
pub mod watch_mgr;
pub mod watcher;

pub use datasource::{
    Cursor, DataSource, GroupBound, IN_LIST_MAX, RANGE_MAX, ROW_WINDOW_SCREENS, Scope,
};
pub use dims::{DimsProgress, DimsService};
pub use texbudget::{
    MEM_HARD_LIMIT_MB, MEM_SOFT_LIMIT_MB, MemGuard, MemPressure, TextureBudget,
    texture_cache_budget_mb,
};
pub use index::{FailureItem, IndexBoard, IndexSnapshot, IndexState, TaskSnapshot};
pub use library::{Library, View};
pub use mgmt::{
    DECODE_TIMEOUT_MS, HISTORY_KEEP, SNAPSHOT_HZ_MAX, STARTUP_VERIFY_DELAY_MS,
    VERIFY_DIR_INTERVAL_MS, ExclusionView, FolderState, IndexEntryState, MgmtSnapshot,
    MgmtTab, OfflineProbe, RootView, SnapshotInput, StorageView, TREE_COUNT_MODE, TaskView,
    ThumbStorage, TreeCountMode, VerifyPlan, VerifyProgress, anonymize_path, apply_storage,
    build_snapshot, check_export_dir, collect_storage, default_offline_probe, diag_filename,
    live_task_views, path_is_offline,
};
pub use paths::AppPaths;
pub use watch_mgr::{RootWatchConfig, WatchManager, WatchOut};
pub use prefs::Preferences;
pub use scanner::{CoreEvent, IndexService, ScanHandle, spawn_scan, spawn_scan_with_board};
pub use thumbs::{THUMB_WORKERS, ThumbJob, ThumbService};

/// 核心层错误。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("db: {0}")]
    Db(#[from] picasa_db::DbError),
    #[error("imaging: {0}")]
    Imaging(#[from] picasa_imaging::ImageError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// 缩略图档位（三档离散值，**不可连续缩放**，01 F1-14 / 02 §8.2.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Tier {
    Small,
    Medium,
    Large,
}

impl Tier {
    pub fn index(self) -> usize {
        match self {
            Tier::Small => 0,
            Tier::Medium => 1,
            Tier::Large => 2,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Tier::Small,
            1 => Tier::Medium,
            _ => Tier::Large,
        }
    }

    /// 单元格边长（逻辑 pt）。
    ///
    /// ⚠️ **U-21 仍是「待截图核对」项**。这里的值是 02 §8.2.4 的估值，
    /// 集中在 [`crate::library::CELL_SIZES`] 一张常量表里——等 PM 用原版截图量出真值，
    /// **只改那三行，不动布局算法**。
    pub fn cell_size(self) -> f32 {
        match self {
            Tier::Small => 85.0,
            Tier::Medium => 144.0,
            Tier::Large => 200.0,
        }
    }

    /// 该档是否显示文件名（U-26）。
    ///
    /// **三档全显示**。`01` U-26 的原文是「缩略图**下方**居中显示文件名，单行、
    /// 超出省略号」，当时只有「哪几档显示」标了 `⚠️待核对`，实现先按
    /// 「只在大档显示」落地。用户明确要求「图片预览下面要展示图片的名字」，
    /// 即三档都要有——这也更接近原版 Picasa 3.9 的观感：
    /// 文件名是**辨认照片的主要线索**，只在大档出现等于默认视图里没有。
    ///
    /// ⚠️ 改这个返回值会改变 `GridLayout::cell_h`，进而改变**每屏格数**。
    /// 虚拟化按行号切分（ADR-0005），行高变化是安全的；但 `grid_scroll` 的
    /// 像素记忆在**切换档位后**会落到略微不同的位置——换档本来就会重排，
    /// 不是本改动引入的。
    pub fn shows_filename(self) -> bool {
        true
    }

    pub fn next_bigger(self) -> Self {
        match self {
            Tier::Small => Tier::Medium,
            Tier::Medium | Tier::Large => Tier::Large,
        }
    }

    pub fn next_smaller(self) -> Self {
        match self {
            Tier::Large => Tier::Medium,
            Tier::Medium | Tier::Small => Tier::Small,
        }
    }
}

#[cfg(test)]
mod tier_tests {
    use super::Tier;

    /// U-26：文件名**三档都要显示**。
    ///
    /// 这条曾经是 `matches!(self, Tier::Large)`（只在大档显示），而
    /// `GridLayout::cell_h` 是靠这个返回值算出来的——把它改回「只有大档」，
    /// 中档/小档的文件名会立刻消失，且**布局仍然是对的**（不会报错、不会错位），
    /// 所以只有用例能拦住。
    #[test]
    fn every_tier_shows_the_filename() {
        for tier in [Tier::Small, Tier::Medium, Tier::Large] {
            assert!(
                tier.shows_filename(),
                "{tier:?} 档没有显示文件名——U-26 要求缩略图下方始终显示"
            );
        }
    }

    /// 三档的缩略图边长必须严格递增（布局的列数是按 `cell_size` 算出来的）。
    #[test]
    fn cell_size_grows_with_tier() {
        assert!(Tier::Small.cell_size() < Tier::Medium.cell_size());
        assert!(Tier::Medium.cell_size() < Tier::Large.cell_size());
    }
}
