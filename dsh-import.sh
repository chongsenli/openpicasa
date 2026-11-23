#!/bin/bash
# DSH 数据导入（配合 dsh-export.sh 使用，纯文件夹拷贝）
#
# 用法:
#   ./dsh-import.sh ~/Desktop/dsh-migration-20261107-223500
#   ./dsh-import.sh ~/Desktop/dsh-migration-20261107-223500 --dry-run
#
# 前置条件:
#   1. 新机器已安装并至少启动过一次同版本 DSH（让它生成自己的 ~/.dsh 骨架）
#   2. 完全退出 DeepSeek Harness
#   3. 建议先备份新机器现有的 ~/.dsh
set -euo pipefail

SRC="${1:-}"
DRY="${2:-}"
DSH="$HOME/.dsh"

if [ -z "$SRC" ] || [ ! -d "$SRC/.dsh" ]; then
  echo "用法: $0 <导出目录> [--dry-run]"
  echo "导出目录里应包含 .dsh/ 子目录（由 dsh-export.sh 生成）"
  exit 1
fi
SRC="${SRC%/}"

if pgrep -f "dsh-desktop-host" >/dev/null 2>&1; then
  echo "⚠️  DSH 似乎仍在运行，请先完全退出再导入。"
  echo "   继续请按 y，其它键取消："
  read -r ans
  [ "$ans" = "y" ] || { echo "已取消"; exit 1; }
fi

RUN=(rsync -a --exclude '.DS_Store')
[ "$DRY" = "--dry-run" ] && RUN=(rsync -a --dry-run --exclude '.DS_Store')

OLD_HOME=$(sed -n 's/^oldHome[[:space:]]*=[[:space:]]*//p' "$SRC/MIGRATION_INFO.txt" 2>/dev/null || echo "")
OLD_HOME="${OLD_HOME:-$HOME}"

echo "== DSH 导入 =="
echo "源:   $SRC"
echo "目标: $DSH"
echo "旧 HOME: $OLD_HOME"
echo "新 HOME: $HOME"
[ -n "$DRY" ] && echo "*** dry-run：只显示将要执行的操作 ***"
echo

# ---------- 0. 先备份新机器现状（可回滚） ----------
if [ -z "$DRY" ] && [ -d "$DSH" ]; then
  BAK="$HOME/.dsh.before-import-$(date +%Y%m%d-%H%M%S)"
  echo "-- 备份当前 ~/.dsh → $BAK"
  cp -a "$DSH" "$BAK"
fi

mkdir -p "$DSH"

# ---------- 1. 核心数据 ----------
for item in sessions storages skills skill-manager attachments task-board \
            dsh-claude-style workbuddy-vault dsh-market llm-deepseek; do
  [ -d "$SRC/.dsh/$item" ] || continue
  echo "-- .dsh/$item"
  mkdir -p "$DSH/$item"
  "${RUN[@]}" "$SRC/.dsh/$item/" "$DSH/$item/"
done

# ---------- 2. 配置 + 插件清单 ----------
echo "-- .dsh 顶层配置文件"
for f in "$SRC"/.dsh/settings.yaml.imported "$SRC"/.dsh/.anonymous-user-id \
         "$SRC"/.dsh/.buddy-checkin.json "$SRC"/.dsh/.workbuddy-ai-catalog.json \
         "$SRC"/.dsh/.workbuddy-catalog.json "$SRC"/.dsh/.workbuddy-app-version.json \
         "$SRC"/.dsh/dsh-skill-picker-state.json "$SRC"/.dsh/.credentials.yaml; do
  [ -f "$f" ] || continue
  [ "$DRY" = "--dry-run" ] && echo "   cp $(basename "$f")" || cp -p "$f" "$DSH/$(basename "$f")"
done

echo "-- profiles/desktop 与 profiles/web"
for p in desktop web; do
  [ -d "$SRC/.dsh/profiles/$p" ] || continue
  mkdir -p "$DSH/profiles/$p"
  "${RUN[@]}" "$SRC/.dsh/profiles/$p/" "$DSH/profiles/$p/"
done

