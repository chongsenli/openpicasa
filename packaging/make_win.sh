#!/usr/bin/env bash
# OpenPicasa · Windows 双架构安装包（在 macOS 上交叉编译，MinGW-w64 + NSIS）
#
# 用法：
#   packaging/make_win.sh              # 两个架构都打
#   packaging/make_win.sh x64          # 只打 x86_64
#   packaging/make_win.sh arm64        # 只打 aarch64
#   packaging/make_win.sh --check      # 只做 cargo check --target（不链接，快速验证 cfg 分叉）
#
# 产出（默认 dist/）：
#   OpenPicasa-0.4.1-Beta2-x64-installer.exe   NSIS 安装器
#   OpenPicasa-0.4.1-Beta2-arm64-installer.exe
#   OpenPicasa-0.4.1-Beta2-x64-portable.zip   便携包（解压即用）
#   OpenPicasa-0.4.1-Beta2-arm64-portable.zip
#   OpenPicasa-<arch>.exe                     裸二进制（供排查）
#
# 前置（brew install 一次）：
#   rustup target add x86_64-pc-windows-gnu aarch64-pc-windows-gnullvm
#   brew install mingw-w64 makensis
#
# 为什么是 -gnu / -gnullvm 而不是 -msvc：MSVC 目标需要 cl.exe + Windows SDK，
# docs/02 §7 已判定「必须在 Windows 机器/CI 上构建」；MinGW 路线用 brew 的
# 交叉 gcc 编 sqlite3.c 并链接，不需要 Windows SDK，能在 macOS 上完成。
# 注意 aarch64 没有 -gnu 目标，用 -gnullvm（LLVM lld）+ 同一套 mingw gcc。
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="$repo/dist"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -1)"
[[ -n "$version" ]] || { echo "无法解析版本号" >&2; exit 1; }

