#!/usr/bin/env bash
# 在 Linux（Ubuntu 26.10 / GNOME Wayland）上启动 OpenPicasa。
#
# 二进制由 `cargo build --release --bin OpenPicasa` 产出。
# 需要 cargo 环境时：
#   export PATH="$HOME/.cargo/bin:$PATH"
#
# 可选环境变量：
#   OPENPICASA_LOG=debug         打开调试日志（默认 release 只有 info）
#   OPENPICASA_DATA_DIR=/some/dir 覆盖应用数据目录（默认 ~/Library/Application Support/app.openpicasa）
#   WGPU_BACKEND=gl               wgpu 后端强制走 GL（Vulkan 出问题时用）

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$here/target/release/OpenPicasa"

if [[ ! -x "$bin" ]]; then
    echo "二进制不存在：$bin" >&2
    echo "先编译：" >&2
    echo "  cd \"$here\" && cargo build --release --bin OpenPicasa" >&2
    exit 1
fi

exec "$bin" "$@"
