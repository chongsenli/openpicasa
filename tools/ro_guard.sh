#!/usr/bin/env bash
#
# R-1「照片目录零写入」的**系统调用级**守卫。
#
# 为什么不能用单元测试代替：
#   `crates/picasa-core/tests/zero_write.rs` 比的是文件哈希/mtime/大小——它能抓住
#   「写进去了」，抓不住「以写方式打开过但没写」和「写完又改回去了」。规格 §3.4 末段
#   把三段流水线 + DCT 缩放解码 + mmap 列为本版最大的新写入风险，评审 §1.2 明确要求
#   补 `fs_usage` 级证据。所以这里在系统调用层面抓。
#
# 三种后端，按可用性自动选：
#   ① strace   —— 最权威（能看到 open 的标志位）；但 ptrace 常被 YAMA/容器/沙箱禁掉
#   ② LD_PRELOAD —— 拦 libc（Rust 的 std::fs 走 libc，覆盖完整），**不需要任何特权**
#   ③ fs_usage —— macOS（需 sudo）
#
# 用法：
#   tools/ro_guard.sh [照片目录] [张数]
#
# 不传照片目录时，自动从真实库的 roots 表里挑一个**在线**的根。
#
# ⚠️ 三种后端都必须先证明「真的碰到了照片目录」——命中数为 0 就判**无效**（退出码 2），
#    因为"零违规"可能只是"压根没读到"，那属于假通过。
#
# 退出码：0 = 零写入通过；1 = 抓到写入（违反 R-1）；2 = 环境不满足/负载没造成。

set -uo pipefail

PHOTO_DIR="${1:-}"
LIMIT="${2:-300}"

# ⚠️ 这台开发机的 /tmp 是 10MB tmpfs，造库副本会撑爆。统一走大容量缓存目录。
WORK_BASE="${OPENPICASA_GUARD_TMP:-/home/chongsenli/.cache/tmp-test}"
SRC_DB="$HOME/Library/Application Support/app.openpicasa/library.sqlite"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRELOAD_SRC="$SCRIPT_DIR/ro_guard_preload.c"
# 被测程序。默认跑流水线（GUARD-001）；设 RO_GUARD_PROBE 可换别的探针
# （例如 GUARD-002 用 verify_probe 驱动启动校验）。
PROBE="${RO_GUARD_PROBE:-target/release/examples/ro_guard_probe}"

die() { echo "❌ $*" >&2; exit 2; }
info() { echo "── $*"; }

# ---------------------------------------------------------------- 前置检查
command -v cargo >/dev/null 2>&1 || die "cargo 不在 PATH（先 export PATH=\"\$HOME/.cargo/bin:\$PATH\"）"
[[ -f "$SRC_DB" ]] || die "找不到真实库：$SRC_DB"

if [[ -z "$PHOTO_DIR" ]]; then
  PHOTO_DIR="$(python3 - "$SRC_DB" <<'PY'
import sqlite3, sys
con = sqlite3.connect("file:%s?mode=ro" % sys.argv[1], uri=True)
for (p,) in con.execute("SELECT path FROM roots WHERE state = 0 ORDER BY id LIMIT 1"):
    print(p)
PY
)"
fi
[[ -n "$PHOTO_DIR" && -d "$PHOTO_DIR" ]] || die "照片目录不可用：$PHOTO_DIR"

mkdir -p "$WORK_BASE" || die "无法创建工作目录 $WORK_BASE"
WORK="$(mktemp -d "$WORK_BASE/ro_guard.XXXXXX")" || die "mktemp 失败"
trap 'rm -rf "$WORK"' EXIT

info "照片目录（被测，只读）：$PHOTO_DIR"
info "张数上限：$LIMIT"

# ---------------------------------------------------------------- 准备库副本
# 直接写真实库会污染用户的真实缩略图缓存，所以复制一份在容器目录里写。
info "复制库副本（不动原库）…"
cp "$SRC_DB" "$WORK/library.sqlite" || die "复制库失败"
DB_COPY="$WORK/library.sqlite"

# ⚠️ 构建与抓包可能不在同一个权限域：strace 需要 ptrace（沙箱里被拒），
#    而关闭沙箱后 cargo 又可能读不到 registry 缓存。默认构建，
#    设 `RO_GUARD_SKIP_BUILD=1` 可跳过（先单独构建，再跑本脚本抓包）。
if [[ "${RO_GUARD_SKIP_BUILD:-0}" == "1" ]]; then
  info "跳过构建（RO_GUARD_SKIP_BUILD=1）"
else
  info "构建 ro_guard_probe（release）…"
  cargo build --release --example ro_guard_probe -p picasa-core >/dev/null 2>&1 \
    || die "构建失败（可先单独构建后设 RO_GUARD_SKIP_BUILD=1 重跑）"
fi
[[ -x "$PROBE" ]] || die "探针二进制不存在：$PROBE（先构建，或检查工作目录）"

# ---------------------------------------------------------------- 抓包
TRACE="$WORK/trace.log"
VIOL="$WORK/violations.txt"
ALL="$WORK/access.log"
: >"$VIOL"
: >"$ALL"
backend=""
rc=0

