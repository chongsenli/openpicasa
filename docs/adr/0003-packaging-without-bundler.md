# ADR-0003：手写 `bundle.sh` 组 bundle，不引 `cargo-bundle` / `cargo-ndk`

- 状态：已采纳
- 日期：2026-10-29
- 相关：02 技术架构 §9.3、`packaging/bundle.sh`

## 背景

Rust 侧打 macOS `.app` 有成熟工具（`cargo-bundle`、`cargo-packager`、`tauri-bundler`）。
但它们都默认走「cargo 依赖 + 一套 YAML/TOML 配置」的路子，对本项目的三个具体要求都不擅长：

1. **Info.plist 必须逐字可控**。R-3 与 TCC 授权（03 §5B）依赖 `NSPhotoLibraryUsageDescription`
   等四个 `NS*FolderUsageDescription` 键的**精确文案**，以及 `CFBundleIdentifier` 必须与
   `picasa_core::paths::BUNDLE_ID` 完全一致（否则偏好/索引写到 `~/Library/Application Support/`
   下的另一个目录，重启后「照片没了」）。
2. **ad-hoc 签名要能显式校验**。M1 不做公证，但要求 `codesign -dv` 能通过、且未来能平滑换成
   Developer ID。工具自带的签名步骤是一层黑盒，出问题时看不到中间态。
3. **构建产物路径要能被 CI 断言**（`dist/OpenPicasa.app/Contents/MacOS/OpenPicasa`）。

## 决定

写一个 60 行的 `packaging/bundle.sh`，顺序固定：

```
cargo build --release --bin OpenPicasa
  → rm -rf dist/OpenPicasa.app && mkdir -p Contents/{MacOS,Resources}
  → cp target/release/OpenPicasa Contents/MacOS/
  → python3 packaging/make_icon.py 生成 icns（iconutil -c icns）
  → sed 把 __VERSION__/__BUILD__ 写进 Info.plist → plutil -lint 校验
  → codesign --force --sign - --timestamp=none
  → codesign -dv --verbose=2 && du -sh 体积上限检查
```

每一步失败即 `set -euo pipefail` 中止，不留半成品 bundle。

## 后果

- 好处：plist 字段、签名身份、产物路径三件事都是明文可读、可 diff 的 shell。
- 代价：多维护一个脚本；`iconutil` 只在 macOS 上存在（Windows/Linux 构建不出 `.app`，
  这在 M1 是可接受的，M2 的跨平台打包另立 ADR）。
- 已知限制：脚本不做公证（notarization），首次打开需要用户右键 →「打开」，
  这与 03 §5B 的验收描述一致。
