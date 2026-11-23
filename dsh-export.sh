#!/bin/bash
# DSH 数据导出（纯文件夹拷贝，不使用 dsh-sync）
#
# 用法:
#   ./dsh-export.sh                      # 导出到 ~/Desktop/dsh-migration-<时间戳>/
#   ./dsh-export.sh /Volumes/U盘/dsh-bak  # 导出到指定目录
#   ./dsh-export.sh /Volumes/U盘/dsh-bak win-chong   # 额外为 Windows 用户 chong 改写会话日志
#
# 注意: 请先完全退出 DeepSeek Harness（菜单退出，不是关窗口）。
#       *.lock / .DS_Store / .bak-* 会被自动跳过。
#
# 第二参数 = 目标平台预设：
#   win-chong  : Windows / 用户名 chong → C:\Users\chong\Documents\<项目>
#   linux      : Debian / 用户名沿用当前 → /home/<user>/Documents/<项目>
#   留空      : 不改写日志，仅原样导出（配套 dsh-import.sh / .ps1 自行处理）
set -euo pipefail

DSH="$HOME/.dsh"
DEST="${1:-$HOME/Desktop/dsh-migration-$(date +%Y%m%d-%H%M%S)}"
DEST="${DEST%/}"
TARGET_PRESET="${2:-}"

RSYNC=(rsync -aL --exclude '.DS_Store' --exclude '*.lock' --exclude '.bak-*')

echo "== DSH 导出 =="
echo "源:   $DSH"
echo "目标: $DEST"
echo

#  softly 检查 DSH 是否仍在运行（不同系统 ps 行为不一致，仅提示不阻断）
if pgrep -f "dsh-desktop-host" >/dev/null 2>&1; then
  echo "⚠️  检测到 DSH 似乎仍在运行。建议先完全退出，避免拷到写了一半的会话文件。"
  echo "    继续请按 y，其它键取消："
  read -r ans
  [ "$ans" = "y" ] || { echo "已取消"; exit 1; }
fi

[ -d "$DSH" ] || { echo "找不到 $DSH"; exit 1; }
mkdir -p "$DEST"

# ---------- 1. 核心数据 ----------
# 会话日志（*.jsonl.zstd）、工作区状态、技能、附件等
for item in sessions storages skills skill-manager attachments task-board \
            dsh-claude-style workbuddy-vault dsh-market llm-deepseek; do
  if [ -d "$DSH/$item" ]; then
    echo "-- .dsh/$item"
    mkdir -p "$DEST/.dsh/$item"
    "${RSYNC[@]}" "$DSH/$item/" "$DEST/.dsh/$item/"
  fi
done

# ---------- 2. 配置 + 插件清单 ----------
# 关键：DSH 的"设置"其实分散在 settings.yaml.imported、
#       profiles/desktop/cordis.patch.yml、package.json 里，别只拷一个。
mkdir -p "$DEST/.dsh/profiles/desktop" "$DEST/.dsh/profiles/web"

echo "-- .dsh 顶层配置文件"
for f in settings.yaml.imported .anonymous-user-id .buddy-checkin.json \
         .workbuddy-ai-catalog.json .workbuddy-catalog.json \
         .workbuddy-app-version.json dsh-skill-picker-state.json; do
  [ -f "$DSH/$f" ] && cp -p "$DSH/$f" "$DEST/.dsh/$f"
done

# .credentials.yaml 含密钥，单独放并提示
if [ -f "$DSH/.credentials.yaml" ]; then
  cp -p "$DSH/.credentials.yaml" "$DEST/.dsh/.credentials.yaml"
  echo "   ⚠️  已包含 .credentials.yaml（内含凭据，请妥善保管导出目录）"
fi

echo "-- profiles/desktop（排除 node_modules 和日志）"
for f in cordis.yml cordis.patch.yml package.json pnpm-lock.yaml pnpm-workspace.yaml; do
  [ -f "$DSH/profiles/desktop/$f" ] && cp -p "$DSH/profiles/desktop/$f" "$DEST/.dsh/profiles/desktop/$f"
