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

pub mod filters;
pub mod index;
pub mod library;
pub mod migrate;
pub mod paths;
pub mod prefs;
pub mod scanner;
pub mod thumbs;
pub mod trash;
pub mod watcher;

pub use index::{FailureItem, IndexBoard, IndexSnapshot, IndexState, TaskSnapshot};
pub use library::{Library, View};
pub use paths::AppPaths;
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

    /// 该档是否显示文件名（U-26 待核对，暂定「只在大档显示」）。
    pub fn shows_filename(self) -> bool {
        matches!(self, Tier::Large)
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
