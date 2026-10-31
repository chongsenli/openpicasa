//! 宽高与方向的状态机（`photos.dims_status` + `display_*` 生成列）。
//!
//! | dims_status | 含义 |
//! | --- | --- |
//! | 0 | 待回填（宽高未知） |
//! | 1 | 已完成 |
//! | 2 | 读取失败（文件损坏 / 格式不支持） |
//!
//! ## 生成列不在 Rust 里算
//!
//! `display_width` / `display_height` 是 SQLite 的 VIRTUAL 生成列：
//! 读时按 `orientation ∈ 5..8` 决定是否宽高互换。**Rust 侧不要重复实现这个规则**——
//! 两套折算必然在某次改动后不一致，而生成列在结构上不可能与 `orientation` 打架。
//!
//! VIRTUAL 列不进索引：M3 瀑布流只读不筛，够用；M5 真要按显示宽高筛选时再物化。

use crate::{Db, DimsStats, PhotoRow, Result};
use rusqlite::params;

impl Db {
    pub fn dims_stats(&self) -> Result<DimsStats> {
        let mut st = self.conn.prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN dims_status = 1 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN dims_status = 0 THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN dims_status = 2 THEN 1 ELSE 0 END), 0)
             FROM photos",
        )?;
        let row = st.query_row([], |r| {
            Ok(DimsStats {
                total: r.get::<_, i64>(0)? as u64,
                done: r.get::<_, i64>(1)? as u64,
                pending: r.get::<_, i64>(2)? as u64,
                failed: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(row)
    }

    /// 待回填宽高的照片（`folder_ids = None` 表示全库）。
    ///
    /// 走 `idx_photos_dims_pending` 部分索引，只扫 `dims_status = 0` 的行——
    /// 回填 5 万张库里剩下的 61 张时，不该每次都全表扫。
    pub fn photos_pending_dims(
        &self,
        folder_ids: Option<&[i64]>,
        limit: usize,
    ) -> Result<Vec<PhotoRow>> {
        let mut sql = String::from(
            "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                    p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                    p.thumb_state, p.decoder_ver
             FROM photos p JOIN folders f ON f.id = p.folder_id
             WHERE p.dims_status = 0",
        );
        if let Some(ids) = folder_ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let list = ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND p.folder_id IN ({list})"));
        }
        sql.push_str(&format!(" ORDER BY p.taken_at DESC LIMIT {limit}"));
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map([], crate::map_photo)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 写回一张照片的宽高 / 方向。
    ///
    /// `ok = false` 时只把 `dims_status` 置 2（读取失败），**不清空已有宽高**——
    /// 之前读到过的值仍然有效，清掉只会让 M3 瀑布流多一次无谓的重排。
    pub fn set_photo_dims(
        &self,
        photo_id: i64,
        width: Option<i64>,
        height: Option<i64>,
        orientation: Option<i64>,
        ok: bool,
    ) -> Result<()> {
        if ok {
            self.conn.execute(
                "UPDATE photos SET
                     width = ?2,
                     height = ?3,
                     orientation = COALESCE(?4, orientation, 1),
                     dims_status = 1
                 WHERE id = ?1",
                params![photo_id, width, height, orientation],
            )?;
        } else {
            self.conn.execute(
                "UPDATE photos SET dims_status = 2 WHERE id = ?1",
                params![photo_id],
            )?;
        }
        Ok(())
    }

    /// 读生成列（诊断页与 M3 瀑布流的口径校验用）。
    pub fn display_dims(&self, photo_id: i64) -> Result<Option<(Option<i64>, Option<i64>)>> {
        let mut st = self
            .conn
            .prepare("SELECT display_width, display_height FROM photos WHERE id = ?1")?;
        let mut rows = st.query(params![photo_id])?;
        match rows.next()? {
            Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
            None => Ok(None),
        }
    }

    /// 按 id 批量取行（`docs/05` §2.1「可见区域按需回填」的数据入口）。
    ///
    /// 栅格只把**视口里**那几个 id 递下来，一次几十个；不按目录取，
    /// 因为视口可能横跨多个分组，而按目录取会把整屏外的几百张也带出来。
    ///
    /// 分块是因为 SQLite 的变量个数上限随编译选项而变（旧版 999、新版 32766），
    /// 500 在所有版本上都安全。
    pub fn photos_by_ids(&self, ids: &[i64]) -> Result<Vec<PhotoRow>> {
        const CHUNK: usize = 500;
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let list = chunk
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT p.id, p.folder_id, f.path, p.name, p.size_bytes, p.mtime_ns,
                        p.width, p.height, p.orientation, p.taken_at, p.taken_at_src,
                        p.thumb_state, p.decoder_ver
                 FROM photos p JOIN folders f ON f.id = p.folder_id
                 WHERE p.id IN ({list})"
            );
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map([], crate::map_photo)?;
            out.extend(rows.collect::<std::result::Result<Vec<_>, _>>()?);
        }
        Ok(out)
    }

    /// 还有多少张待回填。
    ///
    /// 走 `idx_photos_dims_pending` 部分索引，只数索引里那些行，不做全表聚合——
    /// 回填线程每批都要问一次，用 [`Db::dims_stats`] 那种四路 `SUM` 会在 5 万行上
    /// 每 150 ms 扫一遍全表。
    pub fn pending_dims_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM photos WHERE dims_status = 0",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n.max(0) as u64)
    }

    /// 把「读取失败」改回「待回填」。返回改动行数。
    ///
    /// 对应 `Db::reset_failed_thumbs` 的同款语义：失败是**可撤销**的结论，
    /// 用户换了文件、装了新解码器、或从诊断页点「重试」时要用。
    pub fn reset_failed_dims(&self) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE photos SET dims_status = 0 WHERE dims_status = 2",
            [],
        )?;
        Ok(n)
    }
}