done
for d in .plugin-manager .dsh-market; do
  if [ -d "$DSH/profiles/desktop/$d" ]; then
    mkdir -p "$DEST/.dsh/profiles/desktop/$d"
    rsync -aL --exclude 'logs/' --exclude '.DS_Store' \
      "$DSH/profiles/desktop/$d/" "$DEST/.dsh/profiles/desktop/$d/"
  fi
done

echo "-- profiles/web（排除 node_modules）"
for f in cordis.yml cordis.patch.yml package.json pnpm-lock.yaml pnpm-workspace.yaml; do
  [ -f "$DSH/profiles/web/$f" ] && cp -p "$DSH/profiles/web/$f" "$DEST/.dsh/profiles/web/$f"
done

# ---------- 3. 散落在 HOME 其他位置的技能 / 记忆 ----------
mkdir -p "$DEST/home"

# ~/.agents/skills 里有指向 ~/.workbuddy/skills 的软链，-aL 会解引用成实体目录
for item in .agents/skills .workbuddy/skills .mnemon .openviking \
            .config/billion-context .local/share/billion-context; do
  if [ -e "$HOME/$item" ]; then
    echo "-- ~/$item"
    mkdir -p "$DEST/home/$item"
    "${RSYNC[@]}" "$HOME/$item/" "$DEST/home/$item/"
  fi
done

# ---------- 4. 本地 link: 插件 ----------
# package.json 里 dsh-full-width 是 link:/Users/xxx/Documents/... 的本地插件，
# 不拷过去的话新机器上会是一个断链。
LINKED="$HOME/Documents/deepseek workspace/dsh-full-width"
if [ -d "$LINKED" ]; then
  echo "-- 本地插件 dsh-full-width"
  mkdir -p "$DEST/extras/deepseek workspace"
  "${RSYNC[@]}" "$LINKED/" "$DEST/extras/deepseek workspace/dsh-full-width/"
fi

# 你自写的备份辅助脚本（可选，与 dsh-sync 插件无关）
if [ -d "$HOME/Library/Application Support/dsh-sync" ]; then
  mkdir -p "$DEST/extras/Application Support"
  "${RSYNC[@]}" "$HOME/Library/Application Support/dsh-sync/" \
    "$DEST/extras/Application Support/dsh-sync/"
fi

