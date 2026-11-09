//! M4 · 索引文件导出 / 导入引擎（docs/11 §2.2～§2.5）。
//!
//! ## 关于 `clippy.toml` 的 `disallowed_methods`（R-1）
//!
//! 该规则禁止 `std::fs::remove_file`，是为了守住「**照片目录零写入**」——
//! 用户照片文件只能通过 `trash::move_to_trash` 删除。本模块里所有 `remove_file`
//! 的目标都是**应用容器内的临时文件**：导出 `.tmp`、导入 `staging-*.db`、
//! 写权限探测 `.opicasa-write-probe-*`、中断残留清理。它们由本模块自己创建、
//! 只含派生索引数据、绝不在任何图库根目录下（调用方保证 `work_dir` 在容器内、
//! `target` 过了 `check_export_dir`）。因此这里整模块豁免该规则，但**仍然**
//! 严格禁止删除任何照片文件——本模块从不接收照片目录路径，也不 `open` 任何照片。
#![allow(clippy::disallowed_methods)]
//!
//! ## 设计：为什么是「一个 SQLite 文件」而不是 JSON / ZIP
//!
//! - 与现有 `picasa-db` 同构：**事务、原子提交、迁移框架、完整性检查全部复用**，
//!   不需要把 20 多张表序列化一遍再反序列化一遍（那才是 bug 温床）。
//! - 不引入任何新依赖：`VACUUM INTO` 产出完整自洽的库快照（WAL 未 checkpoint 也不丢事务），
//!   `ATTACH DATABASE` 让「导入」变成纯 SQL 的 `INSERT … SELECT`，天然事务化。
//! - 原始照片**永远不进这个文件**：`thumbs` 表可选，其余全是应用自有索引与整理数据。
//!
//! ## 安全模型（R-M4-1 / R-M4-2）
//!
//! - 所有临时文件都落在**应用容器目录**（调用方传入的 `work_dir` = `AppPaths.support`），
//!   绝不落在任何图库根目录内；导出目标由调用方先过 `check_export_dir`。
//! - 导出：先写 `<目标>.<随机>.tmp` → 写清单 → 自校验 → 原子 `rename`；
//!   任何失败都删 tmp，**不产生半成品文件**，现有库只读。
//! - 导入：先 `copy` 到容器内的 staging 文件 → 全套校验 → `ATTACH` →
//!   在**单个事务**内合并/替换 → 提交成功才删 staging；失败/取消/强杀后
//!   现有索引保持可用（staging 由下次启动的 `cleanup_staging` 清理）。

use crate::{Db, DbError, Result};
use crate::m4::{
    ContentFlags, ImportReport, ImportStrategy, IndexManifest, ManifestCounts, ManifestRoot,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// SQLite `application_id` 的 magic（"OPIX" 的 ASCII）。写进导出文件头，
/// 用于「这不是 OpenPicasa 索引文件」的快速判定（`docs/11` §2.2.2）。
const MAGIC: i64 = 0x4F50_4958;
/// 当前支持的索引文件格式版本。
pub const FORMAT_VERSION: i64 = 1;

/// 导出结果（`M4-IDX-25`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub counts: ManifestCounts,
    /// 是否附带缩略图（索引包）。
    pub with_thumbs: bool,
}

/// 导入前预览（`M4-IDX-31/32`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPreview {
    pub manifest: IndexManifest,
    /// 与现有库重复的记录数（按 `(folder 路径, name_key)` 粗算）。
    pub duplicates: i64,
    /// 路径在本机不存在的来源根目录数。
    pub unmatched_roots: i64,
    /// 将被覆盖（替换策略）或保留（合并策略）的现有独有数据量。
    pub existing_only_user_data: i64,
    /// M4.2 `M42-UI-07`：导入向导「选前缩略图预览」用的包内缩略图。
    ///
    /// 每项 = `(photo_id, 缩略图字节)`。**只读索引包 `thumbs` 表的 `data` BLOB，
    /// 绝不读取原始照片**；`with_thumbs=false` 的包（导出时 `DELETE FROM thumbs`）
    /// 或读取失败时为空 Vec，优雅降级、不阻断导入流程。
    pub thumbs: Vec<(i64, Vec<u8>)>,
}

