#!/usr/bin/env bash
# 把 release 二进制打成一个 .deb（Ubuntu / Debian，amd64 或本机架构）。
#
# 用法：
#   cargo build --release --bin OpenPicasa
#   packaging/make_deb.sh [输出目录]
#
# 产出：<输出目录>/openpicasa_<version>-1_<arch>.deb（默认输出到 ./dist/）
#
# 说明：
# - 这是 packaging/bundle.sh（macOS 的 .app）在 Linux 侧的对称物。
# - 图标优先用 packaging/logo.png（正式品牌图）经 make_icons.py 生成 hicolor
#   多尺寸 PNG；没有 logo.png 时退回 make_icon.py 的自绘图（跳过其末尾的
#   iconutil —— macOS 专有）。
# - 不依赖 cargo-deb / debhelper，只用 dpkg-deb + python3。
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-$repo/dist}"
bin="$repo/target/release/OpenPicasa"

# ---- 前置检查 ----
command -v dpkg-deb >/dev/null || { echo "缺少 dpkg-deb（属于 dpkg 包）" >&2; exit 1; }
command -v python3  >/dev/null || { echo "缺少 python3" >&2; exit 1; }
[[ -x "$bin" ]] || {
    echo "找不到 release 二进制：$bin" >&2
    echo "先编译：cargo build --release --bin OpenPicasa" >&2
    exit 1
}

# 版本号从 workspace 的 Cargo.toml 取，避免两处维护
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -1)"
[[ -n "$version" ]] || { echo "无法从 Cargo.toml 解析版本号" >&2; exit 1; }
arch="$(dpkg --print-architecture)"

mkdir -p "$out_dir"
# ⚠️ 临时目录**不能**用 /tmp：有些发行版（以及本仓库验证过的 Ubuntu 26.10）把 /tmp
# 挂成 10MB 的 tmpfs，20MB 的二进制光拷进去就 ENOSPC。放在输出目录旁边最稳。
stage="$(mktemp -d "$out_dir/.deb-stage.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
root="$stage/pkgroot"
doc="$root/usr/share/doc/openpicasa"

mkdir -p "$root/DEBIAN" "$root/usr/bin" "$root/usr/share/applications" "$doc"

# ---- 1. 二进制 ----
install -m 755 "$bin" "$root/usr/bin/openpicasa"

# ---- 2. 图标 ----
logo="$repo/packaging/logo.png"
if [[ -f "$logo" ]]; then
    # 正式品牌图：白底圆徽标 → 圆形透明遮罩 + 面积平均下采样
    python3 "$repo/packaging/make_icons.py" --logo "$logo" \
        --hicolor "$root/usr/share/icons/hicolor"
else
    # 兜底：make_icon.py 的 procedural 自绘图（仓库没放 logo.png 时）
    echo "未找到 packaging/logo.png，退回自绘图标" >&2
    python3 - "$repo" "$root" <<'PY'
import importlib.util, os, sys
repo, root = sys.argv[1], sys.argv[2]
spec = importlib.util.spec_from_file_location("make_icon", os.path.join(repo, "packaging", "make_icon.py"))
mi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mi)   # 只取定义，不跑 main()，所以不会调用 iconutil
for s in (16, 24, 32, 48, 64, 128, 256, 512):
    d = os.path.join(root, "usr/share/icons/hicolor", f"{s}x{s}", "apps")
    os.makedirs(d, exist_ok=True)
    mi.write_png(os.path.join(d, "openpicasa.png"), s, s, mi.render(s))
