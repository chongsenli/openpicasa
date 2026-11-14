#!/usr/bin/env bash
# 把 dist/OpenPicasa.app 打成一个可双击安装的 .dmg（macOS，arm64）。
#
# 用法：
#   packaging/bundle.sh                 # 先产出 dist/OpenPicasa.app
#   packaging/make_dmg.sh [输出目录]     # 默认输出到 ./dist/
#
# 产出：<输出目录>/OpenPicasa-<version>-<arch>.dmg
#
# 说明：
# - 这是 packaging/make_deb.sh（Linux）与 bundle.sh（.app）之后的第三段，
#   三者构成完整链路：cargo build → 组 .app → 装进 DMG。
# - 不用 dropdmg / create-dmg 这类工具：它们各自会往镜像里塞背景图、
#   .DS_Store 布局，产出不可复现，而这里只需要「拖进应用程序」这一个动作。
# - 卷标固定为 OpenPicasa：Finder 里显示的就是这个名字，与 .app 同名，
#   用户看到的「磁盘 → 应用程序」路径才不别扭。
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-$repo/dist}"

app="$repo/dist/OpenPicasa.app"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -1)"
[[ -n "$version" ]] || { echo "无法从 Cargo.toml 解析版本号" >&2; exit 1; }
arch="$(uname -m)"

# ---- 前置检查 ----
command -v hdiutil >/dev/null || { echo "缺少 hdiutil（macOS 自带）" >&2; exit 1; }
[[ -d "$app" ]] || {
    echo "找不到 $app" >&2
    echo "先打包：packaging/bundle.sh" >&2
    exit 1
}
# 没有签名的 .app 装进 DMG 后双击会被 Gatekeeper 拦在「已损坏」上，
# 那种提示比「未验证的开发者」更难解释，所以这里直接拦下。
codesign -dv "$app" >/dev/null 2>&1 || {
    echo "$app 未签名，先跑 packaging/bundle.sh" >&2
    exit 1
}

mkdir -p "$out_dir"
dmg="$out_dir/OpenPicasa-${version}-${arch}.dmg"

# ⚠️ 临时目录放在输出目录旁边而不是 /tmp：
#    这个仓库在 Linux 上已经因为「/tmp 是 10MB tmpfs」踩过一次 ENOSPC
#    （见 make_deb.sh 的注释）。macOS 的 /tmp 虽然通常是真盘，
#    但把中间产物和最终产物放在一起，失败时更容易看到残留。
stage="$(mktemp -d "$out_dir/.dmg-stage.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

echo "== 1/4 组装镜像内容 =="
# ditto 而不是 cp -R：前者保留符号链接、扩展属性与代码签名所需的元数据。
# 用 cp -R 拷过的 .app 经常在目标机上签名校验失败（"resource fork, Finder
# information, or similar detritus not allowed"）。
ditto "$app" "$stage/OpenPicasa.app"

# 卷图标：镜像里放一份 .VolumeIcon.icns，再给根目录打上「自定义图标」位。
# 少了这一步，挂载后 Finder 侧边栏/桌面显示的是系统默认的白磁盘，
# 看上去跟我们的应用没关系（.app 自己的图标不受影响，那是包内的 icns）。
iconsrc="$repo/dist/OpenPicasa.icns"
if [[ -f "$iconsrc" ]]; then
    cp "$iconsrc" "$stage/.VolumeIcon.icns"
    # -a C = 目录带自定义图标；-a V = 卷本身带自定义图标（第 3 步挂载后补）。
    SetFile -a C "$stage"
else
    echo "    ⚠️ 缺少 $iconsrc，跳过卷图标（先跑 packaging/bundle.sh）" >&2
fi

# 「拖进应用程序」的落点：一个指向 /Applications 的符号链接。
# Finder 会照原样显示成 Applications 文件夹图标，用户把左边的 app 拖到右边即可。
ln -s /Applications "$stage/Applications"
echo "    OpenPicasa.app + Applications → $stage"

