//! M4 · 索引管理版的数据层能力（docs/11 §2～§5）。
//!
//! 单独成文件的理由：`lib.rs` 已经 2700+ 行，且 `photo_cols()` / `map_photo()` /
//! `PhotoRow` 三者是「加一列就要同步改三处」的高风险链路（见 `tests/photo_columns.rs`）。
//! M4 的 EXIF 摘要用**独立的按需查询**（`PhotoExif`），不塞进 `PhotoRow`，
//! 因此本文件的新增列**不需要**碰那条链路。

use crate::{Db, DbError, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- 相册

/// 相册视图（创建 / 重命名 / 列表用）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRow {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    /// 成员数（列表页用，避免 N+1 查询）。
    pub photo_count: i64,
}

/// 一张照片的 EXIF 摘要（v8 列，全部可空 = 缺失，不编造）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoExif {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length: Option<String>,
    pub f_number: Option<String>,
    pub exposure_time: Option<String>,
    pub iso: Option<String>,
    /// 0=未读 / 1=OK / 2=缺失或读取失败
    pub status: i64,
    pub error: Option<String>,
}

/// 近期搜索条目（`query_json` 是 `PhotoFilter` + 搜索词的 JSON，UI 自行解释）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSearch {
    pub id: i64,
    pub query_json: String,
    pub created_at: i64,
}

/// 索引文件清单（导出时写、导入时读；`docs/11` §2.2.1）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexManifest {
    pub format_version: i64,
    pub min_reader_version: String,
    pub app_version: String,
    pub schema_version: i64,
    pub created_at: i64,
    pub content_flags: ContentFlags,
    pub counts: ManifestCounts,
    pub source_roots: Vec<ManifestRoot>,
    pub payload_checksum: String,
    pub notes: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentFlags {
    pub index_records: bool,
    pub user_data: bool,
    pub paths: bool,
    pub thumbnails: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestCounts {
    pub photos: i64,
    pub roots: i64,
    pub folders: i64,
    pub tags: i64,
    pub albums: i64,
    pub thumbs: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRoot {
    pub path: String,
    pub path_key: String,
    pub case_insensitive: bool,
    pub closed: bool,
    pub is_online: bool,
}

impl Default for IndexManifest {
    fn default() -> Self {
        Self {
            format_version: 1,
            min_reader_version: "0.4.0".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: crate::schema::SCHEMA_VERSION,
            created_at: 0,
            content_flags: ContentFlags {
                index_records: true,
                user_data: true,
                paths: true,
                thumbnails: false,
            },
            counts: ManifestCounts::default(),
            source_roots: Vec::new(),
            payload_checksum: String::new(),
            notes: String::new(),
        }
    }
}

/// 导入策略（`docs/11` §2.4.4）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportStrategy {
    /// 只新增、不删除现有记录；身份相同且内容不同保留现有。
    #[default]
    Merge,
    /// 以导入文件为准，现有独有的整理数据会被移除（执行前先备份）。
    Replace,
}

/// 导入结果（`docs/11` §2.4 结果报告）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: i64,
    pub skipped: i64,
    pub failed: i64,
    pub conflicts: i64,
    pub failures: Vec<String>,
    pub upgraded_from: Option<i64>,
}

impl Db {
    /// 表是否存在（测试与诊断用；只查 `sqlite_master`，不改数据）。
    pub fn table_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![name],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    // ------------------------------------------------------------ 相册 CRUD