PY
fi
chmod 644 "$root"/usr/share/icons/hicolor/*/apps/openpicasa.png

# ---- 3. desktop 入口 ----
# 文件名必须是 app.openpicasa.desktop：GNOME 用 Wayland app_id
# （= picasa_core::paths::BUNDLE_ID）匹配 .desktop 文件名，对不上会出现两个图标。
cat > "$root/usr/share/applications/app.openpicasa.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Version=1.0
Name=OpenPicasa
GenericName=Photo Library
GenericName[zh_CN]=照片图库
Comment=Browse and organize a local photo library (open-source Picasa alternative)
Comment[zh_CN]=浏览与整理本地照片库，是 Picasa 的开源替代
Exec=openpicasa
TryExec=openpicasa
Icon=openpicasa
Terminal=false
StartupNotify=true
StartupWMClass=app.openpicasa
Categories=Graphics;Photography;Viewer;
Keywords=photo;picture;image;gallery;album;picasa;
Keywords[zh_CN]=照片;图片;图库;相册;管理;
EOF
chmod 644 "$root/usr/share/applications/app.openpicasa.desktop"

# ---- 4. copyright ----
cat > "$doc/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: OpenPicasa
Upstream-Contact: chongsenli <chongsenli@users.noreply.github.com>
Source: https://github.com/chongsenli/openpicasa

Files: *
Copyright: OpenPicasa contributors
License: MIT or Apache-2.0
 This project is dual-licensed under the MIT License and the Apache License,
 Version 2.0. You may use it under the terms of either license, at your option.
 .
 On Debian systems the full text of the Apache License 2.0 can be found in
 /usr/share/common-licenses/Apache-2.0 .
 .
 The MIT License text:
 .
 Permission is hereby granted, free of charge, to any person obtaining a copy
 of this software and associated documentation files (the "Software"), to deal
 in the Software without restriction, including without limitation the rights
 to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 copies of the Software, and to permit persons to whom the Software is
 furnished to do so, subject to the following conditions:
 .
 The above copyright notice and this permission notice shall be included in all
 copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 SOFTWARE.
 .
 Comment: 本项目与原版 Picasa / Google 无关；图标为自绘，未使用 Google 版权素材。
EOF
chmod 644 "$doc/copyright"

# ---- 5. changelog ----
cat > "$stage/changelog" <<EOF
openpicasa ($version-1) unstable; urgency=medium

  * Build for Linux from upstream main.
  * The binary links against the glibc symbol versions of the distribution it
    was built on — install it on the same release or newer (see Depends).

 -- chongsenli <chongsenli@users.noreply.github.com>  $(date -R)
EOF
gzip -9nc "$stage/changelog" > "$doc/changelog.Debian.gz"
chmod 644 "$doc/changelog.Debian.gz"

# ---- 6. 运行期依赖 ----
# 二进制的 ELF 依赖只有 libc/libgcc（GUI 库全是运行时 dlopen 的），
# 所以图形栈放进 Recommends：桌面环境本来就有，纯 CLI 装也不至于装不上。
max_glibc="$(objdump -T "$bin" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1 | sed 's/GLIBC_//')"
depends="libc6 (>= ${max_glibc:-2.39}), libgcc-s1"
installed_size="$(du -sk "$root/usr" | cut -f1)"

cat > "$root/DEBIAN/control" <<EOF
Package: openpicasa
Version: $version-1
Architecture: $arch
Maintainer: chongsenli <chongsenli@users.noreply.github.com>
Installed-Size: $installed_size
Section: graphics
Priority: optional
Homepage: https://github.com/chongsenli/openpicasa
Depends: $depends
Recommends: mesa-vulkan-drivers | libgl1, libwayland-client0, libxkbcommon0
Description: open-source photo library browser (Picasa alternative)
 OpenPicasa indexes a folder tree you point it at, decodes and orients
 thumbnails, and shows them in a virtualised grid with date grouping, a folder
 sidebar, a tray and a dark full-screen viewer. Indexing runs in the background
 and can be paused at a folder boundary or cleared without touching the photos;
 metadata lives in a local SQLite database; the photo directories are only ever
 read, never written to.
 .
 Current stage: M2.2 (index management, failure classification and a three-stage index pipeline).
EOF
chmod 644 "$root/DEBIAN/control"

# ---- 6.5 维护者脚本（图标缓存 / desktop 数据库）----
# 装完之后必须刷新这两个缓存，否则 GNOME / KDE 的应用列表里会出现
# 「入口在、图标是个问号」——文件其实已经装到 /usr/share 下了，只是索引没更新。
#
# ⚠️ 两个命令都不保证存在（最小化系统没有 gtk-update-icon-cache），用 command -v 兜住；
# ⚠️ 更关键的是**不能让它们的退出码决定安装成败**：缓存刷新失败只是图标显示问题，
#    让它把 dpkg 打断成 half-installed 是最坏的一种结果，所以末尾无条件 `exit 0`。
cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -q /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
cat > "$root/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -q /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
chmod 755 "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"

# ---- 7. 打包 ----
deb="$out_dir/openpicasa_${version}-1_${arch}.deb"
dpkg-deb --build --root-owner-group "$root" "$deb" >/dev/null
echo "✅ $deb"
echo "   版本 $version · 架构 $arch · 安装后 $(du -sh "$root/usr" | cut -f1) · 依赖 $depends"