/// 可理解的错误原因（`M4-IDX-14/51`）。全部是能直接展示给用户的人话。
fn reject(msg: impl Into<String>) -> DbError {
    DbError::Other(msg.into())
}

/// M4.2 `M42-UI-07`：导入向导「选前缩略图预览」最多读取的张数（docs/16 §3，N 取 6~12）。
const PREVIEW_THUMB_LIMIT: i64 = 12;

/// M4.2 `M42-UI-07`：从索引包（staging 副本）里读出最多 `limit` 张缩略图。
///
/// - **只读 `thumbs` 表的 `data` BLOB，绝不读取原始照片**（R-M42-2：数据层零改动、
///   不写照片目录）。`inspect_index` 传进来的是 `stage_and_validate` 复制出的副本连接。
/// - `thumbs` 以 `(photo_id, tier)` 为主键，一张照片可能有多级（T0/T1）；用相关子查询
///   取每张照片最小 tier 的那一行，保证 `photo_id` 不重复、预览格不出现同一张两次。
/// - **优雅降级**：旧版包缺 `thumbs` 表 / `with_thumbs=false` 导出时已 `DELETE FROM thumbs`
///   导致空表 / 查询失败 / 单行 BLOB 损坏，一律返回空 Vec，`不报错、不阻断导入`。
///   这里刻意不用 `Result`：预览是锦上添花，任何读取问题都不该让整个预览失败。
fn read_package_thumbs(conn: &Connection, limit: i64) -> Vec<(i64, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(mut st) = conn.prepare(
        "SELECT t.photo_id, t.data FROM thumbs t
         WHERE t.tier = (SELECT MIN(t2.tier) FROM thumbs t2 WHERE t2.photo_id = t.photo_id)
         ORDER BY t.photo_id
         LIMIT ?1",
    ) else {
        return out;
    };
    let Ok(rows) = st.query_map([limit], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    }) else {
        return out;
    };
    out.extend(rows.flatten());
    out
}

// ---------------------------------------------------------------- 校验和

