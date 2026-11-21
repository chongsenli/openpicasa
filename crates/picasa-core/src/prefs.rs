//! B 类偏好（会话状态）。< 10 KB，丢了只影响体验，不影响数据。
//!
//! 落盘位置由 [`crate::paths::AppPaths::preferences_file`] 决定，**永远不在照片目录**。

use crate::{Result, Tier, ViewMode, paths::AppPaths, tag_counts::TagsSort};
use picasa_db::SortSpec;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const PREFS_VERSION: u32 = 1;

/// 界面外观：亮色 / 暗色 / 跟随系统。
///
/// **默认亮色**：产品既有默认是浅色，M5 不把默认改成跟系统或暗色。
/// 「跟随系统」在本进程里由 UI 层每帧把 OS 的深浅色写进 [`set_system_dark`]，
/// 因此 [`Appearance::is_dark`] 对 `System` 也能给出**实际**结果，而不是只看枚举。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// 亮色主题。
    #[default]
    Light,
    /// 暗色主题。
    Dark,
    /// 跟随操作系统外观（UI 层解析；无系统信息时按亮色）。
    System,
}

/// 最近一次从系统读到的深浅色（由 UI 层每帧写入；`false` = 亮色）。
static SYSTEM_DARK: AtomicBool = AtomicBool::new(false);

/// UI 层把系统深浅色同步进来（`true` = 系统暗色）。
pub fn set_system_dark(dark: bool) {
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
}

/// 当前系统是否为暗色（未读到系统信息时按亮色）。
pub fn system_is_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

impl Appearance {
    /// 是否暗色主题。`System` 按**实际系统外观**解析，不是只看枚举。
    pub fn is_dark(self) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => system_is_dark(),
        }
    }

    /// 是否「跟随系统」。
    pub fn is_system(self) -> bool {
        matches!(self, Self::System)
    }

    /// 把 `System` 落到具体的亮 / 暗色（其余值原样返回）。
    pub fn resolve_system(self) -> Self {
        match self {
            Self::System => {
                if system_is_dark() {
                    Self::Dark
                } else {
                    Self::Light
                }
            }
            other => other,
        }
    }

    /// 偏好里存的值（`"light"` / `"dark"` / `"system"`，与 serde 输出一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    /// ⚠️ **M2.1 起废弃（只读兼容）**：关注根目录的唯一真相源已迁到索引库的 `roots` 表。
    ///
    /// 这个字段现在只在两处出现：
    /// 1. 迁移时当种子读一次（`crate::migrate::seed_roots`）；
    /// 2. `roots` 表不可用（老库降级运行）时兜底，别让用户看到空图库。
    ///
    /// **不要再写它**。同时写 DB 与 JSON 就是双真相源，`docs/04` §3.2 按 P0 处理。
    /// 保留 3 个版本用于回读，M4 之后删除。
    pub roots: Vec<String>,
    /// 上次打开的视图（`folder:<path>` 或 `all` / `recent`）。
    pub last_view: Option<String>,
    /// 全局缩略图档位。
    pub tier: Tier,
    pub sort: SortSpec,
    /// 栅格滚动位置（像素），用于「重开后停在原处」。
    pub grid_scroll: f32,
    pub sidebar_width: f32,
    /// 左栏折叠起来的目录路径。
    pub collapsed: Vec<String>,
    /// 托盘里的照片 id（跨会话保留；M1 的托盘内容是可重建的索引数据，不是用户数据）。
    pub tray: Vec<i64>,
    /// 查看器背景：0 黑 / 1 白 / 2 中性灰（F2-11）。
    pub viewer_bg: u8,
    /// 查看器信息面板是否展开（F2-09「可折叠记忆」）。
    pub viewer_info_open: bool,
    /// 窗口几何（x, y, w, h）。
    pub window: Option<[f32; 4]>,
    /// 栅格视图模式（默认方格 / 纵向瀑布流 / 横向瀑布流）。
    pub view_mode: ViewMode,
    /// M4.3 IDX-22 ④：左栏标签列表的排序方式。**默认按名称**（P-M43-11 改变了
    /// 旧的「按数量降序」默认），组头菜单可切换，重启保留。
    pub tags_sort: TagsSort,
    /// M4.3 IDX-22 ⑥：是否在左栏列出 0 计数的标签。默认隐藏。
    pub show_zero_tags: bool,
    /// 界面外观（亮色 / 暗色 / 跟随系统），默认亮色。
    ///
    /// 正式入口在设置页；「跟随系统」由 UI 层解析成实际的亮 / 暗 token。
    pub appearance: Appearance,
    /// 栅格排序时把「已经有缩略图」的照片排在前面（用户要求）。
    ///
    /// 打开时两桶各自按当前排序键排：先整桶有图、再整桶没图。
    /// 默认**开着**——用户眼里「有一半格子是灰的」比「顺序被分成两段」更难受。
    pub thumbs_first: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: PREFS_VERSION,
            roots: Vec::new(),
            last_view: None,
            // F1-14：默认中档（原版默认也是中等密度），Ctrl+1/Ctrl+2 切换。
            tier: Tier::Medium,
            sort: SortSpec::default(),
            grid_scroll: 0.0,
            sidebar_width: 220.0,
            collapsed: Vec::new(),
            tray: Vec::new(),
            viewer_bg: 0,
            viewer_info_open: true,
            window: None,
            view_mode: ViewMode::default(),
            tags_sort: TagsSort::default(),
            show_zero_tags: false,
            appearance: Appearance::Light,
            thumbs_first: true,
        }
    }
}

