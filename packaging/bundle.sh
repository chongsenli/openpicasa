#!/bin/bash
# OpenPicasa · macOS 打包（02 §7.3 的「手写脚本」路线）
#
# 为什么不用 cargo-bundle：它表达不了 Info.plist 里我们需要的那几个键
# （TCC 的 UsageDescription 就是典型），还得再补一遍 PlistBuddy。
# 直接手写 30 行脚本，比「装一个工具 + 再打补丁」更可控。
#
# 用法：
#   packaging/bundle.sh            # release，产出 dist/OpenPicasa.app
#   packaging/bundle.sh --debug    # debug，加快验证循环
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

PROFILE="release"
CARGO_FLAGS="--release"
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="debug"
  CARGO_FLAGS=""
fi

VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
BUILD="$(date +%Y%m%d%H%M)"
APP_NAME="OpenPicasa"
DIST="$ROOT/dist"
APP="$DIST/$APP_NAME.app"

# 注意：变量后面紧跟全角标点时必须写 ${VAR}。
# bash 在这个 locale 下会把「多字节字符」当成变量名的一部分（$PROFILE）→ PROFILE<字节>），
# 于是 set -u 直接报 `PROFILE?: unbound variable`。ASCII 分隔符没这个问题。
echo "== 1/6 构建（${PROFILE}）=="
export PATH="$HOME/.cargo/bin:$PATH"
cargo build $CARGO_FLAGS --bin "$APP_NAME"

BIN="$ROOT/target/$PROFILE/$APP_NAME"
[[ -f "$BIN" ]] || { echo "❌ 找不到可执行文件 $BIN"; exit 1; }

echo "== 2/6 清空旧产物 =="
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

echo "== 3/6 拷贝可执行文件 =="
cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"
chmod +x "$APP/Contents/MacOS/$APP_NAME"

echo "== 4/6 生成图标 =="
if [[ ! -f "$DIST/$APP_NAME.icns" ]]; then
  mkdir -p "$DIST"
  python3 packaging/make_icon.py "$DIST"
fi
cp "$DIST/$APP_NAME.icns" "$APP/Contents/Resources/$APP_NAME.icns"

echo "== 5/6 写 Info.plist =="
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" \
  packaging/Info.plist > "$APP/Contents/Info.plist"
# 校验 plist 语法（写错的话 .app 会静默打不开，这是最常见的坑）
plutil -lint "$APP/Contents/Info.plist" > /dev/null

echo "== 6/6 ad-hoc 签名 =="
# M1 用 ad-hoc 签名（--sign -）：不需要开发者证书，但**换了机器/重新构建就会变**，
# 后果是 TCC 授权可能要求重新确认（02 §7.4 已登记这个代价）。
codesign --force --sign - --timestamp=none "$APP" 2>&1 | sed 's/^/    /'

echo
echo "== 校验 =="
codesign -dv "$APP" 2>&1 | sed 's/^/    /'
SIZE="$(du -sh "$APP" | cut -f1)"
echo "    体积：${SIZE}（M1-P11 目标 ≤ 30MB）"
echo "    产物：$APP"
echo
echo "打开：open \"$APP\""