# ---------- 5. 跨平台改写（Python 驱动，统一处理日志/索引/目录名） ----------
# DSH 的 session/resume 会用 sameDirectory(header.cwd, params.cwd) 校验，
# 且按 cwd 重算 projectDir() 定位日志文件。日志头里的 Mac 路径若不改，
# 会话既列不出（win32 下 isAbsolute('/Users/...') == false）也恢复不了。
# 日志是 zstd 压缩 JSONL，Windows 默认没有 zstd，所以在 Mac 导出端一次性
# 改写最稳妥。改写用正斜杠 Windows 路径（C:/Users/chong/...）：Node 的
# isAbsolute() 在 win32 下认可它，JSON 无需转义，projectKey 把 / 和 \ 都
# 编码成 '-' 结果一致。
# 用 Python 而非 shell 管道：避免 set -o pipefail 下 `zstd | head -1` 的
# SIGPIPE 误判，也能用同一个「旧根→新根」映射同时改日志、所有 JSON/YAML
# 索引里的路径（含 workspace.json 的 "path" 键）、profiles 的 link: 路径，
# 以及按 projectKey 重命名会话目录。
rewrite_all() {
  local old_root="$1" new_root="$2"
  command -v zstd >/dev/null 2>&1 || { echo "跳过跨平台改写：未找到 zstd（brew install zstd）"; return 0; }
  echo "-- 跨平台改写: $old_root → $new_root"
  python3 - "$DEST" "$old_root" "$new_root" "$(command -v zstd)" <<'PY'
import os, sys, re, json, subprocess
root, OLD, NEW, ZSTD = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

def project_key(cwd):
    r = ""; sep = False
    for ch in cwd:
        if ch in "/\\:":
            if not sep: r += "-"
            sep = True
        elif ch != "~" and re.match(r'^[A-Za-z0-9._-]$', ch):
            r += ch; sep = False
        else:
            r += "~" + format(ord(ch), "04X"); sep = False
    return "--" + (re.sub(r'^-+', '', r) or "root")[:251] + "--"

# --- 0) 先读原始 workspace.json 算目录映射 ---
# 必须在第 2 步改写索引「之前」做，否则读到的已是改写后的新路径，映射会为空。
ws = os.path.join(root, '.dsh', 'storages', 'workspace.json')
mapping = {}
try:
    w = json.load(open(ws))
    for v in w['tables']['workspaces'].values():
        op = v['path']
        if op.startswith(OLD):
            np = NEW + op[len(OLD):]
            mapping[project_key(op)] = project_key(np)
except Exception as e:
    print("   ⚠️  无法读 workspace.json，跳过目录重命名:", e)

def decomp(path):
    # 完整捕获 stdout，不走 head/管道，避免 set -o pipefail 下的 SIGPIPE 误判
    r = subprocess.run([ZSTD, '-q', '-dc', path], capture_output=True)
    return r.stdout if r.returncode == 0 else None

def comp(data, path):
    r = subprocess.run([ZSTD, '-q', '-c'], input=data, capture_output=True)
    if r.returncode != 0: return False
    with open(path, 'wb') as f: f.write(r.stdout)
    return True

# 1) 改写 zstd 压缩会话日志：只替换 "cwd":"<OLD>/... 里的根，避免误伤聊天正文
sessions = os.path.join(root, '.dsh', 'sessions')
log_ok = log_fail = 0
pat = ('"cwd":"' + OLD + '/').encode()
rep = ('"cwd":"' + NEW + '/').encode()
for base, dirs, files in os.walk(sessions):
    for fn in files:
        if not fn.endswith('.jsonl.zstd'): continue
        p = os.path.join(base, fn)
        raw = decomp(p)
        if raw is None:
            print("   ⚠️  解压失败，保留:", p); log_fail += 1; continue
        tmp = p + '.rewrite.tmp'
        if not comp(raw.replace(pat, rep), tmp):
            print("   ⚠️  压缩失败，保留:", p); log_fail += 1
            try: os.unlink(tmp)
            except OSError: pass
            continue
        check = decomp(tmp)
        first = check.split(b'\n', 1)[0] if check else b''
        try:
            valid = json.loads(first.decode('utf-8')).get('type') == 'session'
        except Exception:
            valid = False
        if valid:
            os.replace(tmp, p); log_ok += 1
        else:
            print("   ⚠️  校验失败，保留:", p); log_fail += 1
            try: os.unlink(tmp)
            except OSError: pass
print(f"   会话日志: 改写 {log_ok} 个，失败 {log_fail} 个")

# 2) 只改配置/索引/技能清单里的绝对路径（含 workspace.json 的 "path"）。
#    刻意不遍历 home/.openviking、.mnemon、.config、.local 等记忆/缓存正文库，
#    避免篡改历史文本里提到的 Mac 路径（且对 DSH 启动毫无帮助）。
exts = ('.json', '.yml', '.yaml', '.imported', '.jsonl')
rewrite_roots = [
    os.path.join(root, '.dsh', 'storages'),
    os.path.join(root, '.dsh', 'skill-manager'),
    os.path.join(root, '.dsh', 'profiles'),
    os.path.join(root, '.dsh', 'sessions'),
    os.path.join(root, 'home', '.agents', 'skills'),
    os.path.join(root, 'home', '.workbuddy', 'skills'),
]
# .dsh 顶层散文件（settings.yaml.imported 等），不递归子目录
dsh_top = os.path.join(root, '.dsh')
top_files = [os.path.join(dsh_top, f) for f in os.listdir(dsh_top)
             if os.path.isfile(os.path.join(dsh_top, f))
             and f.endswith(exts) and f != 'MIGRATION_INFO.txt']

def rewrite_text(p):
    try:
        with open(p, 'r', encoding='utf-8') as f: s = f.read()
    except Exception: return False
    if OLD in s:
        with open(p, 'w', encoding='utf-8') as f: f.write(s.replace(OLD, NEW))
        return True
    return False

n = 0; seen = set()
for rr in rewrite_roots + top_files:
    if os.path.isfile(rr):
        if rr not in seen and rewrite_text(rr): n += 1
        seen.add(rr)
    elif os.path.isdir(rr):
        for base, dirs, files in os.walk(rr):
            if 'node_modules' in base or 'dsh-runtimes' in base: continue
            for fn in files:
                if not fn.endswith(exts): continue
                p = os.path.join(base, fn)
                if p in seen: continue
                if rewrite_text(p): n += 1
                seen.add(p)
print(f"   配置/索引/技能改写 {n} 个文件")

# 3) 按预先算好的 mapping 重命名会话目录
for a, b in mapping.items():
    if a == b: continue
    src, dst = os.path.join(sessions, a), os.path.join(sessions, b)
    if os.path.isdir(src) and not os.path.exists(dst):
        os.rename(src, dst); print(f"   目录: {a} → {b}")
PY
}