# ---- 工具链定位 ----
# brew 的 mingw-w64 bottle 里是分目录布局：
#   <prefix>/toolchain-x86_64/bin/x86_64-w64-mingw32-gcc
#   <prefix>/toolchain-i686/bin/i686-w64-mingw32-gcc
# ⚠️ 该 bottle **不含 aarch64** 工具链（只有 i686/x86_64），arm64 目标需要
#    另配 aarch64-w64-mingw32-*（或走 Windows 原生 runner）。
MINGW_ROOT=""
for p in /opt/homebrew/opt/mingw-w64 /usr/local/opt/mingw-w64 /opt/homebrew/Cellar/mingw-w64/*; do
  [[ -d "$p" ]] && MINGW_ROOT="$p" && break
done
[[ -n "$MINGW_ROOT" ]] || { echo "找不到 mingw-w64：brew install mingw-w64" >&2; exit 1; }

# 架构 → (rust target, gcc 前缀, 该架构在 bottle 里的 toolchain 子目录)
declare -A TOOLCHAIN_DIR=( [x64]="toolchain-x86_64" [arm64]="toolchain-aarch64" )
gcc_path() {
  local arch="$1" prefix="$2"
  local sub="${TOOLCHAIN_DIR[$arch]}"
  if [[ -x "$MINGW_ROOT/$sub/bin/${prefix}-gcc" ]]; then
    echo "$MINGW_ROOT/$sub/bin"
  elif [[ -x "$MINGW_ROOT/bin/${prefix}-gcc" ]]; then
    echo "$MINGW_ROOT/bin"
  else
    echo ""
  fi
}

check_only=0
want_x64=1
want_arm=1
for arg in "$@"; do
  case "$arg" in
    --check) check_only=1 ;;
    x64) want_arm=0 ;;
    arm64) want_x64=0 ;;
  esac
done

# 架构 → (rust target, mingw gcc 前缀, 显示名, PE 机型的简短说明)
declare -a TARGETS=()
[[ $want_x64 -eq 1 ]] && TARGETS+=("x64|x86_64-pc-windows-gnu|x86_64-w64-mingw32")
[[ $want_arm -eq 1 ]] && TARGETS+=("arm64|aarch64-pc-windows-gnullvm|aarch64-w64-mingw32")

mkdir -p "$out_dir"

# ---- ico + LICENSE（两个架构共用）----
if [[ $check_only -eq 0 ]]; then
  PY="$(command -v python3)"
  "$PY" "$repo/packaging/make_ico.py" "$out_dir/OpenPicasa.ico" >/dev/null
  cat > "$out_dir/LICENSE.txt" <<'EOF'
OpenPicasa is dual-licensed under either of

  * Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0)
  * MIT license (http://opensource.org/licenses/MIT)

at your option. See the repository for the full license texts and third-party
notices. This installer bundles no third-party binaries; all dependencies are
compiled into OpenPicasa.exe.
EOF
fi

for spec in "${TARGETS[@]}"; do
  IFS='|' read -r arch target gcc_prefix <<<"$spec"
  bindir="$(gcc_path "$arch" "$gcc_prefix")"
  [[ -n "$bindir" ]] || {
    echo "❌ [$arch] 缺 ${gcc_prefix}-gcc（已查 $MINGW_ROOT/toolchain-*/bin）" >&2
    echo "   brew 的 mingw-w64 bottle 只有 i686/x86_64；aarch64 需另配工具链或改用 Windows 原生 runner" >&2
    continue
  }
  gcc="$bindir/${gcc_prefix}-gcc"
  ar="$bindir/${gcc_prefix}-ar"

  echo "== [$arch] target=$target gcc=$gcc =="
  export "CC_${target//-/_}=$gcc"
  export "AR_${target//-/_}=$ar"
  # cc-rs / windows-sys 的 build.rs 还会按名字找 ${prefix}-dlltool / -ranlib，
  # 把整套工具目录放进 PATH（brew bottle 的 bin 不在默认 PATH 里）。
  export PATH="$bindir:$PATH"
  # libsqlite3-sys 用 cc::Build 编 sqlite3.c；链接由 rustc 自带 rust-lld 完成。
  linker_flags=""

  if [[ $check_only -eq 1 ]]; then
    cargo check --target "$target" --bin OpenPicasa --manifest-path "$repo/Cargo.toml" 2>&1 | tail -25
    continue
  fi

  echo "  → cargo build --release --target $target"
  cargo build --release --target "$target" --bin OpenPicasa --manifest-path "$repo/Cargo.toml" $linker_flags

  exe="$repo/target/$target/release/OpenPicasa.exe"
  [[ -f "$exe" ]] || { echo "❌ 未产出 $exe" >&2; exit 1; }

  # 裸二进制留一份便于排查
  cp "$exe" "$out_dir/OpenPicasa-${arch}.exe"

  # ---- 便携 zip ----
  zip="$out_dir/OpenPicasa-${version}-${arch}-portable.zip"
  rm -f "$zip"
  (cd "$(dirname "$exe")" && zip -q -j "$zip" OpenPicasa.exe)
  (cd "$out_dir" && zip -q "$zip" OpenPicasa.ico LICENSE.txt)
  echo "  → $zip"

  # ---- NSIS 安装器 ----
  setup="$out_dir/OpenPicasa-${version}-${arch}-installer.exe"
  rm -f "$setup"
  makensis -V2 \
    -DAPP_VERSION="$version" \
    -DARCH="$arch" \
    -DOUT_FILE="$setup" \
    -DBIN_FILE="$exe" \
    -DICO_FILE="$out_dir/OpenPicasa.ico" \
    -DLICENSE_FILE="$out_dir/LICENSE.txt" \
    "$repo/packaging/installer.nsi" 2>&1 | tail -12
  [[ -f "$setup" ]] || { echo "❌ NSIS 未产出 $setup" >&2; exit 1; }
  echo "  → $setup ($(du -sh "$setup" | cut -f1))"
done

echo
echo "完成。产物在 $out_dir"
