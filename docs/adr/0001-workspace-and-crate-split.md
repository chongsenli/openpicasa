# ADR-0001：虚拟 workspace + 四 crate 分层，M1 不建未来才用的 crate

- 状态：已采纳
- 日期：2026-10-29
- 相关：02 技术架构 §4、§9.2

## 背景

02 文档给出了 `picasa-db / picasa-imaging / picasa-core / picasa-ui` 的分层，同时也列出了
`picasa-edit / picasa-face / picasa-ipc / picasa-cli` 这些 M2/M3 才会用到的边界。
如果不加限制地把它们都先建出来，M1 就会得到四个空的 `lib.rs`，而「编辑室入口不存在」
这条红线（R-2）反而更难被机械验证——因为编译产物里确实存在一个 edit crate。

## 决定

1. 根 `Cargo.toml` 为**虚拟清单**（virtual manifest，只有 `[workspace]`，没有 `[package]`），
   `resolver = "3"`，`edition 2024`，`rust-version = 1.95`。
2. M1 只建四个 crate，依赖方向严格单向：
   ```
   picasa-ui → picasa-core → picasa-db
                        ↘ → picasa-imaging
   ```
   即 db 与 imaging 互不依赖，core 是唯一的编排层，ui 是唯一的 egui 依赖方。
3. **不建** `picasa-edit`、`picasa-face`、`picasa-ipc`、`picasa-cli`。R-2 的验证方式因此变成
   「`cargo metadata` 的 members 里没有 edit 相关 crate」+「`picasa-ui` 源码里不存在编辑入口」，
   而不是「有一个但没启用」。
4. 依赖版本一律写进 `[workspace.dependencies]`，成员 crate 只写 `{ workspace = true }`，
   避免 egui 这类强耦合依赖出现版本漂移。

## 后果

- 好处：M1 的编译产物可审计——没有任何一条通往图像写回的路由是「存在但未调用」。
- 代价：M2 开工时需要新增 crate 并调整 `members`，属于一次性低成本动作。
- 副作用：虚拟清单不允许写 `[features]`（cargo 会直接报错
  `virtual manifest specifies a features section`）。这一点在实现时踩过，写在这里备查。
