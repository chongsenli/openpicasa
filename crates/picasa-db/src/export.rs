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
    ContentFlags, FolderTree, FolderTreeNode, FolderTreeRoot, ImportReport, ImportStrategy,
    IndexManifest, ManifestCounts, ManifestRoot, TreeSource,
};
use crate::pathutil::{base_name, compare_key, is_under, nfc};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// SQLite `application_id` 的 magic（"OPIX" 的 ASCII）。写进导出文件头，
/// 用于「这不是 OpenPicasa 索引文件」的快速判定（`docs/11` §2.2.2）。
const MAGIC: i64 = 0x4F50_4958;
/// 当前支持的索引文件格式版本。
///
/// - v1：单一 SQLite 快照 + 清单（roots / folders 两张表即真相）。
/// - v2（M4.3 IDX-20）：清单多一段显式 `folder_tree`，校验和覆盖它；
///   `min_reader_version` 仍是 `0.4.0`——v1 读者忽略不认识的 key，照常能打开。
pub const FORMAT_VERSION: i64 = 2;

/// 导出结果（`M4-IDX-25`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportReport {
    pub path: PathBuf,
    pub bytes: u64,
    pub counts: ManifestCounts,
    /// 是否附带缩略图（索引包）。
    pub with_thumbs: bool,
    /// 产物文件的 SHA-256（IDX-04 可显示/事后复核，区别于 FNV 清单校验和）。
    pub sha256: String,
    /// 导出时间戳（UTC 秒，等于清单 created_at）。
    pub created_at: i64,
    /// 生成时本机时区（IDX-04：记录版本/数量/时区）。
    pub timezone: String,
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
    /// IDX-20 ④：解析出的文件夹树（预览里展示层级与来源）。
    pub tree: FolderTree,
    /// 这棵树从哪来（清单自带 / 由 folders 重建 / 按照片路径推导）。
    /// `needs_notice()` 为真时 UI 必须显示「结构由照片路径推导」之类的提示。
    pub tree_source: TreeSource,
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
    manifest_checksum_with(m, true)
}

/// 校验和算法的**旧版**（v1 格式，`folder_tree` 尚未进清单）。
///
/// 反序列化一个 v1 包时 `folder_tree` 是 `None`，但**旧版算法根本没有把这一项
/// 拼进去**——多拼一个 `"null"|` 会让所有旧包都算不出原来的值。
/// 实测用户手里 0.4.1-Beta2 导出的 9 GiB 包正是这种，被误报「已损坏」。
fn manifest_checksum_legacy(m: &IndexManifest) -> String {
    manifest_checksum_with(m, false)
}

/// 判定一个包的 `payload_checksum` 是否自洽。
///
/// v2 起 `folder_tree` 进校验和（篡改即被拒）；v2 之前的包按旧算法校验，
/// 否则老包全部被当成坏包——这是真实的向前兼容要求，不是防御性代码。
fn manifest_checksum_matches(m: &IndexManifest) -> bool {
    if m.payload_checksum.is_empty() {
        return false;
    }
    if m.payload_checksum == manifest_checksum(m) {
        return true;
    }
    m.format_version < 2 && m.payload_checksum == manifest_checksum_legacy(m)
}

fn manifest_checksum_with(m: &IndexManifest, include_tree: bool) -> String {
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
    // IDX-20：folder_tree 进校验和，篡改即被拒。
    // `include_tree == false` 是 v1 格式：那时这一项还不存在，拼进去会改变摘要。
    if include_tree {
        s.push_str(&serde_json::to_string(&m.folder_tree).unwrap_or_default());
        s.push('|');
    }
    s.push_str(&m.notes);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// SHA-256（FIPS 180-4）自包含实现（IDX-04/05）。
///
/// 与 `manifest_checksum` 的 FNV-1a 用途不同：FNV 只校验清单字段有没有被篡改，
/// SHA-256 是**整个产物文件**的可复核摘要，给用户显示/事后核对。刻意不引
/// `sha2` 依赖——加入依赖会重写用户已修改的 `Cargo.lock`/`Cargo.toml`，红线不允许。
/// 已知向量单测见本文件 `#[cfg(test)] mod tests`。
///
/// 写成「可增量喂数据」的状态机而不是一次性吃整段切片，是因为真实索引包有
/// 9 GiB 量级：`std::fs::read` 整读会在 16 GiB 机器上换页、进程看起来像死了
/// （实机导入「无响应」就是这么来的）。
#[derive(Clone)]
struct Sha256 {
    h: [u32; 8],
    /// 已经喂进去、但还没凑满 64 字节块的尾巴。
    buf: Vec<u8>,
    /// 累计字节数（用于末尾的 bit length）。
    total: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            h: [
                0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a, 0x510e_527f, 0x9b05_688c,
                0x1f83_d9ab, 0x5be0_cd19,
            ],
            buf: Vec::with_capacity(64),
            total: 0,
        }
    }

    /// 喂一段数据；内部按 64 字节块压缩，未满一块的尾巴留到下次。
    #[allow(
        clippy::needless_range_loop,
        clippy::many_single_char_names,
        clippy::chunks_exact_to_as_chunks
    )]
    fn update(&mut self, mut data: &[u8]) {
        const K: [u32; 64] = [
            0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1,
            0x923f_82a4, 0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
            0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786,
            0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
            0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147,
            0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
            0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
            0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
            0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a,
            0x5b9c_ca4f, 0x682e_6ff3, 0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
            0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
        ];
        self.total += data.len() as u64;
        // 先把上次剩下的尾巴补齐。
        if !self.buf.is_empty() {
            let need = 64 - self.buf.len();
            let take = need.min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() < 64 {
                return;
            }
            let block = std::mem::take(&mut self.buf);
            self.compress(&block, &K);
        }
        let mut chunks = data.chunks_exact(64);
        for chunk in &mut chunks {
            self.compress(chunk, &K);
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            self.buf.extend_from_slice(rest);
        }
    }

    #[allow(
        clippy::needless_range_loop,
        clippy::many_single_char_names,
        clippy::chunks_exact_to_as_chunks
    )]
    fn compress(&mut self, block: &[u8], k: &[u32; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let h = &mut self.h;
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *hv = hv.wrapping_add(v);
        }
    }

    /// 收尾：补 0x80 + 零填充到 56 mod 64，再附 64 位大端 bit length。
    #[allow(clippy::needless_range_loop, clippy::chunks_exact_to_as_chunks)]
    fn finish(mut self) -> String {
        let bit_len = self.total.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.buf);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bit_len.to_be_bytes());
        // finish 阶段的块不需要增量状态，但复用 compress 必须给 K。
        const K: [u32; 64] = [
            0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1,
            0x923f_82a4, 0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
            0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786,
            0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
            0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147,
            0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
            0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
            0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
            0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a,
            0x5b9c_ca4f, 0x682e_6ff3, 0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
            0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
        ];
        let tail_len = tail.len();
        for chunk in tail.chunks_exact(64) {
            self.compress(chunk, &K);
        }
        debug_assert_eq!(tail_len % 64, 0);
        self.h.iter().map(|x| format!("{x:08x}")).collect()
    }
}

/// 一次性算一段内存的 SHA-256（小数据 / 单测用）。
#[cfg(test)]
fn sha256_hex(data: &[u8]) -> String {
    let mut s = Sha256::new();
    s.update(data);
    s.finish()
}