case "$(uname -s)" in
  Linux)
    # ① 先探测 strace 到底能不能用（存在 ≠ 可用）
    if command -v strace >/dev/null 2>&1 \
       && strace -f -o /dev/null -e trace=openat /bin/true 2>/dev/null; then
      backend="strace"
      info "后端：strace（调用级）"
      strace -f -y -o "$TRACE" \
        -e trace=openat,openat2,open,creat,write,pwrite64,pwritev,unlink,unlinkat,rename,renameat,renameat2,mkdir,mkdirat,rmdir,truncate,ftruncate,chmod,fchmod,utimensat,utimes,setxattr,lsetxattr,removexattr,symlink,symlinkat,link,linkat,mknod,mknodat \
        "$PROBE" "$DB_COPY" "$PHOTO_DIR" "$LIMIT"
      rc=$?
    else
      # ② 降级到 LD_PRELOAD：不需要 ptrace
      command -v gcc >/dev/null 2>&1 || die "strace 不可用且没有 gcc，无法抓包"
      [[ -f "$PRELOAD_SRC" ]] || die "缺少 $PRELOAD_SRC"
      backend="ld_preload"
      info "后端：LD_PRELOAD 拦 libc（strace/ptrace 在本机不可用）"
      gcc -shared -fPIC -O2 -o "$WORK/guard.so" "$PRELOAD_SRC" -ldl || die "编译拦截器失败"
      RO_GUARD_DIR="$PHOTO_DIR" RO_GUARD_LOG="$VIOL" RO_GUARD_LOG_ALL="$ALL" \
        LD_PRELOAD="$WORK/guard.so" \
        "$PROBE" "$DB_COPY" "$PHOTO_DIR" "$LIMIT"
      rc=$?
    fi
    ;;
  Darwin)
    command -v fs_usage >/dev/null 2>&1 || die "macOS 分支需要 fs_usage"
    backend="fs_usage"
    info "后端：fs_usage（需 sudo，会提示输入密码）"
    sudo fs_usage -w -f filesys >"$TRACE" 2>&1 &
    FS_PID=$!
    sleep 1
    "$PROBE" "$DB_COPY" "$PHOTO_DIR" "$LIMIT"
    rc=$?
    sleep 1
    sudo kill "$FS_PID" 2>/dev/null
    wait "$FS_PID" 2>/dev/null
    ;;
  *) die "不支持的平台：$(uname -s)" ;;
esac

[[ $rc -eq 0 ]] || die "探针自身失败了（退出码 $rc）——负载没造成，GUARD 断言无意义"

# ---------------------------------------------------------------- 判定
info "分析（后端：$backend）…"

if [[ "$backend" == "ld_preload" ]]; then
  : # 违规与命中已由拦截器分别写进 $VIOL / $ALL
elif [[ "$backend" == "strace" ]]; then
  grep -F "$PHOTO_DIR" "$TRACE" >"$ALL" 2>/dev/null || true
  # ① 明确的写调用（unlink/rename/…）：出现即违规
  grep -E "^[[:space:]]*[0-9]+[[:space:]]+(write|pwrite64|pwritev|unlink|unlinkat|rename|renameat|renameat2|mkdir|mkdirat|rmdir|truncate|ftruncate|chmod|fchmod|utimensat|utimes|setxattr|lsetxattr|removexattr|symlink|symlinkat|link|linkat|mknod|mknodat|creat)\(" \
    "$ALL" >>"$VIOL" 2>/dev/null || true
  # ② open 类：带写标志才算（O_RDONLY 的正常读取要放行）
  grep -E "^[[:space:]]*[0-9]+[[:space:]]+(openat|openat2|open)\(" "$ALL" 2>/dev/null \
    | grep -E "O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND" >>"$VIOL" 2>/dev/null || true
else
  grep -F "$PHOTO_DIR" "$TRACE" >"$ALL" 2>/dev/null || true
  # fs_usage 输出：`<时间>  <操作>  <路径>  <耗时>  <进程>`
  grep -E "[[:space:]](WrData|WrMeta|write|unlink|rename|mkdir|rmdir|chmod|chown|utimes|setattr|setxattr|rmxattr|symlink|link|create)" \
    "$ALL" >>"$VIOL" 2>/dev/null || true
fi

n_all=$(wc -l <"$ALL" 2>/dev/null | tr -d ' ')
n_viol=$(wc -l <"$VIOL" 2>/dev/null | tr -d ' ')
: "${n_all:=0}"
: "${n_viol:=0}"
# GUARD-001 额外要求：`*.ini` **零 open**（Picasa 时代的目录配置文件的残留，
# 我们不该去碰——碰了就说明有人在读/写老 Picasa 的元数据）
n_ini=$(grep -ciE '\.ini([^a-z]|$)' "$ALL" 2>/dev/null || true)
: "${n_ini:=0}"

echo
echo "════════════════════════════════════════════"
echo " 后端                : $backend"
echo " 被测目录            : $PHOTO_DIR"
echo " 碰到该目录的次数    : $n_all"
echo " 其中写类调用        : $n_viol"
echo " 其中 *.ini          : $n_ini（GUARD-001 要求 0）"
echo "════════════════════════════════════════════"

if [[ "$n_all" -eq 0 ]]; then
  echo "❌ 判定：无效 —— 一次都没碰到照片目录，负载没造成（换个有照片的根再跑）"
  exit 2
fi

if [[ "$n_ini" -gt 0 ]]; then
  echo "❌ 判定：违反 GUARD-001 —— 打开过 *.ini（前 10 条）："
  grep -iE '\.ini([^a-z]|$)' "$ALL" | head -10
  exit 1
fi

if [[ "$n_viol" -gt 0 ]]; then
  echo "❌ 判定：违反 R-1 —— 抓到对照片目录的写调用，前 20 条："
  head -20 "$VIOL"
  exit 1
fi

echo "✅ 判定：通过 —— 碰到 $n_all 次全部是读，零写入（R-1 成立）"
exit 0