/// FNV-1a 64 位，覆盖清单里除 `payload_checksum` 自身以外的全部字段。
/// 不引 sha2 依赖（离线 registry 没有），与 `movematch::head_hash` 同款自写哈希。
fn manifest_checksum(m: &IndexManifest) -> String {
    let mut s = String::new();
    s.push_str(&m.format_version.to_string());
    s.push('|');
    s.push_str(&m.min_reader_version);
    s.push('|');
    s.push_str(&m.app_version);
    s.push('|');
    s.push_str(&m.schema_version.to_string());
    s.push('|');
    s.push_str(&m.created_at.to_string());
    s.push('|');
    s.push_str(&serde_json::to_string(&m.content_flags).unwrap_or_default());
    s.push('|');
    s.push_str(&serde_json::to_string(&m.counts).unwrap_or_default());
    s.push('|');
    s.push_str(&serde_json::to_string(&m.source_roots).unwrap_or_default());
    s.push('|');
    s.push_str(&m.notes);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn read_manifest(conn: &Connection) -> Result<IndexManifest> {
    let get = |key: &str| -> Result<Option<String>> {
        conn.query_row(
            "SELECT value FROM index_manifest WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(Into::into)
    };
    let m = IndexManifest {
        format_version: get("format_version")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        min_reader_version: get("min_reader_version")?.unwrap_or_default(),
        app_version: get("app_version")?.unwrap_or_default(),
        schema_version: get("schema_version")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        created_at: get("created_at")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        content_flags: get("content_flags")?
            .and_then(|v| serde_json::from_str::<ContentFlags>(&v).ok())
            .unwrap_or_default(),
        counts: get("counts")?
            .and_then(|v| serde_json::from_str::<ManifestCounts>(&v).ok())
            .unwrap_or_default(),
        source_roots: get("source_roots")?
            .and_then(|v| serde_json::from_str::<Vec<ManifestRoot>>(&v).ok())
            .unwrap_or_default(),
        payload_checksum: get("payload_checksum")?.unwrap_or_default(),
        notes: get("notes")?.unwrap_or_default(),
    };
    Ok(m)
}

fn write_manifest(conn: &Connection, m: &IndexManifest) -> Result<()> {
    let pairs: Vec<(&str, String)> = vec![
        ("format_version", m.format_version.to_string()),
        ("min_reader_version", m.min_reader_version.clone()),
        ("app_version", m.app_version.clone()),
        ("schema_version", m.schema_version.to_string()),
        ("created_at", m.created_at.to_string()),
        (
            "content_flags",
            serde_json::to_string(&m.content_flags).unwrap_or_default(),
        ),
        ("counts", serde_json::to_string(&m.counts).unwrap_or_default()),
        (
            "source_roots",
            serde_json::to_string(&m.source_roots).unwrap_or_default(),
        ),
        ("payload_checksum", m.payload_checksum.clone()),
        ("notes", m.notes.clone()),
    ];
    let tx = conn.unchecked_transaction()?;
    for (k, v) in pairs {
        tx.execute(
            "INSERT INTO index_manifest(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![k, v],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------- 导出

/// 把当前索引库导出成**一个自洽的 SQLite 文件**（`M4-IDX-20～25`）。
///
/// - `db`：现有库（只读使用，不修改任何一行）。
/// - `target`：用户选择的目标路径（调用方保证已过 `check_export_dir`）。
pub fn export_index(
    db: &Db,
    target: &Path,
    with_thumbs: bool,
    app_version: &str,
    now: i64,
) -> Result<ExportReport> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 与 target 同目录的 tmp：跨文件系统 rename 不是原子的。
    let tmp = target.with_extension(format!(
        "opicasaidx.{}.tmp",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);

    // 1) VACUUM INTO：读事务语义，产物是完整自洽的库（含 WAL 里未 checkpoint 的事务）。
    let res = (|| -> Result<()> {
        db.conn
            .execute("VACUUM INTO ?1", params![tmp.to_string_lossy().as_ref()])?;

        // 2) 打开临时库，写 application_id magic + 清单。
        let out = Connection::open(&tmp)?;
        out.pragma_update(None, "application_id", MAGIC)?;
        if !with_thumbs {
            out.execute("DELETE FROM thumbs", [])?;
        }
        let counts = counts_from_conn(&out, with_thumbs)?;
        let mut m = IndexManifest {
            format_version: FORMAT_VERSION,
            min_reader_version: "0.4.0".to_string(),
            app_version: app_version.to_string(),
            schema_version: crate::schema::SCHEMA_VERSION,
            created_at: now,
            content_flags: ContentFlags {
                index_records: true,
                user_data: true,
                paths: true,
                thumbnails: with_thumbs,
            },
            counts,
            source_roots: roots_from_conn(&out)?,
            notes: String::new(),
            ..Default::default()
        };
        m.payload_checksum = manifest_checksum(&m);
        write_manifest(&out, &m)?;
        out.execute_batch("PRAGMA optimize; VACUUM;")?;
        drop(out);

        // 3) 自校验：重新打开、读回清单、比对校验和，确认文件不是半成品。
        let check = Connection::open(&tmp)?;
        let magic: i64 = check.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if magic != MAGIC {
            return Err(reject("导出文件自检失败：格式标识不匹配"));
        }
        let back = read_manifest(&check)?;
        if back.payload_checksum != manifest_checksum(&back) {
            return Err(reject("导出文件自检失败：清单校验和不匹配"));
        }
        drop(check);

        // 4) 原子落位。覆盖同名文件是调用方二次确认过的语义（P-M4-07）。
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();

    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let bytes = std::fs::metadata(target)?.len();
    let counts = db.manifest_counts(with_thumbs)?;
    Ok(ExportReport {
        path: target.to_path_buf(),
        bytes,
        counts,
        with_thumbs,
    })
}

fn counts_from_conn(conn: &Connection, with_thumbs: bool) -> Result<ManifestCounts> {
    let scalar = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
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

fn roots_from_conn(conn: &Connection) -> Result<Vec<ManifestRoot>> {
    let mut st = conn.prepare(
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
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------- 校验 / 预览

/// 打开一个索引文件做**全套校验**（`M4-IDX-11/14/30/51/52`）。
/// 返回 staging 文件路径（在 `work_dir` 内）与读出的清单；原文件只读不改。
fn stage_and_validate(src: &Path, work_dir: &Path) -> Result<(PathBuf, IndexManifest, Option<i64>)> {
    if !src.is_file() {
        return Err(reject("所选文件不存在或不是普通文件"));
    }
    std::fs::create_dir_all(work_dir)?;
    let staging = work_dir.join(format!(
        "import-staging-{}-{}.db",
        std::process::id(),
        now_millis()
    ));
    std::fs::copy(src, &staging)?;

    let res = (|| -> Result<(IndexManifest, Option<i64>)> {
        let conn = Connection::open(&staging)?;
        // 1) magic
        let magic: i64 = conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap_or(0);
        if magic != MAGIC {
            return Err(reject("不是 OpenPicasa 索引文件（格式标识不匹配）"));
        }
        // 2) SQLite 自身完整性
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error".into());
        if integrity != "ok" {
            return Err(reject("索引文件已损坏或不完整（SQLite 完整性检查失败）"));
        }
        // 3) 必需表
        let photos: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='photos'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !photos {
            return Err(reject("索引文件结构不完整：缺少 photos 表"));
        }
        // 4) 清单 + 校验和
        let m = read_manifest(&conn)?;
        if m.format_version == 0 {
            return Err(reject("不是 OpenPicasa 索引文件（缺少清单或格式版本为 0）"));
        }
        if m.payload_checksum.is_empty()
            || m.payload_checksum != manifest_checksum(&m)
        {
            return Err(reject("索引文件已损坏或不完整（清单校验和不匹配）"));
        }
        // 5) 版本识别
        if m.format_version > FORMAT_VERSION || m.schema_version > crate::schema::SCHEMA_VERSION {
            return Err(reject(format!(
                "该索引文件由更新版本的 OpenPicasa（{}，格式 v{}）生成，请升级应用后重试",
                m.app_version, m.format_version
            )));
        }
        // 6) 低版本：在 staging 副本上按 MIGRATIONS 顺序升级；原文件只读不改。
        let upgraded_from = if m.schema_version < crate::schema::SCHEMA_VERSION {
            let ver: i64 = conn
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| r.get(0))
                .unwrap_or(m.schema_version);
            for mig in crate::schema::MIGRATIONS.iter().filter(|x| x.version > ver) {
                conn.execute_batch(mig.sql).map_err(|_| {
                    reject(format!(
                        "旧版索引升级失败：{} 步骤；可先用生成该文件的旧版应用导出一次",
                        mig.name
                    ))
                })?;
            }
            Some(ver)
        } else {
            None
        };
        Ok((m, upgraded_from))
    })();

    match res {
        Ok(v) => Ok((staging, v.0, v.1)),
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            Err(e)
        }
    }
}

/// 导入前预览（`M4-IDX-31/32`）：不修改任何数据，返回冲突与路径匹配信息。
pub fn inspect_index(db: &Db, src: &Path, work_dir: &Path) -> Result<ImportPreview> {
    let (staging, manifest, _) = stage_and_validate(src, work_dir)?;
    // M4.2 `M42-UI-07`：在 staging 副本上读包内缩略图（只读 thumbs 表、绝不读原图）。
    // 用独立的短连接读，避免和下面比对用的 ATTACH 连接互相干扰；任何失败都降级为空。
    let thumbs = Connection::open(&staging)
        .map(|c| read_package_thumbs(&c, PREVIEW_THUMB_LIMIT))
        .unwrap_or_default();
    let staging_str = staging.to_string_lossy().to_string();
    // ATTACH 到现有库连接，才能在一条 SQL 里比对「导入文件 vs 现有库」。
    let attach = db.conn.execute_batch(&format!(
        "ATTACH DATABASE '{}' AS src",
        staging_str.replace('\'', "''")
    ));
    if let Err(e) = attach {
        let _ = std::fs::remove_file(&staging);
        return Err(e.into());
    }
    let duplicates: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM src.photos p
             JOIN src.folders f ON f.id = p.folder_id
             WHERE EXISTS (
                SELECT 1 FROM main.folders mf JOIN main.photos mp ON mp.folder_id = mf.id
                WHERE mf.path = f.path AND mp.name_key = p.name_key)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let unmatched_roots = manifest
        .source_roots
        .iter()
        .filter(|r| !Path::new(&r.path).exists())
        .count() as i64;
    let existing_only_user_data: i64 = db
        .conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM main.tags) + (SELECT COUNT(*) FROM main.captions)
                    + (SELECT COUNT(*) FROM main.albums) + (SELECT COUNT(*) FROM main.album_photos)
                    + (SELECT COUNT(*) FROM main.photos WHERE starred = 1)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let _ = db.conn.execute_batch("DETACH DATABASE src");
    let _ = std::fs::remove_file(&staging);
    Ok(ImportPreview {
        manifest,
        duplicates,
        unmatched_roots,
        existing_only_user_data,
        thumbs,
    })
}

// ---------------------------------------------------------------- 导入

/// 执行导入（`M4-IDX-33～42`）。
///
/// - 所有写入在**单个事务**内完成；`cancel` 在每个阶段之间检查（原子布尔）。
/// - 失败/取消 → 事务回滚 → 现有索引保持可用；staging 文件被删除。
/// - 原始照片不复制、不移动、不重命名（本函数只碰数据库）。
pub fn execute_import(
    db: &mut Db,
    src: &Path,
    work_dir: &Path,
    strategy: ImportStrategy,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<ImportReport> {
    use std::sync::atomic::Ordering;

    let (staging, manifest, upgraded_from) = stage_and_validate(src, work_dir)?;
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&staging);
        return Err(reject("导入已取消"));
    }

    // 替换策略：先在**事务之外**备份（`VACUUM INTO` 不能跑在事务内），
    // 备份失败就中止，现有索引一个字节不动。
    if strategy == ImportStrategy::Replace {
        let backup = work_dir.join(format!("pre-import-{}.db", now_millis()));
        db.conn.execute(
            "VACUUM main INTO ?1",
            params![backup.to_string_lossy().as_ref()],
        )?;
    }

    let res = (|| -> Result<ImportReport> {
        let staging_str = staging.to_string_lossy().to_string();
        // ATTACH 到**同一个连接**，才能在一个事务里跨库 INSERT … SELECT。
        db.conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS src",
            staging_str.replace('\'', "''")
        ))?;
        let result = (|| -> Result<ImportReport> {
            let tx = db.conn.unchecked_transaction()?;
            let mut report = ImportReport {
                upgraded_from,
                ..Default::default()
            };

            if strategy == ImportStrategy::Replace {
                // 清掉「用户数据」与派生记录，再整表搬入；顺序满足外键依赖。
                for t in [
                    "album_photos", "albums", "tags", "captions", "recent_searches",
                    "index_task_failures", "index_tasks", "thumbs", "photos", "folders",
                    "exclusions", "roots",
                ] {
                    clear_table(&tx, t)?;
                }
            }

            if cancel.load(Ordering::Relaxed) {
                return Err(reject("导入已取消"));
            }

            // ---- 身份键与 id 重映射（合并策略的核心，`docs/11` §2.4.4）
            //
            // 两个独立库的主键都是从 1 开始自增，直接 INSERT OR IGNORE 会因
            // PRIMARY KEY 冲突把导入记录整条丢掉（实测：目标已有 1 张、源有 3 张 →
            // 只剩 3 张而不是 4 张）。合并必须把源 id 平移到目标当前最大 id 之后，
            // 并在复制依附表时按映射改写外键。
            let before: i64 = tx.query_row("SELECT COUNT(*) FROM main.photos", [], |r| r.get(0))?;
            let ids_conflict: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM src.photos p JOIN src.folders f ON f.id=p.folder_id
                     WHERE EXISTS (SELECT 1 FROM main.folders mf JOIN main.photos mp
                                   ON mp.folder_id=mf.id
                                   WHERE mf.path=f.path AND mp.name_key=p.name_key)",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);

            let merge = strategy == ImportStrategy::Merge;
            // 平移量：目标库各表现有最大 id（空表为 0）。源 id + off 必然不冲突。
            let off = |tx: &rusqlite::Transaction<'_>, table: &str| -> Result<i64> {
                Ok(tx
                    .query_row(&format!("SELECT COALESCE(MAX(id),0) FROM main.{table}"), [], |r| {
                        r.get(0)
                    })
                    .unwrap_or(0))
            };
            let root_off = if merge { off(&tx, "roots")? } else { 0 };
            let folder_off = if merge { off(&tx, "folders")? } else { 0 };
            let photo_off = if merge { off(&tx, "photos")? } else { 0 };
            let album_off = if merge { off(&tx, "albums")? } else { 0 };
            let task_off = if merge { off(&tx, "index_tasks")? } else { 0 };
            let excl_off = if merge { off(&tx, "exclusions")? } else { 0 };
            let search_off = if merge { off(&tx, "recent_searches")? } else { 0 };

            // 每张表用 copy_table_shifted：id 与外键列按 off 平移后插入。
            // 参数 = (表, 需平移的「本表 id」列, 需平移的「外键列名 → 平移量」)。
            copy_shifted(&tx, "roots", root_off, &[])?;
            copy_shifted(&tx, "folders", folder_off, &[("root_id", root_off), ("parent_id", folder_off)])?;
            copy_shifted(&tx, "photos", photo_off, &[("folder_id", folder_off)])?;
            let after: i64 = tx.query_row("SELECT COUNT(*) FROM main.photos", [], |r| r.get(0))?;
            report.imported = after - before;
            report.skipped = manifest.counts.photos - report.imported;
            report.conflicts = ids_conflict;

            if cancel.load(Ordering::Relaxed) {
                return Err(reject("导入已取消"));
            }

            // ---- 用户数据 + 派生数据（星标随 photos 一起带过来）
            copy_shifted(&tx, "captions", 0, &[("photo_id", photo_off)])?;
            copy_shifted(&tx, "tags", 0, &[("photo_id", photo_off)])?;
            copy_shifted(&tx, "albums", album_off, &[])?;
            copy_shifted(&tx, "album_photos", 0, &[("album_id", album_off), ("photo_id", photo_off)])?;
            copy_shifted(&tx, "recent_searches", search_off, &[])?;
            copy_shifted(&tx, "exclusions", excl_off, &[("root_id", root_off)])?;
            copy_shifted(&tx, "index_tasks", task_off, &[("root_id", root_off)])?;
            copy_shifted(&tx, "index_task_failures", 0, &[("task_id", task_off)])?;
            copy_shifted(&tx, "thumbs", 0, &[("photo_id", photo_off)])?;
            tx.commit()?;
            Ok(report)
        })();
        // 无论成功失败都 DETACH
        let _ = db.conn.execute_batch("DETACH DATABASE src");
        result
    })();

    let _ = std::fs::remove_file(&staging);
    match res {
        Ok(mut r) => {
            r.failed = 0;
            Ok(r)
        }
        Err(e) => Err(e),
    }
}

/// 清理上次中断留下的 staging 文件（启动时调用；`M4-IDX-50`）。
pub fn cleanup_staging(work_dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(work_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("import-staging-")
                && name.ends_with(".db")
                && std::fs::remove_file(e.path()).is_ok()
            {
                n += 1;
            }
        }
    }
    n
}

/// 复制一张表，并把指定列的值按 `off` 平移（`M4` 合并策略的 id 重映射）。
///
/// - `id_off`：本表主键 `id` 的平移量（没有 id 列的表传 0）。
/// - `fk_offsets`：外键列名 → 平移量（引用哪张表就传那张表的 off）。
///
/// 用 `PRAGMA table_xinfo` 动态取普通列（跳过生成列），避免 v8 之后列漂移；
/// 生成列（`display_width` / `eff_taken_at` 等）不能 INSERT，必须排除。
/// `INSERT OR IGNORE` 保证「合并只新增、不覆盖、不删除」。
fn copy_shifted(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    id_off: i64,
    fk_offsets: &[(&str, i64)],
) -> Result<usize> {
    let mut cols: Vec<String> = Vec::new();
    {
        let mut st = tx.prepare(&format!("PRAGMA src.table_xinfo('{table}')"))?;
        let rows = st.query_map([], |r| {
            // table_xinfo: cid(0), name(1), type(2), notnull(3), dflt(4), pk(5), hidden(6)
            Ok((r.get::<_, String>(1)?, r.get::<_, i64>(6)?))
        })?;
        for row in rows {
            let (name, hidden) = row?;
            if hidden == 0 {
                cols.push(name);
            }
        }
    }
    if cols.is_empty() {
        return Ok(0);
    }
    // 目标列清单
    let target = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    // SELECT 表达式：需要平移的列写成 `col + off`，其余原样。
    let select = cols
        .iter()
        .map(|c| {
            let expr = if c == "id" && id_off != 0 {
                format!("\"{c}\" + {id_off}")
            } else if let Some((_, o)) = fk_offsets.iter().find(|(n, _)| n == c) {
                if *o != 0 {
                    format!("\"{c}\" + {o}")
                } else {
                    format!("\"{c}\"")
                }
            } else {
                format!("\"{c}\"")
            };
            format!("{expr} AS \"{c}\"")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO main.\"{table}\" ({target}) SELECT {select} FROM src.\"{table}\""
    );
    Ok(tx.execute(&sql, [])?)
}

/// 清空一张表（替换策略用，顺序由调用方保证满足外键）。
fn clear_table(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<()> {
    tx.execute(&format!("DELETE FROM main.\"{table}\""), [])?;
    Ok(())
}
/// `roots` 由调用方从 `MgmtSnapshot.roots` 传入。
pub fn check_export_dir(dir: &Path, roots: &[String]) -> Result<()> {
    if !dir.exists() {
        return Err(reject(format!("目标目录不存在：{}", dir.display())));
    }
    if !dir.is_dir() {
        return Err(reject(format!("目标不是目录：{}", dir.display())));
    }
    // 写权限探测：在目录内建一个临时文件再删（不碰照片目录内的任何已有文件）
    let probe = dir.join(format!(".opicasa-write-probe-{}", std::process::id()));
    if std::fs::write(&probe, b"").is_err() {
        return Err(reject(format!("目标目录不可写：{}", dir.display())));
    }
    let _ = std::fs::remove_file(&probe);
    for r in roots {
        let root = Path::new(r);
        if dir.starts_with(root) {
            return Err(reject(format!(
                "导出文件不能放在图库照片目录内（{}），请另选位置",
                root.display()
            )));
        }
    }
    Ok(())
}

/// 毫秒时间戳，只用于 staging/备份文件名（不是业务时间，不进库）。
fn now_millis() -> u128 {
    #[expect(clippy::disallowed_types)]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhotoInput, SortSpec, ThumbBuf};

    /// 造一个含 2 张照片 + 各自一张 T0 缩略图的临时库（用 tempfile 落盘，
    /// 避开 in-memory `VACUUM INTO` 的兼容问题）。
    fn seeded_db(path: &Path) -> (Db, Vec<i64>) {
        let db = Db::open(path).unwrap();
        let root = db.upsert_root("/photos", 0, false).unwrap();
        let fid = db.upsert_folder("/photos", "/photos", None).unwrap();
        db.set_folder_root(fid, root, None).unwrap();
        let mk = |name: &str, taken: i64| PhotoInput {
            name: name.to_string(),
            name_key: name.to_ascii_lowercase(),
            size_bytes: 1000,
            mtime_ns: taken * 1_000_000_000,
            width: Some(8),
            height: Some(8),
            orientation: Some(1),
            taken_at: Some(taken),
            taken_at_src: 0,
            format: 1,
        };
        db.apply_scan(fid, "/photos", &[mk("a.jpg", 100), mk("b.jpg", 200)], 0)
            .unwrap();
        let rows = db
            .photos_from_visible(None, SortSpec::default(), 20, 0)
            .unwrap();
        let ids: Vec<i64> = rows.iter().map(|p| p.id).collect();
        for (i, id) in ids.iter().enumerate() {
            db.put_thumb(
                *id,
                0,
                &ThumbBuf {
                    w: 8,
                    h: 8,
                    format: 0,
                    data: vec![0xFF, 0xD8, 0xFF, i as u8],
                },
            )
            .unwrap();
        }
        (db, ids)
    }

    /// M42-UI-07 TC：`with_thumbs=true` 导出 → `inspect_index` 读出包内缩略图，
    /// 且 photo_id 与源库一致、每个 id 只出现一次、不读取原图。
    #[test]
    fn inspect_index_reads_package_thumbs_when_export_has_thumbs() {
        let tmp = tempfile::tempdir().unwrap();
        let (src_db, ids) = seeded_db(&tmp.path().join("src.sqlite"));
        let out = tmp.path().join("with.opicasaidx");
        export_index(&src_db, &out, true, "0.4.2-test", 1000).unwrap();

        let dst_db = Db::open_in_memory().unwrap();
        let prev = inspect_index(&dst_db, &out, tmp.path()).unwrap();
        assert!(
            !prev.thumbs.is_empty(),
            "含缩略图包必须读出预览用的 thumbs"
        );
        let mut got: Vec<i64> = prev.thumbs.iter().map(|(id, _)| *id).collect();
        let mut want = ids.clone();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want, "photo_id 集合必须与源库一致");
        assert_eq!(
            got.iter().collect::<std::collections::HashSet<_>>().len(),
            got.len(),
            "同一 photo_id 不能因多 tier 重复"
        );
        assert!(
            prev.thumbs.iter().all(|(_, b)| !b.is_empty()),
            "每张都要带上真实的缩略图字节"
        );
    }

    /// M42-UI-07 TC：`with_thumbs=false` 的包（导出时 `DELETE FROM thumbs`）→
    /// `thumbs` 为空 Vec，且 `inspect_index` 仍 `Ok`（优雅降级、不阻断导入）。
    #[test]
    fn inspect_index_degrades_gracefully_when_package_has_no_thumbs() {
        let tmp = tempfile::tempdir().unwrap();
        let (src_db, _) = seeded_db(&tmp.path().join("src.sqlite"));
        let out = tmp.path().join("no_thumbs.opicasaidx");
        export_index(&src_db, &out, false, "0.4.2-test", 1000).unwrap();

        let dst_db = Db::open_in_memory().unwrap();
        let prev = inspect_index(&dst_db, &out, tmp.path()).unwrap();
        assert!(prev.thumbs.is_empty(), "无缩略图包必须优雅降级为空 Vec");
        assert_eq!(prev.manifest.format_version, FORMAT_VERSION);
        assert_eq!(prev.manifest.counts.photos, 2, "预览本身仍要正常完成");
    }
}
