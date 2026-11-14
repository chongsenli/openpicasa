#!/usr/bin/env bash
# 用**隔离的全新空库**运行 OpenPicasa（不会读 / 写 ~/Library 里的真实图库）。
#
# 背景：M4 首次运行会触发 v8 迁移；用真实库（1.1 GB / 29,816 张）跑既慢又有风险。
#
# 两条硬约束（都是实机运行踩过的坑）：
# 1. `OPENPICASA_DATA_DIR` 必须用 `export` 显式导出——行内赋值 + `& / nohup / timeout`
#    组合下偶发不被子进程继承，应用会静默回退到 ~/Library 读真实库。
#    应用侧另有 `AppPaths::from_root` 硬断言 + `run()` 的 INFO 日志双重兜底。
# 2. **默认永远是全新空库**：指定目录若已存在（含上次会话的 library.sqlite /
#    preferences.json / ui_state.ron），旧数据会让「空库」名不副实——曾出现隔离库
#    里凭空带着真实照片目录 pap.er 的情况。复用只在显式 `--reuse` 时允许。
#
# 用法：
#   ./run-isolated.sh                       # 全新空库（自动新建带时间戳的临时目录）
#   ./run-isolated.sh /some/dir             # 指定目录；若已存在会报错并退出
#   ./run-isolated.sh /some/dir --reuse     # 显式复用已有隔离目录（调试用）
#   OPENPICASA_LOG=debug ./run-isolated.sh
#
# 日志在 <dir>/Logs/app.openpicasa/openpicasa.log

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# ⚠️ 默认用 **release**：① 性能（debug 下 EXIF/缩略图路径会明显慢，用户会误判成
# 「索引变慢」）；② epaint 的 `Other mesh is invalid` 是 `debug_assert`，release 下
# 被编译掉，交互时不会因个别退化几何直接崩溃。debug 版用 --debug 切换。
bin="$here/target/release/OpenPicasa"
for arg in "$@"; do
    [[ "$arg" == "--debug" ]] && bin="$here/target/debug/OpenPicasa"
done
if [[ ! -x "$bin" ]]; then
    echo "二进制不存在，先编译：cargo build --release --bin OpenPicasa" >&2
    exit 1
fi

reuse=0
data_dir=""
for arg in "$@"; do
    case "$arg" in
        --reuse) reuse=1 ;;
        *) if [[ -z "$data_dir" ]]; then data_dir="$arg"; fi ;;
    esac
done
data_dir="${data_dir:-/tmp/openpicasa-isolated-$(date +%s)}"

if [[ -e "$data_dir" ]]; then
    # 「全新空库」是这份脚本的承诺：已存在的目录一律拒绝，除非显式 --reuse。
    # 尤其要防的是目录里残留 library.sqlite —— 那会让本次运行读到上次的图库状态。
    if [[ "$reuse" -eq 0 ]]; then
        echo "拒绝启动：数据目录已存在：$data_dir" >&2
        echo "  这份脚本承诺「全新空库」，复用旧目录会带入上次的图库状态。" >&2
        echo "  换一个新路径，或显式加 --reuse 复用（调试用）。" >&2
        exit 2
    fi
    # ⚠️ 变量必须写成 ${data_dir}：紧跟全角「（」时 `set -u` 下的 bash 会把多字节字符
    # 并进变量名，直接报 `data_dir?: unbound variable`（实机踩过）。
    echo "⚠️  复用已有隔离目录：${data_dir}（非全新空库）"
else
    mkdir -p "$data_dir"
fi

# ⚠️ 必须 export：行内赋值在 & / nohup / timeout 组合下偶发不继承。
export OPENPICASA_DATA_DIR="$data_dir"
export OPENPICASA_LOG="${OPENPICASA_LOG:-info}"

echo "二进制  : $bin"
echo "数据目录: $OPENPICASA_DATA_DIR  ($( [[ "$reuse" -eq 0 ]] && echo '全新空库' || echo '复用' ))"
echo "日志    : $data_dir/Logs/app.openpicasa/openpicasa.log"
echo "启动后请在日志里确认这一行："
echo "  INFO ... 启动数据目录（已隔离，不会读 ~/Library 的真实图库）"
echo "------------------------------------------------------------"

# "$@" 里的 --reuse / --debug 与目录参数都不是应用参数，一律不透传。
# `set -u` 下空数组不能直接展开，按长度分两条 exec。
app_args=()
for arg in "$@"; do
    [[ "$arg" == "--reuse" || "$arg" == "--debug" || "$arg" == "$data_dir" ]] || app_args+=("$arg")
done
if [[ ${#app_args[@]} -eq 0 ]]; then
    exec "$bin"
else
    exec "$bin" "${app_args[@]}"
fi