case "$TARGET_PRESET" in
  win-chong)
    rewrite_all "/Users/starinight/Documents" "C:/Users/chong/Documents"
    ;;
  linux)
    rewrite_all "/Users/starinight/Documents" "/home/$(id -un)/Documents"
    ;;
  "")
    echo "-- 未指定目标平台，会话日志保持原样（由导入脚本处理）"
    ;;
  *)
    echo "未知目标预设: $TARGET_PRESET（可用: win-chong / linux / 留空）" >&2
    exit 1
    ;;
esac

# ---------- 6. 迁移元信息（导入脚本用来改绝对路径） ----------
cat > "$DEST/MIGRATION_INFO.txt" <<EOF
exportedAt   = $(date '+%F %T %z')
oldHome      = $HOME
oldUser      = $(id -un)
arch         = $(uname -m)
os           = $(uname -s) $(uname -r)
dshVersion   = $(cat "$DSH/.workbuddy-app-version.json" 2>/dev/null || echo unknown)
targetPreset = ${TARGET_PRESET:-none}
logsRewritten= $([ -n "$TARGET_PRESET" ] && echo yes || echo no)
NOTE         = 会话目录名按工作区绝对路径编码；日志头 cwd 已按 targetPreset 改写。
EOF

echo
echo "== 导出完成 =="
du -sh "$DEST"
echo
echo "未拷贝（可重建 / 机器相关 / 按要求排除）："
echo "  ~/.dsh/dsh-sync/            （你说不用）"
echo "  ~/.dsh/dsh-runtimes/        （node/python/pnpm 二进制，跟 CPU 架构绑定）"
echo "  ~/.dsh/profiles/**/node_modules（插件实体，新机器由 DSH 按 package.json 重装）"
echo "  ~/.dsh/cache/               （缓存）"
echo "  ~/.workbuddy/ 除 skills/    （8.8G 缓存与运行数据）"
echo "  ~/.dsh-chrome-profile/      （浏览器缓存）"
echo
echo "把整个目录拷到新机器后："
echo "  macOS / Linux（未用预设导出）: ./dsh-import.sh <这个目录> [--dry-run]"
echo "  Windows / 用户 $TARGET_PRESET（用了 win-chong 预设）:"
echo "      powershell -ExecutionPolicy Bypass -File .\\dsh-import.ps1 -Src <这个目录>"
echo "      （会话日志/目录名已在 Mac 端改写好，Windows 端只负责拷贝，不要再改路径）"
echo "  ⚠️  若导出时没带 win-chong 而目标机是 Windows：会话日志是 Mac 路径，"
echo "      Windows 端无 zstd 无法改写，会话会打不开。请重新用 win-chong 导出。"
echo
echo "提示：拷到 Windows 时请用支持长路径与软链的介质；导出目录里的符号链接"
echo "      已由 rsync -aL 解引用成实体文件，U 盘/exFAT/NTFS 均可安全传输。"