# ---------- 3. HOME 里的技能 / 记忆 ----------
if [ -d "$SRC/home" ]; then
  mkdir -p "$HOME/.agents"
  echo "-- ~/.agents/skills"
  "${RUN[@]}" "$SRC/home/.agents/skills/" "$HOME/.agents/skills/"

  for item in .workbuddy/skills .mnemon .openviking \
              .config/billion-context .local/share/billion-context; do
    [ -d "$SRC/home/$item" ] || continue
    echo "-- ~/$item"
    mkdir -p "$HOME/$item"
    "${RUN[@]}" "$SRC/home/$item/" "$HOME/$item/"
  done
fi

# ---------- 4. 本地 link: 插件 ----------
if [ -d "$SRC/extras/deepseek workspace/dsh-full-width" ]; then
  echo "-- 本地插件 dsh-full-width"
  # 建议新机器把项目放在与旧机器相同的相对位置：~/Documents/deepseek workspace/
  TARGET="$HOME/Documents/deepseek workspace/dsh-full-width"
  if [ "$DRY" = "--dry-run" ]; then
    echo "   → $TARGET"
  else
    mkdir -p "$(dirname "$TARGET")"
    rsync -a --exclude '.DS_Store' "$SRC/extras/deepseek workspace/dsh-full-width/" "$TARGET/"
  fi
fi

# 你自写的备份辅助脚本
if [ -d "$SRC/extras/Application Support/dsh-sync" ]; then
  DEST="$HOME/Library/Application Support/dsh-sync"
  echo "-- 辅助脚本 → $DEST"
  if [ "$DRY" = "--dry-run" ]; then :; else
    mkdir -p "$DEST"
    rsync -a "$SRC/extras/Application Support/dsh-sync/" "$DEST/"
  fi
fi

# ---------- 5. 改写硬编码的绝对路径 ----------
# 已知位置：~/.dsh/storages/workspace.json、skill-manager/workspaces.json、
#          session_projcache*、profiles/desktop/package.json 与 pnpm-lock.yaml
if [ "$OLD_HOME" != "$HOME" ] && [ -z "$DRY" ]; then
  echo "-- 把配置里的 $OLD_HOME 改写为 $HOME"
  # 用 python 逐文件替换，避免 sed 处理 JSON 转义
  python3 - "$OLD_HOME" "$HOME" "$DSH" <<'PY'
import os, sys
old, new, root = sys.argv[1], sys.argv[2], sys.argv[3]
exts = ('.json', '.yml', '.yaml', '.imported', '.jsonl')
count = 0
for base, dirs, files in os.walk(root):
    if 'node_modules' in base or 'dsh-runtimes' in base:
        continue
    for fn in files:
        if not fn.endswith(exts):
            continue
        p = os.path.join(base, fn)
        try:
            with open(p, 'r', encoding='utf-8') as f: s = f.read()
        except Exception:
            continue
        if old in s:
            with open(p, 'w', encoding='utf-8') as f: f.write(s.replace(old, new))
            count += 1
            print('   ', os.path.relpath(p, root))
print(f'   共改写 {count} 个文件')
PY
fi

echo
echo "== 导入完成 =="
echo
cat <<'EOF'
接下来（重要）：
1. 先不要启动 DSH。打开 ~/.dsh/profiles/desktop/package.json，检查
   "dsh-full-width": "link:/旧路径..." 是否还指向旧机器路径；
   若新机器项目不在相同位置，改成新路径，或者删掉这一行。
2. 启动 DeepSeek Harness。它读取 profiles/desktop/package.json 后会自动
   用 pnpm 重装插件（首次启动较慢、需要联网）。node_modules 不用手拷。
3. 会话目录是按工作区绝对路径编码命名的，例如
   --Users-starinight-Documents-OpenPIcasa--。
   若新机器用户名或项目路径不同，旧会话不会出现在侧栏里：
   最省事的做法是把项目放到与旧机器相同的路径
   （旧机器是 ~/Documents/OpenPIcasa，见 MIGRATION_INFO.txt）。
4. 如需校验，先运行 ./dsh-import.sh <目录> --dry-run 看清单。
EOF