impl Preferences {
    /// 从应用容器读；文件不存在或损坏都返回默认值（**绝不 panic，也绝不回退到照片目录**）。
    pub fn load(paths: &AppPaths) -> Self {
        let file = paths.preferences_file();
        match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice::<Preferences>(&bytes) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "偏好文件损坏，使用默认值（原文件保留，不覆盖）");
                    // 保留坏文件方便排查，但用默认值继续跑：偏好丢失不该阻塞启动。
                    Preferences {
                        version: PREFS_VERSION,
                        ..Preferences::default()
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
            Err(e) => {
                tracing::warn!(error = %e, "读偏好失败，使用默认值");
                Preferences::default()
            }
        }
    }

    /// 原子写：先写同目录 `.tmp`，再 `rename`。
    ///
    /// 注意 `rename` 的目标与源**都在应用容器内**——R-1 只禁止「往照片目录写」，
    /// 应用容器里怎么做原子写都行。
    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure()?;
        let file = paths.preferences_file();
        let tmp = file.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &file)?;
        Ok(())
    }

    pub fn add_root(&mut self, root: &Path) {
        let s = crate::paths::canonical_string(root);
        if !self.roots.contains(&s) {
            self.roots.push(s);
        }
    }

    pub fn remove_root(&mut self, root: &Path) {
        let s = crate::paths::canonical_string(root);
        self.roots.retain(|r| r != &s);
    }

    /// ⚠️ 仅供**迁移种子**与降级兜底使用；UI 请走 `Library::root_paths()`。
    pub fn root_paths(&self) -> Vec<PathBuf> {
        self.roots.iter().map(PathBuf::from).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> (tempfile::TempDir, AppPaths) {
        let dir = tempfile::tempdir().unwrap();
        let mut p = AppPaths::resolve();
        p.support = dir.path().join("support");
        p.preferences = dir.path().join("prefs");
        p.caches = dir.path().join("caches");
        p.logs = dir.path().join("logs");
        p.ensure().unwrap();
        (dir, p)
    }

    #[test]
    fn round_trip() {
        let (_d, p) = temp_paths();
        let mut prefs = Preferences::default();
        prefs.add_root(Path::new("/Users/x/Photos"));
        prefs.tier = Tier::Large;
        prefs.sidebar_width = 260.0;
        prefs.save(&p).unwrap();

        let back = Preferences::load(&p);
        assert_eq!(back.roots, vec!["/Users/x/Photos".to_string()]);
        assert_eq!(back.tier, Tier::Large);
        assert_eq!(back.sidebar_width, 260.0);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let (_d, p) = temp_paths();
        let prefs = Preferences::load(&p);
        assert!(prefs.roots.is_empty());
        assert_eq!(prefs.tier, Tier::Medium);
    }

    #[test]
    fn corrupt_file_does_not_panic() {
        let (_d, p) = temp_paths();
        std::fs::write(p.preferences_file(), b"{not json").unwrap();
        let prefs = Preferences::load(&p);
        assert_eq!(prefs.sidebar_width, 220.0);
        // 坏文件保留，便于排查
        assert!(p.preferences_file().exists());
    }

    /// M5-R3：外观默认亮色，显式切暗色后**重启仍保持**（走真实的 JSON 落盘）。
    #[test]
    fn m5_r3_appearance_round_trips_and_defaults_to_light() {
        let (_d, p) = temp_paths();

        // 1) 没写过偏好 → 默认亮色。
        let fresh = Preferences::load(&p);
        assert_eq!(fresh.appearance, Appearance::Light);
        assert_eq!(fresh.appearance.as_str(), "light");
        assert!(!fresh.appearance.is_dark());

        // 2) 切到暗色并落盘 → 重新加载仍是暗色。
        let mut prefs = Preferences::default();
        prefs.appearance = Appearance::Dark;
        prefs.save(&p).unwrap();
        let back = Preferences::load(&p);
        assert_eq!(back.appearance, Appearance::Dark);
        assert!(back.appearance.is_dark());
        assert_eq!(back.appearance.as_str(), "dark");

        // 2b) 「跟随系统」也能落盘并读回；解析结果按系统深浅色走。
        let mut prefs = Preferences::default();
        prefs.appearance = Appearance::System;
        prefs.save(&p).unwrap();
        let back = Preferences::load(&p);
        assert_eq!(back.appearance, Appearance::System);
        assert!(back.appearance.is_system());
        assert_eq!(back.appearance.as_str(), "system");
        set_system_dark(true);
        assert!(back.appearance.is_dark(), "System 必须按系统暗色解析");
        assert_eq!(back.appearance.resolve_system(), Appearance::Dark);
        set_system_dark(false);
        assert!(!back.appearance.is_dark());
        assert_eq!(back.appearance.resolve_system(), Appearance::Light);

        // 3) 旧偏好文件没有 `appearance` 字段时按默认（亮色）读，不报错。
        let json = std::fs::read_to_string(p.preferences_file()).unwrap();
        let without = json
            .lines()
            .filter(|l| !l.contains("appearance"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(p.preferences_file(), without).unwrap();
        assert_eq!(Preferences::load(&p).appearance, Appearance::Light);
    }

    #[test]
    fn adding_same_root_twice_is_idempotent() {
        let mut prefs = Preferences::default();
        prefs.add_root(Path::new("/a/b"));
        prefs.add_root(Path::new("/a/b/"));
        assert_eq!(prefs.roots.len(), 1);
    }
}