    /// 新建相册；同名返回 `DbError::Other("同名相册已存在")`（不静默覆盖，`M4-ORG-11`）。
    pub fn create_album(&self, name: &str, now: i64) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Other("相册名称不能为空".into()));
        }
        let exists: Option<i64> = self
            .conn
            .query_row("SELECT id FROM albums WHERE name = ?1", params![name], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_some() {
            return Err(DbError::Other("同名相册已存在，请换一个名称".into()));
        }
        self.conn.execute(
            "INSERT INTO albums(name, created_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 重命名相册；目标名已被占用时报错。
    pub fn rename_album(&self, id: i64, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Err(DbError::Other("相册名称不能为空".into()));
        }
        let clash: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM albums WHERE name = ?1 AND id <> ?2",
                params![new_name, id],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(DbError::Other("同名相册已存在，请换一个名称".into()));
        }
        let n = self.conn.execute(
            "UPDATE albums SET name = ?1 WHERE id = ?2",
            params![new_name, id],
        )?;
        if n == 0 {
            return Err(DbError::Other("相册不存在".into()));
        }
        Ok(())
    }

    /// 删除相册：**只删定义与归属，不删照片**（CASCADE 只作用于 `album_photos`）。
    pub fn delete_album(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM albums WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// 全部相册（含成员计数）。
    pub fn all_albums(&self) -> Result<Vec<AlbumRow>> {
        let mut st = self.conn.prepare(
            "SELECT a.id, a.name, a.created_at,
                    (SELECT COUNT(*) FROM album_photos ap WHERE ap.album_id = a.id)
             FROM albums a ORDER BY a.created_at DESC, a.name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(AlbumRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                photo_count: r.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 相册成员 id（升序，稳定）。
    pub fn album_photo_ids(&self, album_id: i64) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare(
            "SELECT photo_id FROM album_photos WHERE album_id = ?1 ORDER BY photo_id",
        )?;
        let rows = st.query_map(params![album_id], |r| r.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 一张照片所属的相册（多对多，`M4-ORG-09`）。
    pub fn albums_of(&self, photo_id: i64) -> Result<Vec<AlbumRow>> {
        let mut st = self.conn.prepare(
            "SELECT a.id, a.name, a.created_at,
                    (SELECT COUNT(*) FROM album_photos x WHERE x.album_id = a.id)
             FROM albums a JOIN album_photos ap ON ap.album_id = a.id
             WHERE ap.photo_id = ?1 ORDER BY a.name",
        )?;
        let rows = st.query_map(params![photo_id], |r| {
            Ok(AlbumRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                photo_count: r.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 批量加入相册；重复去重（`M4-ORG-02/08`）。
    pub fn add_to_album(&self, album_id: i64, photo_ids: &[i64], now: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare(
                "INSERT OR IGNORE INTO album_photos(album_id, photo_id, added_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            for id in photo_ids {
                n += st.execute(params![album_id, id, now])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// 批量移出相册；只是取消归属，不碰照片（`M4-ORG-08`）。
    pub fn remove_from_album(&self, album_id: i64, photo_ids: &[i64]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare(
                "DELETE FROM album_photos WHERE album_id = ?1 AND photo_id = ?2",
            )?;
            for id in photo_ids {
                n += st.execute(params![album_id, id])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    // ------------------------------------------------------------ 标签定义（重命名/删除）

    /// 重命名标签：全表更新 label（`M4-ORG-01`）。v7 `tags` 以 label 为自然键，
    /// 改名必须一次事务内改完，否则会出现「一半旧名一半新名」。
    pub fn rename_tag(&self, old: &str, new: &str) -> Result<usize> {
        let new = new.trim();
        if new.is_empty() || old.trim().is_empty() {
            return Err(DbError::Other("标签名称不能为空".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        // 目标名已存在 → 合并（把 old 的行并进 new），去重靠主键。
        let target_exists: bool = tx
            .query_row(
                "SELECT 1 FROM tags WHERE label = ?1 LIMIT 1",
                params![new],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        let n = if target_exists {
            tx.execute(
                "INSERT OR IGNORE INTO tags(photo_id, label)
                 SELECT photo_id, ?2 FROM tags WHERE label = ?1",
                params![old, new],
            )?;
            tx.execute("DELETE FROM tags WHERE label = ?1", params![old])?
        } else {
            tx.execute("UPDATE tags SET label = ?2 WHERE label = ?1", params![old, new])?
        };
        tx.commit()?;
        Ok(n)
    }

    /// 删除标签：从所有照片移除该标签，**不删照片**（`M4-ORG-01`）。
    pub fn delete_tag(&self, label: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM tags WHERE label = ?1", params![label])?)
    }

    // ------------------------------------------------------------ EXIF 摘要

    /// 写入一张照片的 EXIF 摘要（扫描/回填时调用；只写自有库，不碰原图）。
    pub fn set_photo_exif(&self, photo_id: i64, e: &PhotoExif) -> Result<()> {
        self.conn.execute(
            "UPDATE photos SET camera_make=?1, camera_model=?2, lens_model=?3,
                    focal_length=?4, f_number=?5, exposure_time=?6, iso=?7,
                    exif_status=?8, exif_error=?9
             WHERE id=?10",
            params![
                e.camera_make,
                e.camera_model,
                e.lens_model,
                e.focal_length,
                e.f_number,
                e.exposure_time,
                e.iso,
                e.status,
                e.error,
                photo_id
            ],
        )?;
        Ok(())
    }

    /// 批量写入 EXIF 摘要（单事务，M4 回填用）。
    ///
    /// 性能：旧路径每张照片一条独立 `UPDATE` = N 次事务提交，在 WAL 下会把
    /// 索引 / 缩略图写入线程一起拖慢。这里一次 `BEGIN … COMMIT` 全部落库。
    pub fn set_photo_exif_batch(&self, items: &[(i64, PhotoExif)]) -> Result<usize> {
        if items.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare(
                "UPDATE photos SET camera_make=?1, camera_model=?2, lens_model=?3,
                        focal_length=?4, f_number=?5, exposure_time=?6, iso=?7,
                        exif_status=?8, exif_error=?9
                 WHERE id=?10",
            )?;
            for (id, e) in items {
                n += st.execute(params![
                    e.camera_make,
                    e.camera_model,
                    e.lens_model,
                    e.focal_length,
                    e.f_number,
                    e.exposure_time,
                    e.iso,
                    e.status,
                    e.error,
                    id
                ])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// 按需读一张照片的 EXIF 摘要（查看器信息面板用）。
    pub fn photo_exif(&self, photo_id: i64) -> Result<PhotoExif> {
        let mut st = self.conn.prepare(
            "SELECT camera_make, camera_model, lens_model, focal_length, f_number,
                    exposure_time, iso, exif_status, exif_error
             FROM photos WHERE id = ?1",
        )?;
        st.query_row(params![photo_id], |r| {
            Ok(PhotoExif {
                camera_make: r.get(0)?,
                camera_model: r.get(1)?,
                lens_model: r.get(2)?,
                focal_length: r.get(3)?,
                f_number: r.get(4)?,
                exposure_time: r.get(5)?,
                iso: r.get(6)?,
                status: r.get(7)?,
                error: r.get(8)?,
            })
        })
        .map_err(Into::into)
    }

    /// 待回填 EXIF 的照片 id（`exif_status = 0`），分批返回。
    pub fn photos_pending_exif(&self, limit: usize) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare(
            "SELECT id FROM photos WHERE exif_status = 0 ORDER BY id LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64], |r| r.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 供「相机 / 镜头」筛选面板列出可选项（`M4-SEARCH-01`）。
    pub fn camera_options(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare(
            "SELECT DISTINCT COALESCE(camera_make,''), COALESCE(camera_model,'')
             FROM photos WHERE camera_make IS NOT NULL OR camera_model IS NOT NULL
             ORDER BY 1, 2",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn lens_options(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT DISTINCT lens_model FROM photos
             WHERE lens_model IS NOT NULL AND lens_model <> '' ORDER BY 1",
        )?;
        let rows = st.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    // ------------------------------------------------------------ 近期搜索

    /// 记录一次搜索（上限 10 条，`M4-SEARCH-07`）；完全相同的最近一条先删，避免堆积。
    pub fn push_recent_search(&self, query_json: &str, now: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM recent_searches WHERE query_json = ?1",
            params![query_json],
        )?;
        tx.execute(
            "INSERT INTO recent_searches(query_json, created_at) VALUES (?1, ?2)",
            params![query_json, now],
        )?;
        tx.execute(
            "DELETE FROM recent_searches WHERE id NOT IN
             (SELECT id FROM recent_searches ORDER BY created_at DESC, id DESC LIMIT 10)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn recent_searches(&self) -> Result<Vec<RecentSearch>> {
        let mut st = self.conn.prepare(
            "SELECT id, query_json, created_at FROM recent_searches
             ORDER BY created_at DESC, id DESC LIMIT 10",
        )?;
        let rows = st.query_map([], |r| {
            Ok(RecentSearch {
                id: r.get(0)?,
                query_json: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn clear_recent_searches(&self) -> Result<()> {
        self.conn.execute("DELETE FROM recent_searches", [])?;
        Ok(())
    }

    // ------------------------------------------------------------ 清单（导出/导入共用）

    pub fn set_manifest(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO index_manifest(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn manifest_value(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM index_manifest WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 导出前统计各类记录数（`M4-IDX-20` 预计内容）。
    pub fn manifest_counts(&self, with_thumbs: bool) -> Result<ManifestCounts> {
        let scalar = |sql: &str| -> Result<i64> {
            Ok(self.conn.query_row(sql, [], |r| r.get(0))?)
        };
        Ok(ManifestCounts {
            photos: scalar("SELECT COUNT(*) FROM photos")?,
            roots: scalar("SELECT COUNT(*) FROM roots")?,
            folders: scalar("SELECT COUNT(*) FROM folders")?,
            tags: scalar("SELECT COUNT(DISTINCT label) FROM tags")?,
            albums: scalar("SELECT COUNT(*) FROM albums")?,
            thumbs: if with_thumbs {
                scalar("SELECT COUNT(*) FROM thumbs")?
            } else {
                0
            },
        })
    }

    /// 来源根目录清单（导入后重新匹配的依据，`M4-IDX-39`）。
    pub fn manifest_roots(&self) -> Result<Vec<ManifestRoot>> {
        let mut st = self.conn.prepare(
            "SELECT r.path, r.path_key, r.case_insensitive, r.state,
                    COALESCE((SELECT MIN(f.is_online) FROM folders f WHERE f.root_id = r.id), 1)
             FROM roots r ORDER BY r.path",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ManifestRoot {
                path: r.get(0)?,
                path_key: r.get(1)?,
                case_insensitive: r.get::<_, i64>(2)? != 0,
                closed: r.get::<_, i64>(3)? != 0,
                is_online: r.get::<_, i64>(4)? != 0,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }
}
