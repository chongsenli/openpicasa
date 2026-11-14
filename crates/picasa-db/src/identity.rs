//! 身份骨架：把「用户标记」从会随 `photos` 行 CASCADE 删除的状态里解耦出来（M4.3 / R-M43-02）。
//!
//! ## 为什么需要它
//!
//! `photos` 上的 `starred` 列，以及 `captions` / `tags` / `album_photos` 三张表都以
//! `photo_id` 为外键且 `ON DELETE CASCADE`。一旦 `clear_index` / `delete_folder` /
//! `apply_scan` 的 stale 清理执行 `DELETE FROM photos`，这些用户创作数据就被静默带走——
//! 这正是需求说明书 §0.1 / §3.1 点名的红线违规。
//!
//! 本模块在**删除前**把 live 标记按 `(folder_path, name_key)` 快照到 v9 的三张骨架表，
//! 在**重新扫描 INSERT 之后**按同一身份把标记挂回新的 `photo_id`。身份不含自增 id，
//! 因此重建/重加后 id 变化也能恢复（P-M43-01「永久保留 / 重加可恢复」）。
//!
//! ## 纪律
//!
//! - 所有函数只收 `&Connection`、**不自带事务**，由调用方（`clear_index` 的
//!   `unchecked_transaction`、`delete_folder` 的显式事务、`apply_scan` 的
//!   `BEGIN IMMEDIATE`）统一包住，保证「快照 + 删除」原子。
//! - 调用方必须先确认 v9 骨架表存在（见 `Db::has_identity`），否则 SQL 会报「no such table」。

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// 快照**一张**照片的 live 标记到骨架表。
///
/// 必须在 `DELETE FROM photos WHERE id = ?` **之前**调用（此时 live 行还在）。
/// 用 `INSERT OR REPLACE` + 先删后插骨架子表，保证重复快照是幂等的「以当前 live 为准」。
pub(crate) fn snapshot_photo(
    conn: &Connection,
    folder_path: &str,
    name_key: &str,
    photo_id: i64,
) -> Result<()> {
    let starred: i64 = conn
        .query_row(
            "SELECT COALESCE(starred, 0) FROM photos WHERE id = ?1",
            params![photo_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let caption: String = conn
        .query_row(
            "SELECT text FROM captions WHERE photo_id = ?1",
            params![photo_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();

    conn.execute(
        "INSERT OR REPLACE INTO photo_identities
             (folder_path, name_key, starred, caption)
         VALUES (?1, ?2, ?3, ?4)",
        params![folder_path, name_key, starred, caption],
    )?;
    conn.execute(
        "DELETE FROM photo_identity_tags WHERE folder_path = ?1 AND name_key = ?2",
        params![folder_path, name_key],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO photo_identity_tags (folder_path, name_key, label)
         SELECT ?1, ?2, label FROM tags WHERE photo_id = ?3",
        params![folder_path, name_key, photo_id],
    )?;
    conn.execute(
        "DELETE FROM photo_identity_albums WHERE folder_path = ?1 AND name_key = ?2",
        params![folder_path, name_key],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO photo_identity_albums (folder_path, name_key, album_name)
         SELECT ?1, ?2, a.name
           FROM album_photos ap JOIN albums a ON a.id = ap.album_id
          WHERE ap.photo_id = ?3",
        params![folder_path, name_key, photo_id],
    )?;
    Ok(())
}

/// 集合式快照内核：一次 `INSERT ... SELECT` 处理整批照片，**不逐张循环**。
///
/// 为什么必须是这样：逐张版本（`snapshot_photo` 循环）对每张照片跑 6 条语句，
/// 9.6 万张就是约 58 万条语句，全在一个事务里、期间一直持有 DB 锁——
/// 用户点「删除索引」时界面会冻结五十秒（实机复现，v0.7-dev20）。
/// 集合式把整批压成 5 条语句，同样的数据从 50 s 降到秒级以下。
///
/// `cond` 是对 `photos p JOIN folders f` 的过滤条件，`params` 供其使用；
/// 调用方必须保证 `cond` 里不含外部输入拼接（本模块的三种调用都只传常量或整数 id）。
fn snapshot_scope(conn: &Connection, cond: &str, params: &[&dyn rusqlite::ToSql]) -> Result<()> {
    // ① 主表：以当前 live 状态为准（`INSERT OR REPLACE` 保证重复快照幂等）
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO photo_identities
                 (folder_path, name_key, starred, caption)
             SELECT f.path, p.name_key, COALESCE(p.starred, 0), COALESCE(c.text, '')
               FROM photos p
               JOIN folders f ON f.id = p.folder_id
               LEFT JOIN captions c ON c.photo_id = p.id
              WHERE {cond}"
        ),
        params,
    )?;
    // ② 标签：先把这批身份的旧骨架删掉，再按 live 重插（同样「以当前为准」）。
    //    删除必须在 `DELETE FROM photos` 之前做——那时还能 JOIN 回身份。
    for table in ["photo_identity_tags", "photo_identity_albums"] {
        conn.execute(
            &format!(
                "DELETE FROM {table}
                  WHERE EXISTS (
                        SELECT 1 FROM photos p
                          JOIN folders f ON f.id = p.folder_id
                         WHERE {cond}
                           AND f.path = {table}.folder_path
                           AND p.name_key = {table}.name_key)"
            ),
            params,
        )?;
    }
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO photo_identity_tags (folder_path, name_key, label)
             SELECT f.path, p.name_key, t.label
               FROM photos p
               JOIN folders f ON f.id = p.folder_id
               JOIN tags t ON t.photo_id = p.id
              WHERE {cond}"
        ),
        params,
    )?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO photo_identity_albums (folder_path, name_key, album_name)
             SELECT f.path, p.name_key, a.name
               FROM photos p
               JOIN folders f ON f.id = p.folder_id
               JOIN album_photos ap ON ap.photo_id = p.id
               JOIN albums a ON a.id = ap.album_id
              WHERE {cond}"
        ),
        params,
    )?;
    Ok(())
}

