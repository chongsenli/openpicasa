//! B 类偏好（会话状态）。< 10 KB，丢了只影响体验，不影响数据。
//!
//! 落盘位置由 [`crate::paths::AppPaths::preferences_file`] 决定，**永远不在照片目录**。

use crate::{Result, Tier, paths::AppPaths};
use picasa_db::SortSpec;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PREFS_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub version: u32,
    /// 已加入图库的根目录（绝对路径，NFC）。
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

    #[test]
    fn adding_same_root_twice_is_idempotent() {
        let mut prefs = Preferences::default();
        prefs.add_root(Path::new("/a/b"));
        prefs.add_root(Path::new("/a/b/"));
        assert_eq!(prefs.roots.len(), 1);
    }
}
