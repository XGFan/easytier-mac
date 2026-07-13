# easytier-mac 维护指南

macOS 原生 EasyTier 客户端(SwiftUI 菜单栏 GUI + root supervisor),从 XGFan/EasyTier fork 的 `easytier-mac/` 子目录拆分而来的独立仓库。项目概览与构建安装见 README.md;本文件记录维护时需要知道的结构与流程。

## 三个组件与契约

| 组件 | 位置 | 说明 |
|---|---|---|
| supervisor | `supervisor/` | root、launchd 按需激活,spawn/kill core,生命周期跟随 GUI 连接;**不依赖 easytier 代码** |
| bridge | `bridge/` | Rust staticlib/cdylib,supervisor 协议 + core RPC + 配置校验 → C ABI 给 Swift;path 依赖 `vendor/EasyTier/easytier`(仅公开 API) |
| app | `app/` | SwiftUI(macOS 14+,xcodegen 工程);链接 bridge 的 .a(路径 `$(SRCROOT)/../target/<profile>`) |

**DESIGN.md 是接口契约**(路径、launchd 键值、控制协议、生命周期、hooks、GUI 行为):改任何路径/协议/生命周期语义,先同步 DESIGN.md,再改代码/脚本。CONTEXT.md 是术语表;docs/adr/ 记决策(新决策按既有三段式:决定/理由/否决的替代方案)。

## 与 EasyTier fork 的关系

- `vendor/EasyTier` submodule = `git@github.com:XGFan/EasyTier.git` 的 `releases/v2.6.4` 分支(fork = 上游 release + macOS 全隧道修复 + 尚未删除的 easytier-mac 旧副本)。fork 的本地 checkout 在 `~/Developer/Tool/EasyTier`,其维护流程(rebase 上游、rerere)见该仓库的 CLAUDE.md。
- **一个 pin,两个用途**:bridge 链接的 easytier 库和 supervisor 拉起的 easytier-core 二进制都出自这个 submodule,保证 RPC 协议匹配(ADR-0003)。core 二进制用 `scripts/build-core.sh` 构建,产物在 `vendor/EasyTier/target/`(submodule 自己的 workspace),不落在本仓库根 `target/`。
- **升级流程**:fork rebase + push 后,`cd vendor/EasyTier && git fetch origin && git checkout origin/releases/v2.6.4`,回根目录重建验证,提交 gitlink。fork 分支是 force-push 的,**每次 bump 建议在 fork 侧给 pin 的 commit 打 tag 并 push**,防旧 commit 被 GC 后 submodule 拉不到。
- 根 `Cargo.toml` 的 `[profile]` 与 fork 根保持一致(release panic=abort 等),bridge 内嵌的 easytier 在本 workspace 构建时行为才与 fork 内一致;fork 侧若改 profile,这里同步。

## 构建与测试

- 前置:Rust 1.95(rust-toolchain.toml)、protoc、Xcode + xcodegen。
- `cargo build -p easytier-supervisor -p easytier-mac-bridge`;app 用 `scripts/app-install.sh`(cargo → xcodegen → xcodebuild → ditto 到 /Applications)。
- 测试:`cargo build -p easytier-supervisor && cargo test -p easytier-supervisor -p easytier-mac-bridge`——bridge 的集成测试(`bridge/tests/dev_supervisor.rs`)会从 `target/debug/` 找 supervisor 二进制拉真进程,必须先 build。
- 实机验证脚本 `scripts/m0{a,b,c}-*.sh` 覆盖单测测不到的场景(信号残留/daemon 行为/孤儿化),改 supervisor 生命周期逻辑后跑一遍对应脚本。

## 修改时的注意点

- bridge 的 C 头 `bridge/include/easytier_bridge.h` 与 Swift 侧 `app/Sources/Bridge*.swift` 手工同步,改 FFI 先改头文件。
- 脚本里安装路径全部是 DESIGN.md §1 的字面常量(含空格的 `/Library/Application Support/EasyTier`),进程匹配按完整路径精确匹配,不要改成按进程名模糊匹配。
- `app/EasyTier.xcodeproj` 是 xcodegen 生成物(git-ignore),只提交 `app/project.yml`。