/// 快照**一个文件夹**（`folders.path`）下所有照片的标记。用于 `delete_folder`。
pub(crate) fn snapshot_for_folder(conn: &Connection, folder_path: &str) -> Result<()> {
    snapshot_scope(conn, "f.path = ?1", &[&folder_path])
}

/// 快照**一组文件夹 id** 下所有照片的标记。用于 `clear_index_for_root` 的整棵子树。
///
/// id 来自本库 `folders` 表（`SELECT id ...` 的 i64），不是外部字符串，
/// 直接内联进 `IN (...)` 是安全的——这样避免为上千个 id 构造上千个占位符。
pub(crate) fn snapshot_for_folder_ids(conn: &Connection, folder_ids: &[i64]) -> Result<()> {
    if folder_ids.is_empty() {
        return Ok(());
    }
    let list = folder_ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    snapshot_scope(conn, &format!("p.folder_id IN ({list})"), &[])
}

/// 快照**整库**所有照片的标记。用于 `clear_index`。
pub(crate) fn snapshot_all(conn: &Connection) -> Result<()> {
    snapshot_scope(conn, "1", &[])
}

/// 按身份把骨架表里的标记**恢复**到刚 INSERT 的新 `photo_id`。
///
/// 必须在 `INSERT INTO photos` 之后调用（此时有新 id）。只补齐，不覆盖用户在这期间
/// 对 live 行的新编辑：`captions`/`tags`/`album_photos` 全用 `INSERT OR IGNORE`，
/// `starred` 用 `MAX` 语义（骨架为 1 时置 1，否则不动）。相册按名字重建后再挂成员。
pub(crate) fn restore_photo(
    conn: &Connection,
    folder_path: &str,
    name_key: &str,
    photo_id: i64,
) -> Result<()> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT starred, caption FROM photo_identities
              WHERE folder_path = ?1 AND name_key = ?2",
            params![folder_path, name_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((starred, caption)) = row else {
        return Ok(());
    };

    if starred != 0 {
        conn.execute(
            "UPDATE photos SET starred = 1 WHERE id = ?1",
            params![photo_id],
        )?;
    }
    if !caption.is_empty() {
        conn.execute(
            "INSERT OR IGNORE INTO captions (photo_id, text) VALUES (?1, ?2)",
            params![photo_id, caption],
        )?;
    }
    conn.execute(
        "INSERT OR IGNORE INTO tags (photo_id, label)
         SELECT ?1, label FROM photo_identity_tags
          WHERE folder_path = ?2 AND name_key = ?3",
        params![photo_id, folder_path, name_key],
    )?;
    // 相册按名字：不存在则建（`created_at = 0` 与 V8 默认一致），再挂成员。
    let mut st = conn.prepare(
        "SELECT album_name FROM photo_identity_albums
          WHERE folder_path = ?1 AND name_key = ?2",
    )?;
    let albums: Vec<String> = st
        .query_map(params![folder_path, name_key], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(st);
    for name in albums {
        conn.execute(
            "INSERT OR IGNORE INTO albums (name, created_at) VALUES (?1, 0)",
            params![name],
        )?;
        let album_id: i64 = conn.query_row(
            "SELECT id FROM albums WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO album_photos (album_id, photo_id, added_at)
             VALUES (?1, ?2, 0)",
            params![album_id, photo_id],
        )?;
    }
    Ok(())
}

/// 把**所有**命中身份骨架的 live 照片逐张恢复标记（`INSERT` 之后整库补一遍）。
///
/// 适用场景：导入直接用 `copy_merged` 往 `photos` 里插行，**绕过了** `apply_scan`
/// 的逐条 `restore_photo`。若不在这里补一遍，用户在「移除导入的索引」之后再导入
/// 同一包，标记会留在骨架里挂不回新行——那是 R-M43-02 可见的丢标记。
///
/// 只处理「骨架里确有身份」的行（`JOIN photo_identities`），无标记的照片零开销。
pub(crate) fn restore_matching(conn: &Connection) -> Result<usize> {
    let mut st = conn.prepare(
        "SELECT p.id, f.path, p.name_key FROM photos p
           JOIN folders f ON f.id = p.folder_id
           JOIN photo_identities i
                ON i.folder_path = f.path AND i.name_key = p.name_key",
    )?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(st);
    let mut n = 0;
    for (id, folder_path, name_key) in rows {
        restore_photo(conn, &folder_path, &name_key, id)?;
        n += 1;
    }
    Ok(n)
}