/// 流式计算文件的 SHA-256（`M4.3 IDX-18 / M42-PERF-01`）。
///
/// **不许用 `std::fs::read` 再喂给 `sha256_hex`**：用户真实索引包有 9.1 GB，
/// 整文件读进内存会在 16 GB 机器上直接换页、同时让进程看起来彻底没响应
/// （实测实机导入卡死就是这么来的）。这里固定 1 MiB 缓冲逐块喂，内存占用与
/// 文件大小无关。
///
/// 缓冲区**直接喂**、只留不足 64 字节的残块过界，不做「先拼到尾巴再 drain」
/// 那种每块一次 memmove 的写法——9 GiB 下那是白白多搬两遍数据。
///
/// `cancel` 每读一块检查一次：大包在校验阶段就能被「取消」打断，不必等它读完。
fn sha256_file_hex_cancellable(
    path: &Path,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String> {
    use std::io::Read;
    use std::sync::atomic::Ordering;

    /// 1 MiB：够大以摊薄 read 系统调用，够小以让取消的响应不迟于毫秒级。
    const BUF: usize = 1 << 20;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; BUF];
    // 用 `Sha256` 自己累计字节数，这里**不能再单独记 total**（会让 bit length 翻倍）。
    let mut hasher = Sha256::new();
    // 只保留「不足一个块」的残块，长度 < 64。
    let mut rem: Vec<u8> = Vec::with_capacity(64);
    loop {
        if let Some(c) = cancel
            && c.load(Ordering::Relaxed)
        {
            return Err(reject("已取消"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut data = &buf[..n];
        // 上次留下的残块先补满一块。
        if !rem.is_empty() {
            let need = 64 - rem.len();
            let take = need.min(data.len());
            rem.extend_from_slice(&data[..take]);
            data = &data[take..];
            if rem.len() < 64 {
                continue;
            }
            let block = std::mem::take(&mut rem);
            hasher.update(&block);
        }
        // 整块部分可以直接喂；`update` 内部自己按 64 切。
        let whole = data.len() / 64 * 64;
        if whole > 0 {
            hasher.update(&data[..whole]);
        }
        let rest = &data[whole..];
        if !rest.is_empty() {
            rem.extend_from_slice(rest);
        }
    }
    hasher.update(&rem);
    Ok(hasher.finish())
}

/// 产物/备份文件的 SHA-256（不可取消版；导出产物是本进程刚写出的文件，
/// 只需同步短摘要。大文件路径请用 `sha256_file_hex_cancellable`）。
fn sha256_file_hex(path: &Path) -> Result<String> {
    sha256_file_hex_cancellable(path, None)
}

/// 一次读盘同时完成「复制到 work_dir」与「算来源摘要」（`M4.3 PERF`）。
///
/// 原来这里是 `sha256_file_hex(src)` + `fs::copy(src, staging)` + 收尾再
/// `sha256_file_hex(src)` 复核 = **读三遍源文件**。9 GiB 上仅这一项就是两分钟
/// （实测摘要吞吐 ~193 MiB/s）。现在只读一遍：边读边写副本、边喂哈希。
///
/// 「来源在校验期间被修改」的检测改为**比对元信息**（长度 + 修改时间）：
/// 复制过程中真被改写时，这两个里至少有一个会变。为了不把 9 GiB 再读一遍，
/// 这里放弃「逐字节复核」而接受「元信息复核」——对这种「用户自己选文件、
/// 选完到校验完只隔几十秒」的场景，元信息已经足够，而且代价是零。
fn copy_and_hash_cancellable(
    src: &Path,
    dst: &Path,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String> {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering;

    const BUF: usize = 1 << 20;
    let before = std::fs::metadata(src)?;
    let mut input = std::fs::File::open(src)?;
    let mut out = std::fs::File::create(dst)?;
    let mut buf = vec![0u8; BUF];
    let mut hasher = Sha256::new();
    let mut rem: Vec<u8> = Vec::with_capacity(64);
    loop {
        if let Some(c) = cancel
            && c.load(Ordering::Relaxed)
        {
            drop(out);
            let _ = std::fs::remove_file(dst);
            return Err(reject("已取消"));
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        out.write_all(chunk)?;
        // 与 `sha256_file_hex_cancellable` 完全一致的喂法，保证同一份数据
        // 无论走哪条路都得到同一个摘要。
        let mut data = chunk;
        if !rem.is_empty() {
            let need = 64 - rem.len();
            let take = need.min(data.len());
            rem.extend_from_slice(&data[..take]);
            data = &data[take..];
            if rem.len() < 64 {
                continue;
            }
            let block = std::mem::take(&mut rem);
            hasher.update(&block);
        }
        let whole = data.len() / 64 * 64;
        if whole > 0 {
            hasher.update(&data[..whole]);
        }
        let rest = &data[whole..];
        if !rest.is_empty() {
            rem.extend_from_slice(rest);
        }
    }
    hasher.update(&rem);
    out.flush()?;
    // 副本必须完整：写入量与原文件长度一致（防止半截文件被当成合法包）。
    if out.metadata()?.len() != before.len() {
        let _ = std::fs::remove_file(dst);
        return Err(reject("复制索引文件时长度不一致，已中止导入"));
    }
    drop(out);
    let after = std::fs::metadata(src)?;
    if after.len() != before.len() || after.modified().ok() != before.modified().ok() {
        let _ = std::fs::remove_file(dst);
        return Err(reject("来源索引文件在校验期间被修改，已中止导入"));
    }
    Ok(hasher.finish())
}

/// 生成环境的时区标签（IDX-04：清单记录时区，帮助解读 UTC 秒时间戳）。
///
/// 刻意不引 chrono / tz 依赖（会重写用户已改的 Cargo.lock）：优先 `TZ` 环境变量，
/// 其次读 macOS/Linux 的 `/etc/localtime` 符号链接取 zoneinfo 名字（如
/// `Asia/Shanghai`），都拿不到时如实退回 `"UTC"`。
fn timezone_label() -> String {
    if let Ok(tz) = std::env::var("TZ")
        && !tz.is_empty()
    {
        return tz;
    }
    if let Ok(p) = std::fs::read_link("/etc/localtime") {
        let s = p.to_string_lossy();
        if let Some(pos) = s.rfind("zoneinfo/") {
            let tail = s[pos + "zoneinfo/".len()..].to_string();
            if !tail.is_empty() {
                return tail;
            }
        }
    }
    "UTC".to_string()
}

/// 两条路径是否指向同一个文件（canonicalize 优先，失败回退词法相等）。
/// 用于「来源不能覆盖当前库 / 目标不能覆盖当前库」(IDX-05)。
fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// `PRAGMA foreign_key_check` 是否零行（IDX-05 完整性门槛）。
/// 用 `query_row(...).optional()` 判断有没有第一行，比数 COUNT(*) 更省。
fn foreign_key_ok(conn: &Connection) -> Result<bool> {
    let mut st = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = st.query([])?;
    Ok(rows.next()?.is_none())
}

/// 索引包的必需表集合（IDX-02/05：结构完整性，不能只看 photos 一张表）。
const REQUIRED_TABLES: &[&str] = &[
    "photos", "folders", "roots", "exclusions", "thumbs", "captions", "tags", "albums",
    "album_photos", "index_tasks", "index_task_failures", "schema_migrations",
    "index_manifest", "settings", "recent_searches",
];

/// 测试专用：就地改写一个已导出索引包的清单并重算校验和。
///
/// 集成测试里要模拟「v1 老包缺失 `folder_tree`」「清单里树被篡改」这类场景，
/// 手搓 `manifest_checksum` 等于把生产逻辑抄一遍、抄错就测试假绿。这里直接复用
/// 生产的 `read_manifest` / `manifest_checksum` / `write_manifest`。
///
/// 不参与生产路径（`#[doc(hidden)]` + 只在 `cfg(test)` 之外的测试 crate 里调用）。
#[doc(hidden)]
pub fn rewrite_manifest_for_test(
    package: &Path,
    mutate: impl FnOnce(&mut IndexManifest),
) -> Result<()> {
    let conn = Connection::open(package)?;
    let mut m = read_manifest(&conn)?;
    mutate(&mut m);
    m.payload_checksum = manifest_checksum(&m);
    write_manifest(&conn, &m)?;
    Ok(())
}

/// 逐表检查存在性，返回第一个缺的表名（全部齐备返回 None）。
fn missing_required_table(conn: &Connection, tables: &[&str]) -> Result<Option<String>> {
    for t in tables {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![t],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(Some((*t).to_string()));
        }
    }
    Ok(None)
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
        // IDX-20：v1 包没有这一段 → None，导入端按 folders/photos 重建。
        folder_tree: get("folder_tree")?
            .and_then(|v| serde_json::from_str::<FolderTree>(&v).ok()),
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
        // IDX-20（format v2）：显式文件夹树。v1 读者不认识这个 key，直接忽略，
        // 所以 `min_reader_version` 不用动（P-M43-08①）。
        (
            "folder_tree",
            serde_json::to_string(&m.folder_tree).unwrap_or_default(),
        ),
        ("payload_checksum", m.payload_checksum.clone()),
        ("notes", m.notes.clone()),
        // IDX-04：清单额外记录生成环境的时区标签，帮助解读 created_at（UTC 秒）。
        // 作为未知 K/V key 写入，v1 读者忽略它，不改 IndexManifest 结构。
        ("timezone", timezone_label()),
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
    // IDX-05：目标不能覆盖当前运行库（含符号链接别名）。否则用户可能把
    // library.sqlite 本身当作导出目标，VACUUM INTO 后 rename 会毁掉现有索引。
    if let Some(cur) = db.path()
        && same_path(target, cur)
    {
        return Err(reject("导出目标不能是当前正在使用的索引库，请另选位置"));
    }
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
            // IDX-20：显式文件夹树。空文件夹也在里面（P-M43-08④）。
            folder_tree: Some(folder_tree_from_conn(&out)?),
            notes: String::new(),
            ..Default::default()
        };
        m.payload_checksum = manifest_checksum(&m);
        write_manifest(&out, &m)?;
        out.execute_batch("PRAGMA optimize; VACUUM;")?;
        drop(out);

        // 3) 自校验：重新打开、读回清单、比对校验和，确认文件不是半成品。
        //    IDX-04/05 补强：产物自身还要过 integrity_check / foreign_key_check，
        //    并把「清单计数」与「产物实际计数」逐项复核（不能用 live db 冒充产物）。
        let check = Connection::open(&tmp)?;
        let magic: i64 = check.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if magic != MAGIC {
            return Err(reject("导出文件自检失败：格式标识不匹配"));
        }
        let integrity: String = check
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error".into());
        if integrity != "ok" {
            return Err(reject("导出文件自检失败：SQLite 完整性检查未通过"));
        }
        if !foreign_key_ok(&check)? {
            return Err(reject("导出文件自检失败：外键关系不自洽"));
        }
        let back = read_manifest(&check)?;
        if !manifest_checksum_matches(&back) {
            return Err(reject("导出文件自检失败：清单校验和不匹配"));
        }
        let actual = counts_from_conn(&check, with_thumbs)?;
        if actual != back.counts {
            return Err(reject("导出文件自检失败：清单计数与产物实际内容不一致"));
        }
        // IDX-20 ③：显式树必须与包内 folders 表逐项一致（路径 / 父子 / 计数）。
        // 做法是拿产物自己的表再算一遍树，与清单里那段逐字段比对——不一致说明
        // 清单描述的不是这个库的树，导出失败、不发布半成品。
        let Some(tree) = &back.folder_tree else {
            return Err(reject("导出文件自检失败：缺少文件夹树"));
        };
        let recomputed = folder_tree_from_conn(&check)?;
        if *tree != recomputed {
            return Err(reject(
                "导出文件自检失败：清单里的文件夹树与包内容不一致",
            ));
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
    // IDX-04：报告的数量必须来自**产物文件**，而不是 live db（两者可能不一致）。
    let counts = {
        let c = Connection::open(target)?;
        counts_from_conn(&c, with_thumbs)?
    };
    let sha256 = sha256_file_hex(target)?;
    Ok(ExportReport {
        path: target.to_path_buf(),
        bytes,
        counts,
        with_thumbs,
        sha256,
        created_at: now,
        timezone: timezone_label(),
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

/// 导出机器的平台标识（IDX-20 ①：另记 `path_separator` 与平台）。
fn platform_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// 导出机器的主路径分隔符（Windows 反斜杠，其余斜杠）。
fn path_separator_label() -> &'static str {
    if cfg!(windows) { "\\" } else { "/" }
}

/// 相对所属根的展示路径（`/` 分隔，根自身为 `""`）。
fn rel_path_of(root_path: &str, path: &str) -> Option<String> {
    if path == root_path {
        return Some(String::new());
    }
    if !is_under(path, root_path) {
        return None;
    }
    let p = path.strip_prefix(root_path)?.trim_start_matches('/');
    Some(p.replace('\\', "/"))
}

/// 从连接里读出**显式的文件夹树**（IDX-20 ①）。
///
/// 口径：
/// - 每个根一行 `FolderTreeRoot`，`root_key` 用 `roots.path_key`（跨机器稳定，
///   `abs_path` 只作显示与重映射提示）；
/// - 每个文件夹一行 `FolderTreeNode`，`rel_path` 相对根（根自身 `""`），
///   `parent_rel` 是父文件夹的 `rel_path`；`depth` = `rel_path` 的段数；
/// - **空文件夹也在里面**（`direct_count = total_count = 0`），P-M43-08④；
/// - 只属最长前缀的那个根（多根嵌套时避免重复归属）。
pub(crate) fn folder_tree_from_conn(conn: &Connection) -> Result<FolderTree> {
    // (root_id, abs_path, path_key, case_insensitive, closed)
    let mut root_rows: Vec<(i64, String, String, bool, bool)> = {
        let mut st = conn.prepare(
            "SELECT id, path, path_key, case_insensitive, state FROM roots ORDER BY path",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
                r.get::<_, i64>(4)? != 0,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    // 最长前缀优先：`/photos/sub` 归 `/photos/sub` 而不是 `/photos`。
    root_rows.sort_by_key(|(_, p, ..)| std::cmp::Reverse(p.len()));

    // (id, path, name, parent_id, root_id, direct_count, total_count, is_online)
    type FolderRow = (
        i64,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        bool,
    );
    let folder_rows: Vec<FolderRow> = {
        let mut st = conn.prepare(
            "SELECT id, path, name, parent_id, root_id, direct_count, total_count, is_online
             FROM folders ORDER BY path",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)? != 0,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    // root_id → (root_path, root_key)
    let mut owner_of_root: std::collections::HashMap<i64, (String, String)> =
        std::collections::HashMap::new();
    for (id, path, key, ..) in &root_rows {
        owner_of_root.insert(*id, (path.clone(), key.clone()));
    }
    // folder_id → (root_key, rel_path)
    let mut placed: std::collections::HashMap<i64, (String, String)> =
        std::collections::HashMap::new();
    for (id, path, _, _, root_id, ..) in &folder_rows {
        // root_id 可能为 NULL（历史行）；退化为最长前缀匹配。
        let owner = root_id
            .and_then(|rid| owner_of_root.get(&rid).map(|(p, k)| (p.clone(), k.clone())))
            .or_else(|| {
                root_rows
                    .iter()
                    .find(|(_, rp, ..)| rel_path_of(rp, path).is_some())
                    .map(|(_, rp, rk, ..)| (rp.clone(), rk.clone()))
            });
        if let Some((rp, rk)) = owner
            && let Some(rel) = rel_path_of(&rp, path)
        {
            placed.insert(*id, (rk, rel));
        }
    }

    let mut folders: Vec<FolderTreeNode> = Vec::with_capacity(folder_rows.len());
    for (id, path, name, parent_id, _, direct, total, online) in &folder_rows {
        let Some((root_key, rel)) = placed.get(id) else {
            continue;
        };
        let parent_rel = parent_id.and_then(|pid| placed.get(&pid)).and_then(|(rk, pr)| {
            // 父节点必须同根；跨根（不该发生）时按 rel 截断兜底。
            if rk == root_key {
                Some(pr.clone())
            } else if rel.is_empty() {
                None
            } else {
                rel.rsplit_once('/').map(|(head, _)| head.to_string())
            }
        });
        let depth = if rel.is_empty() {
            0
        } else {
            rel.matches('/').count() as i64 + 1
        };
        folders.push(FolderTreeNode {
            root_key: root_key.clone(),
            rel_path: rel.clone(),
            parent_rel,
            depth,
            name: name
                .clone()
                .unwrap_or_else(|| {
                    if rel.is_empty() {
                        base_name(path).to_string()
                    } else {
                        rel.rsplit('/').next().unwrap_or("").to_string()
                    }
                }),
            direct_count: *direct,
            total_count: *total,
            is_online: *online,
        });
    }

    let mut roots: Vec<FolderTreeRoot> = Vec::with_capacity(root_rows.len());
    for (id, path, key, case_insensitive, closed) in &root_rows {
        let photo_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id
                 WHERE f.root_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let is_online = folders
            .iter()
            .filter(|n| &n.root_key == key)
            .all(|n| n.is_online);
        roots.push(FolderTreeRoot {
            root_key: key.clone(),
            abs_path: path.clone(),
            display_name: base_name(path).to_string(),
            path_key: key.clone(),
            case_insensitive: *case_insensitive,
            closed: *closed,
            is_online,
            photo_count,
        });
    }
    // 根的显示顺序按路径（与 roots_from_conn 一致），folders 按 root_key + 深度 + rel。
    roots.sort_by(|a, b| a.abs_path.cmp(&b.abs_path));
    folders.sort_by(|a, b| {
        a.root_key
            .cmp(&b.root_key)
            .then(a.depth.cmp(&b.depth))
            .then(a.rel_path.cmp(&b.rel_path))
    });

    Ok(FolderTree {
        path_separator: path_separator_label().to_string(),
        platform: platform_label().to_string(),
        roots,
        folders,
    })
}

/// 导入端解析「这棵树的真相」（IDX-20 ④）。
///
/// 三条路，优先级从高到低：
/// 1. 清单里有 `folder_tree` → 直接用（校验和已覆盖，前面 `stage_and_validate`
///    已经验过；这里再核对一次它和非空 folders 表不打架，不打架就信清单）。
/// 2. 没有树（v1 包 / 本产品原始库）→ 按包内 `folders` 表重建。
/// 3. `folders` 表缺失、或它的计数与 `photos` 对不上 → 按 `photos.folder_id →
///    文件夹路径` 推导，**计数按 photos 重算**，并标记「结构由照片路径推导」。
fn resolve_folder_tree(conn: &Connection, m: &IndexManifest) -> Result<(FolderTree, TreeSource)> {
    let has_folders = table_exists(conn, "folders")?;
    if let Some(tree) = &m.folder_tree {
        // 清单自带：只有在 folders 表缺失时才放弃它（否则以清单为准）。
        if has_folders || tree.folders.is_empty() {
            return Ok((tree.clone(), TreeSource::Manifest));
        }
        return Ok((tree.clone(), TreeSource::Manifest));
    }
    if has_folders {
        let tree = folder_tree_from_conn(conn)?;
        // 与 photos 交叉校验：每个文件夹的 direct_count 必须等于实际照片数。
        // 对不上说明 folders 表是坏的（历史行 / 被外部改过），退到路径推导。
        if tree_matches_photos(conn, &tree)? {
            return Ok((tree, TreeSource::DerivedFromFolders));
        }
        return Ok((derive_tree_from_photos(conn)?, TreeSource::DerivedFromPhotos));
    }
    Ok((derive_tree_from_photos(conn)?, TreeSource::DerivedFromPhotos))
}

/// 树里每个 `(root_key, rel_path)` 的 `direct_count` 是否等于该目录实际照片数。
fn tree_matches_photos(conn: &Connection, tree: &FolderTree) -> Result<bool> {
    for n in &tree.folders {
        let Some(abs) = join_root(&n.root_key, &n.rel_path, tree) else {
            return Ok(false);
        };
        let actual: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM photos p JOIN folders f ON f.id = p.folder_id
                 WHERE f.path = ?1",
                params![abs],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if actual != n.direct_count {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 由 `(root_key, rel_path)` 还原绝对路径：从树自带的根表里查，不查数据库
/// （推导路径下 folders 可能已经不可信）。
fn join_root(root_key: &str, rel_path: &str, tree: &FolderTree) -> Option<String> {
    let root = tree.roots.iter().find(|r| r.root_key == root_key)?;
    if rel_path.is_empty() {
        return Some(root.abs_path.clone());
    }
    Some(format!(
        "{}/{}",
        root.abs_path.trim_end_matches('/'),
        rel_path
    ))
}

/// 纯按 `photos` 推导树：`folders` 表不可信时的最后一条路。
///
/// 每个照片行带 `folder_id`，`folder_id → folders.path`；folders 缺失时无从得知，
/// 于是把每张照片的 `(folder_id)` 当作「一个目录」，用 `photos.folder_id` 分组，
/// 计数就是该组的照片数。根的归属靠 `roots.path` 的最长前缀。
fn derive_tree_from_photos(conn: &Connection) -> Result<FolderTree> {
    // 有 folders 表时按 path 分组，否则按 folder_id 分组（路径未知，只能用 id 兜底）。
    let has_folders = table_exists(conn, "folders")?;
    type Group = (String, i64); // (dir_key, count)
    let groups: Vec<Group> = if has_folders {
        let mut st = conn.prepare(
            "SELECT COALESCE(f.path, '#folder-' || p.folder_id), COUNT(*)
             FROM photos p LEFT JOIN folders f ON f.id = p.folder_id
             GROUP BY COALESCE(f.path, '#folder-' || p.folder_id) ORDER BY 1",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let mut st = conn.prepare(
            "SELECT '#folder-' || folder_id, COUNT(*) FROM photos
             GROUP BY folder_id ORDER BY 1",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let mut roots: Vec<FolderTreeRoot> = Vec::new();
    if table_exists(conn, "roots")? {
        let mut st = conn.prepare(
            "SELECT path, path_key, case_insensitive, state FROM roots ORDER BY path",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? != 0,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (path, key, ci, closed) = row?;
            let photo_count = groups
                .iter()
                .filter(|(dir, _)| rel_path_of(&path, dir).is_some())
                .map(|(_, c)| *c)
                .sum();
            roots.push(FolderTreeRoot {
                root_key: key.clone(),
                abs_path: path.clone(),
                display_name: base_name(&path).to_string(),
                path_key: key,
                case_insensitive: ci,
                closed,
                is_online: true,
                photo_count,
            });
        }
    }
    // 最长前缀优先归属。
    let mut sorted_roots = roots.clone();
    sorted_roots.sort_by_key(|r| std::cmp::Reverse(r.abs_path.len()));

    let mut folders: Vec<FolderTreeNode> = Vec::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for (dir, count) in &groups {
        let Some(root) = sorted_roots
            .iter()
            .find(|r| rel_path_of(&r.abs_path, dir).is_some())
        else {
            continue;
        };
        let Some(rel) = rel_path_of(&root.abs_path, dir) else {
            continue;
        };
        // 缺中间层时补出来（否则父链指向空节点）。
        let mut ancestors: Vec<String> = Vec::new();
        let mut cur = rel.clone();
        while let Some((head, _)) = cur.rsplit_once('/') {
            ancestors.push(head.to_string());
            cur = head.to_string();
        }
        ancestors.reverse();
        for a in ancestors {
            if seen.insert((root.root_key.clone(), a.clone())) {
                folders.push(FolderTreeNode {
                    root_key: root.root_key.clone(),
                    parent_rel: a.rsplit_once('/').map(|(h, _)| h.to_string()),
                    depth: a.matches('/').count() as i64 + 1,
                    name: a.rsplit('/').next().unwrap_or("").to_string(),
                    rel_path: a,
                    direct_count: 0,
                    total_count: 0,
                    is_online: true,
                });
            }
        }
        if seen.insert((root.root_key.clone(), rel.clone())) {
            folders.push(FolderTreeNode {
                root_key: root.root_key.clone(),
                parent_rel: if rel.is_empty() {
                    None
                } else {
                    rel.rsplit_once('/')
                        .map(|(h, _)| h.to_string())
                        .or(Some(String::new()))
                },
                depth: if rel.is_empty() {
                    0
                } else {
                    rel.matches('/').count() as i64 + 1
                },
                name: if rel.is_empty() {
                    root.display_name.clone()
                } else {
                    rel.rsplit('/').next().unwrap_or("").to_string()
                },
                rel_path: rel.clone(),
                direct_count: *count,
                total_count: *count,
                is_online: true,
            });
        }
    }
    // total_count 需要子树累加（推导路径下按 rel_path 前缀算）。
    for i in 0..folders.len() {
        let key = folders[i].root_key.clone();
        let rel = folders[i].rel_path.clone();
        let total: i64 = folders
            .iter()
            .filter(|n| n.root_key == key && (n.rel_path == rel || is_rel_under(&n.rel_path, &rel)))
            .map(|n| n.direct_count)
            .sum();
        folders[i].total_count = total;
    }
    folders.sort_by(|a, b| {
        a.root_key
            .cmp(&b.root_key)
            .then(a.depth.cmp(&b.depth))
            .then(a.rel_path.cmp(&b.rel_path))
    });

    Ok(FolderTree {
        path_separator: path_separator_label().to_string(),
        platform: "unknown".to_string(),
        roots,
        folders,
    })
}

/// `child` 是否在 `parent` 之下（两者都是 `/` 分隔的相对路径，`parent=""` 表示根）。
fn is_rel_under(child: &str, parent: &str) -> bool {
    if parent.is_empty() {
        return !child.is_empty();
    }
    child.len() > parent.len()
        && child.starts_with(parent)
        && child.as_bytes().get(parent.len()) == Some(&b'/')
}

/// 轻量 `sqlite_master` 存在性判断（IDX-20 ④：folders 表可能被人删掉）。
fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |_| Ok(true),
    )
    .optional()
    .map(|v| v.unwrap_or(false))
    .map_err(Into::into)
}

// ---------------------------------------------------------------- 校验 / 预览

/// 校验结果：staging 副本 + 清单 + 升级来源版本 + 源文件 SHA-256。
struct Staged {
    staging: PathBuf,
    manifest: IndexManifest,
    upgraded_from: Option<i64>,
    source_sha256: String,
    /// IDX-20 ④：这棵文件夹树的真相（可能是清单自带、由 folders 重建、或按照片路径推导）。
    tree: FolderTree,
    tree_source: TreeSource,
}

/// 打开一个索引文件做**全套校验**（`M4-IDX-11/14/30/51/52` + M4.3 IDX-02/05）。
/// 原文件全程只读不改，所有检查都在 `work_dir` 内的副本上进行。
///
/// 两条合法来源：
/// - **OPIX 索引包**：`application_id == MAGIC` + 清单 + FNV 校验和；低版本在副本上
///   按 MIGRATIONS 升级（不改原文件）。
/// - **干净关闭的本产品 `library.sqlite`**：没有 magic / 我们写的清单，但必需表齐、
///   `schema_migrations` 版本正好等于当前版本、integrity/foreign_key 全过 → 内存合成清单。
///
/// 两条路径共同门槛（IDX-02/05）：源文件前后 SHA-256 不变、无未合并 WAL/SHM、
/// 不是当前正在使用的库、SQLite 完整性 / 外键自洽、必需表齐全。
fn stage_and_validate(
    src: &Path,
    work_dir: &Path,
    current_db: Option<&Path>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Staged> {
    if !src.is_file() {
        return Err(reject("所选文件不存在或不是普通文件"));
    }
    // 活动库：来源不能就是当前运行库（含符号链接别名），必须先「导出索引」。
    if let Some(cur) = current_db
        && same_path(src, cur)
    {
        return Err(reject("不能导入当前正在使用的索引库，请先用「导出索引」生成文件"));
    }
    // 带未合并 WAL / SHM 的来源：还有活动连接或崩溃残留，副本可能不自洽。
    for suffix in ["-wal", "-shm"] {
        let mut name = src.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        let side = src.with_file_name(name);
        if std::fs::metadata(&side)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            return Err(reject(
                "来源索引还有未合并的 WAL（仍在运行或上次未正常关闭），请先导出索引文件后再导入",
            ));
        }
    }
    std::fs::create_dir_all(work_dir)?;
    let staging = work_dir.join(format!(
        "import-staging-{}-{}.db",
        std::process::id(),
        now_millis()
    ));
    // 大包（真实样本 9 GiB）：一次读盘同时算来源摘要 + 写副本，可取消、
    // 且不整读进内存。见 `copy_and_hash_cancellable` 的取舍说明。
    let source_sha256 = copy_and_hash_cancellable(src, &staging, cancel)?;

    let res = (|| -> Result<(IndexManifest, Option<i64>, FolderTree, TreeSource)> {
        let conn = Connection::open(&staging)?;
        // 1) SQLite 自身完整性（两条路径都要过）
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error".into());
        if integrity != "ok" {
            return Err(reject("索引文件已损坏或不完整（SQLite 完整性检查失败）"));
        }

        let magic: i64 = conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .unwrap_or(0);

        let (manifest, upgraded_from) = if magic != MAGIC {
            // ---- 第二路径：干净关闭的本产品 library.sqlite ----
            // 结构门槛：必需表齐、外键自洽、版本正好等于当前版本（活动库副本不跑迁移）。
            if let Some(t) = missing_required_table(&conn, REQUIRED_TABLES)? {
                return Err(reject(format!("索引文件结构不完整：缺少 {t} 表")));
            }
            if !foreign_key_ok(&conn)? {
                return Err(reject("索引文件外键关系不自洽，拒绝导入"));
            }
            let ver: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if ver != crate::schema::SCHEMA_VERSION {
                return Err(reject(format!(
                    "来源数据库为 v{ver}，当前需要 v{}，请用生成它的版本先导出索引文件",
                    crate::schema::SCHEMA_VERSION
                )));
            }
            let mut m = IndexManifest {
                format_version: FORMAT_VERSION,
                schema_version: ver,
                counts: counts_from_conn(&conn, true)?,
                source_roots: roots_from_conn(&conn)?,
                ..Default::default()
            };
            m.content_flags.thumbnails = true;
            // payload_checksum 留空：本路径没有 FNV 清单校验和，靠 SHA-256 + 结构校验保证。
            m.notes = "由干净关闭的本产品 library.sqlite 合成清单".to_string();
            (m, None)
        } else {
            // ---- 第一路径：OPIX 索引包 ----
            let m = read_manifest(&conn)?;
            if m.format_version == 0 {
                return Err(reject("不是 OpenPicasa 索引文件（缺少清单或格式版本为 0）"));
            }
            if !manifest_checksum_matches(&m) {
                return Err(reject("索引文件已损坏或不完整（清单校验和不匹配）"));
            }
            if m.format_version > FORMAT_VERSION || m.schema_version > crate::schema::SCHEMA_VERSION
            {
                return Err(reject(format!(
                    "该索引文件由更新版本的 OpenPicasa（{}，格式 v{}）生成，请升级应用后重试",
                    m.app_version, m.format_version
                )));
            }
            // 低版本：在 staging 副本上按 MIGRATIONS 顺序升级；原文件只读不改。
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
            // 结构校验放在升级**之后**：老版本包升级完才具备当前 schema 的全部表。
            if let Some(t) = missing_required_table(&conn, REQUIRED_TABLES)? {
                return Err(reject(format!("索引文件结构不完整：缺少 {t} 表")));
            }
            if !foreign_key_ok(&conn)? {
                return Err(reject("索引文件外键关系不自洽，拒绝导入"));
            }
            (m, upgraded_from)
        };
        // IDX-20 ④：解析「这棵树」的真相并记录来源。v1 包 / 干净原生 DB 没有
        // folder_tree 段，这里会自动退到 folders 表重建或照片路径推导。
        let (tree, tree_source) = resolve_folder_tree(&conn, &manifest)?;
        Ok((manifest, upgraded_from, tree, tree_source))
    })();

    // 「来源在校验期间被修改」已在 `copy_and_hash_cancellable` 里用元信息
    // （长度 + 修改时间）复核过；这里不再把 9 GiB 读第三遍。

    match res {
        Ok((manifest, upgraded_from, tree, tree_source)) => Ok(Staged {
            staging,
            manifest,
            upgraded_from,
            source_sha256,
            tree,
            tree_source,
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            Err(e)
        }
    }
}

/// 导入前预览（`M4-IDX-31/32`）：不修改任何数据，返回冲突与路径匹配信息。
///
/// 同步阻塞版：只适合小包 / 测试。界面里请走 `inspect_index_cancellable`——
/// 9 GiB 的包光校验就要一分钟，放在 UI 线程上就是「程序无响应」。
pub fn inspect_index(db: &Db, src: &Path, work_dir: &Path) -> Result<ImportPreview> {
    inspect_index_cancellable(db, src, work_dir, None)
}

/// 同 `inspect_index`，但支持取消（每读一块检查一次），供后台线程使用。
pub fn inspect_index_cancellable(
    db: &Db,
    src: &Path,
    work_dir: &Path,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ImportPreview> {
    let staged = stage_and_validate(src, work_dir, db.path(), cancel)?;
    let staging = staged.staging;
    let manifest = staged.manifest;
    let tree = staged.tree;
    let tree_source = staged.tree_source;
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
        tree,
        tree_source,
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

    let staged = stage_and_validate(src, work_dir, db.path(), Some(cancel))?;
    let staging = staged.staging;
    let manifest = staged.manifest;
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&staging);
        return Err(reject("导入已取消"));
    }

    // 替换策略：先在**事务之外**备份（`VACUUM INTO` 不能跑在事务内），
    // 备份失败就中止，现有索引一个字节不动。IDX-03：把备份路径与 SHA-256 带回报告，
    // 让用户能一键回退 / 事后复核，而不是「生成了但找不到」。
    let mut backup_path: Option<PathBuf> = None;
    let mut backup_sha256: Option<String> = None;
    if strategy == ImportStrategy::Replace {
        let backup = work_dir.join(format!("pre-import-{}.db", now_millis()));
        db.conn.execute(
            "VACUUM main INTO ?1",
            params![backup.to_string_lossy().as_ref()],
        )?;
        backup_sha256 = sha256_file_hex(&backup).ok();
        backup_path = Some(backup);
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
                upgraded_from: staged.upgraded_from,
                source_sha256: staged.source_sha256.clone(),
                backup_path: backup_path.clone(),
                backup_sha256: backup_sha256.clone(),
                ..Default::default()
            };

            if strategy == ImportStrategy::Replace {
                // 清掉「用户数据」与派生记录，再整表搬入；顺序满足外键依赖。
                // v9 身份骨架 / 导入来源表也一并清（否则 Replace 后残留旧标记、旧批次）。
                for t in [
                    "album_photos", "albums", "tags", "captions", "recent_searches",
                    "index_task_failures", "index_tasks", "thumbs", "photos", "folders",
                    "exclusions", "roots", "photo_identity_tags", "photo_identity_albums",
                    "photo_identities", "import_batch_members", "import_batches",
                    "import_sources",
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
            // 平移量：目标库各表现有最大 id（空表为 0）。
            let off = |tx: &rusqlite::Transaction<'_>, table: &str| -> Result<i64> {
                Ok(tx
                    .query_row(&format!("SELECT COALESCE(MAX(id),0) FROM main.{table}"), [], |r| {
                        r.get(0)
                    })
                    .unwrap_or(0))
            };
            // index_tasks 的主键列名是 task_id（不是 id）；用 MAX(id) 会恒为 0，
            // 合并时 task_id 冲突 → 任务行被 INSERT OR IGNORE 静默丢掉（IDX-03 洞）。
            let off_pk = |tx: &rusqlite::Transaction<'_>, table: &str, pk: &str| -> Result<i64> {
                Ok(tx
                    .query_row(
                        &format!("SELECT COALESCE(MAX({pk}),0) FROM main.{table}"),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0))
            };
            let o = MergeOffsets {
                root: if merge { off(&tx, "roots")? } else { 0 },
                folder: if merge { off(&tx, "folders")? } else { 0 },
                photo: if merge { off(&tx, "photos")? } else { 0 },
                album: if merge { off(&tx, "albums")? } else { 0 },
                task: if merge { off_pk(&tx, "index_tasks", "task_id")? } else { 0 },
                excl: if merge { off(&tx, "exclusions")? } else { 0 },
                search: if merge { off(&tx, "recent_searches")? } else { 0 },
                source: if merge { off(&tx, "import_sources")? } else { 0 },
                batch: if merge { off(&tx, "import_batches")? } else { 0 },
            };

            // 复制顺序 = 外键依赖顺序（父表先、子表后）。
            //
            // IDX-03「ID 映射覆盖全部关系 / 重复导入幂等」：旧版把每个外键都按
            // 线性 `src_id + off` 平移，一旦自然键冲突（同一个包导入两次、或两库
            // 含同一路径），父行被 `INSERT OR IGNORE` 跳过，子行 FK 却仍指向
            // `src_id + off` 这个不存在的 id → FOREIGN KEY constraint failed。
            // `copy_merged` 改为 `COALESCE(按自然键查主库真实 id, src_id + off)`，
            // 同时覆盖「主库已存在」与「刚按 off 插入」两种情形，重复导入时子行
            // 解析到既有父行、被自身主键/唯一键 IGNORE，整库幂等且不留半库。
            // 路径先统一到 NFC 再比对，否则 NFD 源（macOS 上日文/韩文/带重音的
            // 目录名都是 NFD）与 NFC 主库的同一个目录会被当成两个：`folders.path`
            // 有 UNIQUE，NFD 那行插得进去，于是同一张照片在左栏出现两次。
            // 实测一次真实导入产生 101 组重复目录、3470 张重复照片。
            // `copy_merged` 的 `merged_expr` 里 `m.path = s.path` 是 SQL 字节比较，
            // 做不了归一化——只能在这里先把 src 侧的 path / path_key 就地归一。
            normalize_src_paths(&tx)?;
            copy_merged(&tx, "roots", o.root, o)?;
            copy_merged(&tx, "folders", o.folder, o)?;
            copy_merged(&tx, "photos", o.photo, o)?;
            let after: i64 = tx.query_row("SELECT COUNT(*) FROM main.photos", [], |r| r.get(0))?;
            report.imported = after - before;
            report.skipped = manifest.counts.photos - report.imported;
            report.conflicts = ids_conflict;

            if cancel.load(Ordering::Relaxed) {
                return Err(reject("导入已取消"));
            }

            copy_merged(&tx, "captions", 0, o)?;
            copy_merged(&tx, "tags", 0, o)?;
            copy_merged(&tx, "albums", o.album, o)?;
            copy_merged(&tx, "album_photos", 0, o)?;
            copy_merged(&tx, "recent_searches", o.search, o)?;
            copy_merged(&tx, "exclusions", o.excl, o)?;
            copy_merged(&tx, "index_tasks", 0, o)?;
            copy_merged(&tx, "index_task_failures", 0, o)?;
            copy_merged(&tx, "thumbs", 0, o)?;
            // v9 身份骨架 / 导入来源：无 id 列的复合主键表原样搬，有 id 列的按 off 平移。
            copy_merged(&tx, "photo_identities", 0, o)?;
            copy_merged(&tx, "photo_identity_tags", 0, o)?;
            copy_merged(&tx, "photo_identity_albums", 0, o)?;
            copy_merged(&tx, "import_sources", o.source, o)?;
            copy_merged(&tx, "import_batches", o.batch, o)?;
            copy_merged(&tx, "import_batch_members", 0, o)?;
            // R-M43-02：导入走 `copy_merged` 直接往 photos 插行，绕过了
            // `apply_scan` 的逐条 `restore_photo`。这里对「骨架里已有身份」
            // 的行整库补一遍标记，保证「移除导入索引 → 重新导入」不会丢
            // 收藏 / 标签 / 说明 / 相册（此前只能靠重扫触发恢复）。
            if db.has_identity {
                let restored = crate::identity::restore_matching(&tx)?;
                if restored > 0 {
                    tracing::debug!(restored, "导入后按身份骨架恢复用户标记");
                }
            }

            // IDX-21 ①：把「本次导入带进来的根」与「本次真正落库的照片」记成一个
            // 来源 + 一个批次，供侧边栏「导入的索引」分组与「本次导入 · N 张」用。
            //
            // 判定口径：`copy_merged` 把主键平移了 `o.root` / `o.photo`，所以
            // 「id 大于平移量」的行就是这一次新插入的；自然键命中既有行被
            // `INSERT OR IGNORE` 跳过时 id 仍是旧值，不会被误算成新根——正好
            // 对应 P-M43-10「同路径本机已有，以本机为准」。
            let roots_added: Vec<i64> = {
                let mut st = tx.prepare("SELECT id FROM main.roots WHERE id > ?1 ORDER BY id")?;
                let rows = st.query_map(params![o.root], |r| r.get::<_, i64>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            let photo_ids: Vec<i64> = {
                let mut st = tx.prepare("SELECT id FROM main.photos WHERE id > ?1 ORDER BY id")?;
                let rows = st.query_map(params![o.photo], |r| r.get::<_, i64>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            let source_label = src
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "导入的索引".to_string());
            let strategy_label = match strategy {
                ImportStrategy::Merge => "merge",
                ImportStrategy::Replace => "replace",
            };
            // 走 `crate::unix_now_secs()`：`SystemTime` 被 `clippy.toml` 禁用，
            // 全 crate 只留那一处破例。
            let now = crate::unix_now_secs();
            let (source_id, batch_id) = crate::record_import(
                &tx,
                &source_label,
                Some(&src.to_string_lossy()),
                strategy_label,
                &format!("{source_label} · {}", report.imported),
                now,
                &roots_added,
                &photo_ids,
            )?;
            report.import_source_id = Some(source_id);
            report.import_batch_id = Some(batch_id);

            // IDX-20 ⑤：按清单的树把层级补齐（空文件夹、推导树都靠这一步落地），
            // 计数按目标库重算。只动索引库，不建目录、不碰原图。
            apply_folder_tree(&tx, &staged.tree)?;
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

/// 清理某次导出中断后留在**目标目录**的临时产物（M4.3 §3A ⑦「导出中断视为未完成，
/// 清理临时产物」）。
///
/// 导出先写 `target.opicasaidx.<pid>.tmp`、自检通过才 `rename` 成正式文件；进程在
/// 中途被杀就会留下这个 `.tmp`。它只可能出现在用户选定的导出目标旁边，**绝不触碰
/// 照片目录**——调用方传入的是日志里记录的导出目标路径。
///
/// 只删同一次导出留下的那些 `*.opicasaidx.*.tmp`（按 `<文件名>.opicasaidx.` 前缀匹配），
/// 不动目录里任何别的文件。返回删除个数。
pub fn cleanup_export_tmp(target: &Path) -> usize {
    let (Some(dir), Some(stem)) = (target.parent(), target.file_name()) else {
        return 0;
    };
    let prefix = format!("{}.opicasaidx.", stem.to_string_lossy());
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix)
                && name.ends_with(".tmp")
                && std::fs::remove_file(e.path()).is_ok()
            {
                n += 1;
            }
        }
    }
    n
}

/// 合并时各表的 id 平移量（Replace 策略全 0，因为主库已清空）。
#[derive(Clone, Copy)]
struct MergeOffsets {
    root: i64,
    folder: i64,
    photo: i64,
    album: i64,
    task: i64,
    excl: i64,
    search: i64,
    source: i64,
    batch: i64,
}

fn shifted(expr: &str, off: i64) -> String {
    if off != 0 {
        format!("{expr} + {off}")
    } else {
        expr.to_string()
    }
}

/// 某列在合并 `INSERT … SELECT` 中的表达式（源表别名固定 `t`）。
///
/// IDX-03「ID 映射覆盖全部关系 / 重复导入幂等」：
/// - 主键 `id`（或 `index_tasks.task_id`）按对应 off 平移；
/// - **有自然唯一键的外键**用 `COALESCE(按自然键在主库查到的真实 id, src_id + off)`：
///   重复导入时父行被 `INSERT OR IGNORE` 跳过，子行仍解析到既有父行，
///   不会指向「被跳过的 id」而触发 `FOREIGN KEY constraint failed`；
/// - 其余外键（无自然唯一键 / 逻辑外键）线性平移即可。
fn merged_expr(table: &str, c: &str, id_off: i64, o: MergeOffsets) -> String {
    let plain = format!("t.\"{c}\"");
    if c == "id" {
        return shifted(&plain, id_off);
    }
    if table == "index_tasks" && c == "task_id" {
        return shifted(&plain, o.task);
    }
    // (按自然键查主库既有行的子查询, 线性平移量)
    let natural: Option<(String, i64)> = match c {
        "root_id" => Some((
            "(SELECT m.id FROM main.roots m JOIN src.roots s ON s.id = t.\"root_id\" \
             WHERE m.path = s.path)"
                .into(),
            o.root,
        )),
        "parent_id" => Some((
            "(SELECT m.id FROM main.folders m JOIN src.folders s ON s.id = t.\"parent_id\" \
             WHERE m.path = s.path)"
                .into(),
            o.folder,
        )),
        "folder_id" if table == "photos" => Some((
            "(SELECT m.id FROM main.folders m JOIN src.folders s ON s.id = t.\"folder_id\" \
             WHERE m.path = s.path)"
                .into(),
            o.folder,
        )),
        "photo_id" if table != "photos" => Some((
            "(SELECT mp.id FROM main.photos mp JOIN main.folders mf ON mf.id = mp.folder_id \
             JOIN src.photos sp ON sp.id = t.\"photo_id\" \
             JOIN src.folders sf ON sf.id = sp.folder_id \
             WHERE mf.path = sf.path AND mp.name_key = sp.name_key)"
                .into(),
            o.photo,
        )),
        "album_id" => Some((
            "(SELECT m.id FROM main.albums m JOIN src.albums s ON s.id = t.\"album_id\" \
             WHERE m.name = s.name)"
                .into(),
            o.album,
        )),
        _ => None,
    };
    if let Some((sub, off)) = natural {
        return format!("COALESCE({sub}, {})", shifted(&plain, off));
    }
    // 无自然唯一键的外键：线性平移（NULL 参与加法仍是 NULL，安全）。
    let off = match c {
        "import_source_id" | "source_id" => o.source,
        "batch_id" => o.batch,
        "task_id" => o.task,
        "photo_id" => o.photo,
        _ => 0,
    };
    shifted(&plain, off)
}

/// 复制一张表到主库（`INSERT OR IGNORE`），按合并语义改写主键/外键列。
///
/// 用 `PRAGMA src.table_xinfo` 动态取普通列（跳过生成列），避免列漂移；
/// 源表不存在 / 没有普通列时安全返回 0（老版本包升级后表才存在）。
fn copy_merged(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    id_off: i64,
    o: MergeOffsets,
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
    let target = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let select = cols
        .iter()
        .map(|c| format!("{} AS \"{c}\"", merged_expr(table, c, id_off, o)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT OR IGNORE INTO main.\"{table}\" ({target}) \
         SELECT {select} FROM src.\"{table}\" t"
    );
    Ok(tx.execute(&sql, [])?)
}

/// 按 folder_tree 把 roots / folders 的层级**还原**到目标库（IDX-20 ⑤）。
///
/// 包里的 `folders` 表当然会整表搬进来，但有两种情况它不够：
/// - 空文件夹在旧包里可能没有行（v1 导出时只写了有照片的目录）；
/// - 结构是「由照片路径推导」的（folders 表缺失或与 photos 对不上）。
///
/// 所以这里以清单的树为准补一遍：根按 `path_key`（退化为 `abs_path`）对齐到
/// 已存在的 `roots` 行，文件夹按绝对路径 `INSERT OR IGNORE`，再回写
/// `root_id / parent_id / name / direct_count / total_count / is_online`。
/// **只动索引库，绝不建目录、绝不碰原图**（R-M43-01）。
fn apply_folder_tree(tx: &rusqlite::Transaction<'_>, tree: &FolderTree) -> Result<usize> {
    // 根：把树里的 root_key 映射到目标库真实 roots.id。
    // 合并策略下同路径复用（IDX-01），所以这里只查不插——根的存在由上层决定。
    let mut root_ids: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for r in &tree.roots {
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM roots WHERE path_key = ?1 OR path = ?2 ORDER BY id LIMIT 1",
                params![r.path_key, r.abs_path],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = id {
            root_ids.insert(r.root_key.clone(), id);
        }
    }

    let mut written = 0usize;
    for n in &tree.folders {
        let Some(root_id) = root_ids.get(&n.root_key).copied() else {
            continue; // 本机没有这个源根：树里这一支无处可挂，跳过（预览已提示）
        };
        // 清单里的路径也要归一成 NFC。只归一 `src.folders` 不够：这份树是
        // 从清单读进内存的，`copy_merged` 复制完之后由这里**再插一遍**，
        // NFD 的树路径会绕过归一化，又造出一行重复目录（实测复现）。
        let Some(abs) = join_root(&n.root_key, &n.rel_path, tree).map(|p| nfc(&p)) else {
            continue;
        };
        // 推导树的占位路径（`#folder-N`）不是真实路径，落库会污染 folders。
        if n.rel_path.starts_with('#') || !abs.starts_with('/') && !abs.contains(":\\") {
            continue;
        }
        let path_key = crate::pathutil::compare_key(&abs);
        tx.execute(
            "INSERT OR IGNORE INTO folders (path, path_key, is_online) VALUES (?1, ?2, 1)",
            params![abs, path_key],
        )?;
        // 根层文件夹的 `parent_id` 必须是 NULL。曾经这里写成「查自己」，
        // 于是 folders 出现自引用环，`total_count` 的递归 CTE（UNION ALL）
        // 永远爬不完 —— 导入直接卡死。这是 IDX-20 落地时踩到的坑。
        // 注意 `Some("")`：父就是根目录自己那一行（folders 表里根目录也有行），
        // 不能当成「没有父」。曾经把空串也归到 NULL，结果根层文件夹全部失去
        // 父链，`total_count` 递归聚合只看得到叶子 → 祖先计数全为 0。
        let parent_id: Option<i64> = match &n.parent_rel {
            Some(p) => {
                let Some(pabs) = join_root(&n.root_key, p, tree).map(|p| nfc(&p)) else {
                    continue;
                };
                tx.query_row(
                    "SELECT id FROM folders WHERE path = ?1",
                    params![pabs],
                    |row| row.get(0),
                )
                .optional()?
            }
            _ => None,
        };
        // 自引用会让 `total_count` 的递归 CTE 永远爬不完（曾导致导入卡死）。
        // 这里再挡一次：父 id 等于自己就当作没有父。
        let self_id: Option<i64> = tx
            .query_row("SELECT id FROM folders WHERE path = ?1", params![abs], |r| {
                r.get(0)
            })
            .optional()?;
        let parent_id = if parent_id == self_id { None } else { parent_id };
        tx.execute(
            "UPDATE folders SET root_id = ?2, parent_id = ?3, name = ?4,
                    is_online = ?5
              WHERE path = ?1",
            params![
                abs,
                root_id,
                parent_id,
                n.name,
                if n.is_online { 1 } else { 0 }
            ],
        )?;
        written += 1;
    }

    // 计数一律按目标库的物理事实重算，不信任包里带来的数（避免导入后自相矛盾）。
    tx.execute(
        "UPDATE folders SET direct_count =
             (SELECT COUNT(*) FROM photos p WHERE p.folder_id = folders.id)",
        [],
    )?;
    tx.execute(
        "UPDATE folders SET has_children =
             EXISTS (SELECT 1 FROM folders c WHERE c.parent_id = folders.id)",
        [],
    )?;
    tx.execute(
        "WITH RECURSIVE up(folder, amount) AS (
             SELECT id, direct_count FROM folders
              UNION ALL
             SELECT f.parent_id, up.amount
               FROM folders f JOIN up ON f.id = up.folder
              WHERE f.parent_id IS NOT NULL
         )
         UPDATE folders SET total_count = COALESCE(
             (SELECT SUM(amount) FROM up WHERE up.folder = folders.id), 0
         )",
        [],
    )?;

    // 导入进来的照片必须能被看到。
    //
    // 合并策略按路径复用既有根（P-M43-10「同路径以本机为准」），但那个根可能正
    // 处于「已关闭」或「索引已移除」态——比如源根 `/Volumes/Expansion` 在本机
    // 是关闭的外置盘根。照片照样挂进去了，可左栏与网格都把它当不可见范围，
    // 于是用户导入 26 万张后屏幕上一张也没有，观感与「导入失败」完全一样。
    //
    // 只恢复**本包的树里出现过、且确实有照片**的根：不碰包外的根（用户主动
    // 收起的其它盘不该因为一次导入被拉出来），也不碰包内有但没照片的根。
    //
    // ⚠️ 必须是「整棵子树」而不是「映射到的那一个根」：`derive_hierarchy` 按
    // **最长前缀**把每个 folder 归到最具体的根上，所以一张挂进 `/Volumes/Expansion`
    // 的照片，实际可能落在本机更具体的子根 `/Volumes/Expansion/🖼️ 图片` 名下。
    // 只恢复父根的话，这些照片仍在子根里、仍是不可见的（实测：修完父根后
    // 可见数只有 56549 / 265970）。
    let mut mapped: Vec<i64> = {
        let mut v: Vec<i64> = root_ids.values().copied().collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // 把映射到的根展开成「它自己 + 所有路径在它之下的根」。
    let mut keep_paths: Vec<String> = Vec::new();
    for id in &mapped {
        if let Ok(p) = tx.query_row("SELECT path FROM roots WHERE id = ?1", params![id], |r| {
            r.get::<_, String>(0)
        }) {
            keep_paths.push(p);
        }
    }
    for p in &keep_paths {
        let prefix = format!("{}/", p.trim_end_matches('/'));
        let mut st = tx.prepare("SELECT id FROM roots WHERE substr(path, 1, ?1) = ?2")?;
        let extra: Vec<i64> = st
            .query_map(params![prefix.len() as i64, prefix], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(st);
        mapped.extend(extra);
    }
    mapped.sort_unstable();
    mapped.dedup();

    let mut revived = 0usize;
    for id in &mapped {
        let n = tx.execute(
            "UPDATE roots SET state = 0, closed_at = NULL
              WHERE id = ?1 AND state <> 0
                AND EXISTS (SELECT 1 FROM folders f JOIN photos p ON p.folder_id = f.id
                             WHERE f.root_id = ?1)",
            params![id],
        )?;
        if n > 0 {
            tx.execute(
                "DELETE FROM settings WHERE key = ?1",
                params![format!("m43_index_removed_{id}")],
            )?;
            revived += 1;
        }
    }
    if revived > 0 {
        tracing::info!(roots = revived, "导入恢复了被关闭/已移除的根，照片可见");
    }
    Ok(written)
}

/// 把 `src` 库里的路径就地归一成 NFC，让 SQL 侧的 `m.path = s.path` 比对得上。
///
/// macOS 的 HFS/APFS 返回的目录名是 NFD（`つばさ舞` 的 `ば` = `は` + 浊点），
/// 本产品的 `folders.path` 一律存 NFC。导入包里如果留着 NFD，字符串比较就
/// 认为「这是另一个目录」——而 `folders.path` 的 UNIQUE 只在字节层生效，
/// NFD 那行照样插得进去。结果：同一张照片在左栏出现两次。
///
/// 只在 `src` 侧改写（内存里的 staging 文件），主库一行不动：
/// 后续 `copy_merged` 的 `m.path = s.path` 自然命中既有的 NFC 行，
/// 父行被 `INSERT OR IGNORE` 跳过、子行靠自然键映射挂到同一个 `folder_id` 上。
fn normalize_src_paths(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut updates: Vec<(String, String)> = Vec::new();
    {
        let mut st = tx.prepare("SELECT path FROM src.folders")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let path = row?;
            let normalized = nfc(&path);
            if normalized != path {
                updates.push((path, normalized));
            }
        }
    }
    for (old, new) in &updates {
        // 归一后与包里另一行撞车（同一个目录被存了两遍）→ 删掉这条冗余行，
        // 它的照片由 `folder_id` 自然键映射到保留下来的那一行。
        let taken: Option<i64> = tx
            .query_row(
                "SELECT id FROM src.folders WHERE path = ?1 LIMIT 1",
                params![new],
                |r| r.get(0),
            )
            .optional()?;
        if taken.is_some() {
            tx.execute("DELETE FROM src.folders WHERE path = ?1", params![old])?;
            continue;
        }
        tx.execute(
            "UPDATE src.folders SET path = ?2, path_key = ?3 WHERE path = ?1",
            params![old, new, compare_key(new)],
        )?;
    }
    // roots 同理：包的根路径也要归一，否则 `apply_folder_tree` 按 path 对不上。
    let mut root_updates: Vec<(String, String)> = Vec::new();
    {
        let mut st = tx.prepare("SELECT path FROM src.roots")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let path = row?;
            let normalized = nfc(&path);
            if normalized != path {
                root_updates.push((path, normalized));
            }
        }
    }
    for (old, new) in &root_updates {
        tx.execute(
            "UPDATE src.roots SET path = ?2, path_key = ?3 WHERE path = ?1",
            params![old, new, compare_key(new)],
        )?;
    }
    Ok(())
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
    // R-M43-01「照片目录零写入」：先判断目标是否落在任何已关注照片根之下，
    // **再做写探测**——旧版把写探测放在循环之前，若 dir 恰在照片根内就真的
    // 在照片目录里留下了一个临时文件（哪怕随后删掉，也已经写过一次）。
    // canonicalize 同时覆盖符号链接别名（/Volumes/... 与 /private/... 等）。
    let canon_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for r in roots {
        let root = Path::new(r);
        let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if canon_dir.starts_with(&canon_root) || dir.starts_with(root) {
            return Err(reject(format!(
                "导出文件不能放在图库照片目录内（{}），请另选位置",
                root.display()
            )));
        }
    }
    // 写权限探测：确认真的可写（只在通过照片根禁令的目录里进行）。
    let probe = dir.join(format!(".opicasa-write-probe-{}", std::process::id()));
    if std::fs::write(&probe, b"").is_err() {
        return Err(reject(format!("目标目录不可写：{}", dir.display())));
    }
    let _ = std::fs::remove_file(&probe);
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

    /// SHA-256 自包含实现必须用 FIPS 180-4 已知答案验证，避免把 FNV 误当 SHA-256
    /// 或 padding/轮常量写错（IDX-05 要求区别于 FNV 清单校验和）。
    #[test]
    fn sha256_hex_matches_fips_known_answers() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // 跨 64 字节块边界（1000 个 'a'，共 16 块）验证多块 + padding。
        assert_eq!(
            sha256_hex(&vec![b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
        assert_ne!(sha256_hex(b"abc"), manifest_checksum(&IndexManifest::default()));
    }

    /// 增量喂数据必须与一次性计算**逐字节等价**：任何「缓冲没对齐」的 bug
    /// 都会只在大文件上出现（小包测不出来），而用户拿的正是 9 GiB 的包。
    #[test]
    fn incremental_hashing_matches_one_shot_for_every_chunking() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 37 % 251) as u8).collect();
        let expected = sha256_hex(&data);
        // 一个字节一个字节地喂（最坏情况：每次都只有尾巴）。
        let mut one_byte = Sha256::new();
        for b in &data {
            one_byte.update(&[*b]);
        }
        assert_eq!(one_byte.finish(), expected, "逐字节喂结果必须一致");
        // 块大小跨 64 字节边界与 1 MiB 缓冲边界。
        for chunk in [1usize, 3, 63, 64, 65, 127, 128, 1024, 4096] {
            let mut h = Sha256::new();
            for part in data.chunks(chunk) {
                h.update(part);
            }
            assert_eq!(h.finish(), expected, "chunk={chunk} 结果必须一致");
        }
        // 空数据也不能例外。
        assert_eq!(
            Sha256::new().finish(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// 真实回归（用户 9 GiB 包）：v1 包的校验和**不含** `folder_tree`。
    ///
    /// 阶段 3B 把 `folder_tree` 拼进 FNV 之后，所有 v2 之前的包都被误报
    /// 「索引文件已损坏」。这条测试直接锁住「老包仍能被接受」，
    /// 并保证「v2 包用新算法、旧算法不再适用」不被回退。
    #[test]
    fn v1_packages_keep_validating_with_the_legacy_checksum() {
        let mut v1 = IndexManifest {
            format_version: 1,
            schema_version: 8,
            ..Default::default()
        };
        // 模拟旧版导出：只算 v1 算法，写进 payload_checksum。
        v1.payload_checksum = manifest_checksum_legacy(&v1);
        assert_ne!(
            v1.payload_checksum,
            manifest_checksum(&v1),
            "两种算法必须真的不同，否则这条测试没有意义"
        );
        assert!(
            manifest_checksum_matches(&v1),
            "v1 包必须按旧算法通过校验"
        );

        // v2 包：树段进校验和，旧算法不得再被接受。
        let mut v2 = IndexManifest {
            format_version: 2,
            ..v1.clone()
        };
        v2.payload_checksum = manifest_checksum_legacy(&v2);
        assert!(
            !manifest_checksum_matches(&v2),
            "v2 包用旧算法算出的摘要必须被拒（防止篡改绕过）"
        );
        v2.payload_checksum = manifest_checksum(&v2);
        assert!(manifest_checksum_matches(&v2));

        // 空摘要一律拒绝。
        let mut empty = v1.clone();
        empty.payload_checksum.clear();
        assert!(!manifest_checksum_matches(&empty));
    }

    /// 复制 + 哈希一趟完成：摘要必须与单独算文件摘要完全一致，
    /// 副本内容也必须逐字节相同（否则「校验过的包」≠「导入的包」）。
    #[test]
    fn copy_and_hash_matches_plain_file_hash() {
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::tempdir().unwrap();
        // 跨多个读取缓冲 + 末尾非整块。
        let data: Vec<u8> = (0..(2 * 1024 * 1024 + 37u32))
            .map(|i| (i % 251) as u8)
            .collect();
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, &data).unwrap();

        let dst = tmp.path().join("copy.bin");
        let digest = copy_and_hash_cancellable(&src, &dst, None).unwrap();
        assert_eq!(digest, sha256_hex(&data), "一趟摘要必须等于内存版");
        assert_eq!(
            digest,
            sha256_file_hex_cancellable(&src, None).unwrap(),
            "一趟摘要必须等于单独算文件摘要"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), data, "副本必须逐字节相同");

        // 已取消：既不产出摘要，也不留半截副本。
        let cancelled = AtomicBool::new(true);
        let dst2 = tmp.path().join("copy2.bin");
        let err = copy_and_hash_cancellable(&src, &dst2, Some(&cancelled))
            .unwrap_err()
            .to_string();
        assert!(err.contains("已取消"), "实际：{err}");
        assert!(!dst2.exists(), "取消后不得留下工作副本");
    }

    /// 真实的 9 GiB 级索引包：手工跑一遍**整条预览校验链路**的耗时（默认跳过）。
    ///
    /// `PERF_HASH_FILE=/path/to/pkg cargo test -p picasa-db --release --lib -- \
    ///   --ignored inspecting_a_real_package_stays_bounded --nocapture`
    #[test]
    #[ignore = "需要外部大文件 + 空闲磁盘，手工跑（PERF_HASH_FILE=...）"]
    fn inspecting_a_real_package_stays_bounded() {
        let Some(path) = std::env::var_os("PERF_HASH_FILE") else {
            return;
        };
        let src = std::path::PathBuf::from(path);
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(tmp.path().join("library.sqlite")).unwrap();
        let work = tmp.path().join("work");
        let t = std::time::Instant::now();
        let r = inspect_index(&db, &src, &work);
        let dt = t.elapsed();
        match r {
            Ok(p) => println!(
                "inspected in {:.2?}: photos={} roots={} tree_roots={}",
                dt,
                p.manifest.counts.photos,
                p.manifest.source_roots.len(),
                p.tree.roots.len()
            ),
            Err(e) => println!("inspect failed after {:.2?}: {e}", dt),
        }
        // 工作目录必须干净：校验完不许留 9 GiB 的副本。
        let leftovers: Vec<_> = std::fs::read_dir(&work)
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "工作目录残留：{leftovers:?}");
    }

    /// 真实的 9 GiB 级索引包：手工跑一遍量吞吐与内存（默认跳过）。
    ///
    /// `PERF_HASH_FILE=/path/to/pkg cargo test -p picasa-db --release --lib -- \
    ///   --ignored hashing_a_real_package_is_streaming_and_cancellable --nocapture`
    #[test]
    #[ignore = "需要外部大文件，手工跑（PERF_HASH_FILE=...）"]
    fn hashing_a_real_package_is_streaming_and_cancellable() {
        let Some(path) = std::env::var_os("PERF_HASH_FILE") else {
            return;
        };
        let path = std::path::PathBuf::from(path);
        let len = std::fs::metadata(&path).unwrap().len();
        let t = std::time::Instant::now();
        let digest = sha256_file_hex_cancellable(&path, None).unwrap();
        let dt = t.elapsed();
        let mb = len as f64 / (1024.0 * 1024.0);
        println!(
            "hashed {:.0} MiB in {:.2?} ({:.0} MiB/s) = {}",
            mb,
            dt,
            mb / dt.as_secs_f64(),
            digest
        );
        assert_eq!(digest.len(), 64);
    }

    /// 文件哈希必须走流式：同样的内容不论文件多大结果都得对。
    /// 另外验证取消能真的让哈希提前返回（否则 9 GiB 又得等满一分钟）。
    #[test]
    fn file_hashing_is_streaming_and_cancellable() {
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::tempdir().unwrap();
        // 3 MiB 出头：跨多个 1 MiB 读取缓冲，且尾部不是整块。
        let data: Vec<u8> = (0..(3 * 1024 * 1024 + 7u32))
            .map(|i| (i % 256) as u8)
            .collect();
        let path = tmp.path().join("blob.bin");
        std::fs::write(&path, &data).unwrap();

        assert_eq!(
            sha256_file_hex_cancellable(&path, None).unwrap(),
            sha256_hex(&data),
            "流式文件哈希必须与内存版一致"
        );

        // 已取消的 flag：一开始就返回「已取消」，不去读文件。
        let cancelled = AtomicBool::new(true);
        let err = sha256_file_hex_cancellable(&path, Some(&cancelled))
            .unwrap_err()
            .to_string();
        assert!(err.contains("已取消"), "取消应如实报告，实际：{err}");

        // 未取消的 flag 正常出结果。
        let live = AtomicBool::new(false);
        assert_eq!(
            sha256_file_hex_cancellable(&path, Some(&live)).unwrap(),
            sha256_hex(&data)
        );
    }
}