echo "== 2/4 生成 DMG（UDZO 压缩）=="
rm -f "$dmg"
# -fs HFS+：APFS 镜像在老系统上不认，HFS+ 是覆盖面最广的读写文件系统。
# -format UDZO：只读 + zlib 压缩，是发布用镜像的标准格式。
#
# ⚠️ **这一行需要「允许格式化磁盘镜像」的权限**（hdiutil 会 newfs_hfs 一个
#    临时的 /dev/rdiskN）。在受限沙箱里它的报错是
#    `newfs_hfs: cannot create filesystem on /dev/rdisk4s1: Operation not permitted`
#    —— 看着像镜像写不进去，实际是权限，别去改 -fs / -format 试。
#    备选 `hdiutil makehybrid` 不需要这个权限，会成功，**但不能用**：
#    它给镜像里每个文件都打上 com.apple.FinderInfo 扩展属性，装到目标机后
#    `codesign --verify --deep --strict` 会报
#    「resource fork, Finder information, or similar detritus not allowed」，
#    即签名在解镜像那一刻就废了。宁可不产出 DMG，也不产出一个装上去打不开的。
#
# 为什么分两步（UDRW → 挂载打卷图标 → UDZO）：
#   卷图标有两个条件，缺一不可——目录里要有 .VolumeIcon.icns（第 1 步已放），
#   以及**卷头**的 kHasCustomIcon 位（`SetFile -a V`）。后者只能写在可写卷上，
#   而发布镜像必须只读，所以先造可写镜像、挂上、打位，再 `hdiutil convert` 压成 UDZO。
#   实测 convert 会保留该位（挂回只读镜像 `GetFileInfo -a` 仍是 `aV…`）。
#   早先直接 `-srcfolder -format UDZO` 出来的镜像没有这个位，Finder 里显示白磁盘。
rw="$(mktemp -d "$out_dir/.dmg-rw.XXXXXX")/rw.dmg"
mnt="$(dirname "$rw")/mnt"
cleanup_img() {
    [[ -d "$mnt" ]] && hdiutil detach "$mnt" >/dev/null 2>&1 || true
    rm -rf "$(dirname "$rw")"
}
trap 'cleanup_img; rm -rf "$stage"' EXIT

if ! hdiutil create \
        -volname "OpenPicasa" \
        -srcfolder "$stage" \
        -fs HFS+ \
        -format UDRW \
        -ov \
        "$rw" 2>&1 | sed 's/^/    /'; then
    echo
    echo "❌ DMG 生成失败。若上面出现 newfs_hfs / Operation not permitted，" >&2
    echo "   说明当前环境不允许格式化磁盘镜像——请在正常终端里重跑本脚本。" >&2
    echo "   已产出的 dist/OpenPicasa.app 本身是完整可用的，可以直接拖进应用程序。" >&2
    exit 1
fi

# 打卷图标位。SetFile 缺失时只降级为「没有卷图标」，不让打包失败——
# 图标是门面，能装才是根本。
if [[ -f "$stage/.VolumeIcon.icns" ]] && command -v SetFile >/dev/null; then
    mkdir -p "$mnt"
    if hdiutil attach "$rw" -nobrowse -mountpoint "$mnt" >/dev/null 2>&1; then
        SetFile -a C "$mnt" 2>/dev/null || true
        SetFile -a V "$mnt" 2>/dev/null || true
        sync
        hdiutil detach "$mnt" >/dev/null 2>&1 || true
        echo "    卷图标已写入（.VolumeIcon.icns + kHasCustomIcon）"
    else
        echo "    ⚠️ 可写镜像挂不上，跳过卷图标" >&2
    fi
fi

if ! hdiutil convert "$rw" -format UDZO -o "$dmg" 2>&1 | sed 's/^/    /'; then
    echo "❌ 压缩成只读镜像失败" >&2
    exit 1
fi
[[ -f "$dmg" ]] || { echo "❌ hdiutil 未产出 $dmg" >&2; exit 1; }

echo "== 3/4 挂载自检 =="
# 只验证「挂得上、里面东西齐 + 签名没被镜像过程弄坏」——装不装得进去还取决于
# Gatekeeper（那是签名身份的问题，不是镜像的问题，见 README 的首次打开说明）。
mkdir -p "$stage/mnt"
if ! hdiutil attach "$dmg" -nobrowse -readonly -mountpoint "$stage/mnt" >/dev/null 2>&1; then
    echo "    ❌ 镜像挂不上" >&2
    exit 1
fi
ok=1
[[ -d "$stage/mnt/OpenPicasa.app" ]] || { echo "    ❌ 缺 OpenPicasa.app" >&2; ok=0; }
[[ -L "$stage/mnt/Applications" ]] || { echo "    ❌ 缺 Applications 链接" >&2; ok=0; }
# 这条是重点：只有从**挂载后的镜像里**验过签名，才能证明「装到目标机上是好的」。
# 直接验 dist/ 里那份没有意义——镜像过程本身才是可能弄坏签名的那一步。
if ! codesign --verify --deep --strict "$stage/mnt/OpenPicasa.app" 2>&1 | sed 's/^/    /'; then
    echo "    ❌ 镜像内的 .app 签名校验失败（多半是扩展属性污染）" >&2
    ok=0
fi
hdiutil detach "$stage/mnt" >/dev/null 2>&1 || true
[[ "$ok" == 1 ]] || exit 1
echo "    ✅ 挂载正常、内含 .app 与 Applications 链接、签名严格校验通过"

echo "== 4/4 校验 =="
hdiutil verify "$dmg" 2>&1 | tail -2 | sed 's/^/    /'
echo
echo "    版本：${version}（${arch}）"
echo "    体积：$(du -sh "$dmg" | cut -f1)（.app 本身 $(du -sh "$app" | cut -f1)）"
echo "    产物：$dmg"
echo
echo "安装：双击 $dmg → 把 OpenPicasa 拖进 Applications"
echo "⚠️ 首次打开若提示「无法验证开发者」：在应用程序里右键 → 打开（只需一次，见 README）"
