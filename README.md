# easytier-mac

macOS 原生 [EasyTier](https://github.com/EasyTier/EasyTier) 客户端:SwiftUI 菜单栏 GUI + root 特权 supervisor,架构类似 Tailscale 的 GUI/tailscaled 分离。零常驻:不连接时系统里没有任何 EasyTier 进程。

节点进程与 RPC 库来自 [XGFan/EasyTier fork](https://github.com/XGFan/EasyTier)(上游 + macOS 全隧道修复),以 git submodule 钉在 `vendor/EasyTier`,见[《对 EasyTier 的依赖》](#对-easytier-的依赖)。

## 架构

```
┌────────────────────── 登录用户 ──────────────────────┐
│  EasyTier.app        SwiftUI 菜单栏应用(app/)       │
│    │ C ABI,静态链接                                  │
│  bridge              Rust staticlib(bridge/)        │
│    │ 控制协议                  │ 管理 RPC             │
│    │ (unix socket, JSONL)     │ (tcp, 127.0.0.1)     │
└────┼───────────────────────────┼──────────────────────┘
┌────┼──────────────── root ─────┼──────────────────────┐
│  easytier-supervisor(supervisor/,launchd 按需激活) │
│    │ spawn/kill,生命周期跟随 GUI 连接                │
│  easytier-core(由 vendor/EasyTier 构建)◄───────────┘
└────────────────────────────────────────────────────────┘
```

- **supervisor**:root 权限、launchd socket 按需激活。GUI 连上才拉起 core,GUI 退出/崩溃则 core 全停、supervisor 自身也退出。支持生命周期 hooks(core 启停时以 root 执行 `up.sh`/`down.sh`,典型场景 DNS 切换)。不依赖 easytier 代码。
- **bridge**:Rust cdylib/staticlib,把 supervisor 控制协议、core 管理 RPC、配置校验封成小 C API 给 Swift。机制归 Rust,策略(重启、恢复、设置)归 Swift。
- **app**:macOS 14+ 菜单栏应用。单一只读配置 `~/Library/Application Support/EasyTier/config.toml`,用户自己编辑,GUI 只做校验/连接/断开。

完整接口契约见 [DESIGN.md](DESIGN.md),术语表见 [CONTEXT.md](CONTEXT.md),决策记录见 [docs/adr/](docs/adr/)。

## 仓库布局

| 目录 | 内容 |
|---|---|
| `app/` | SwiftUI 菜单栏应用(xcodegen 工程,`project.yml` 是唯一提交的工程描述) |
| `bridge/` | Rust bridge crate(`easytier-mac-bridge`,staticlib/cdylib) |
| `supervisor/` | 特权守护进程 crate(`easytier-supervisor`) |
| `scripts/` | 安装/卸载/构建/实机验证脚本,hooks 示例 |
| `vendor/EasyTier` | EasyTier fork submodule(easytier 库 + easytier-core 二进制的唯一来源) |
| `docs/adr/` | 架构决策记录 |

## 对 EasyTier 的依赖

bridge 以 path 依赖使用 submodule 里的 easytier 库(仅公开 API:RPC 客户端、配置解析),supervisor 拉起的 `easytier-core` 二进制也从同一 submodule 构建。**一个 submodule pin 同时钉死"链接的库"和"运行的二进制"**,保证两者 RPC 协议永远匹配。为什么不用 cargo git 依赖,见 [ADR-0003](docs/adr/0003-standalone-repo-with-vendored-fork-submodule.md)。

fork 的 macOS 全隧道修复不改 RPC/配置面,且只在全隧道场景(`0.0.0.0/0` 拆分路由 / exit node)生效:**全隧道场景必须用 fork 构建的 core**(`scripts/build-core.sh`);非全隧道场景可以用官方同版本二进制替代,见[「使用官方 core」](#使用官方-core非全隧道)。

## 构建与安装

前置:macOS 14+、Xcode(xcodebuild)、`brew install xcodegen protobuf`、Rust(版本由 `rust-toolchain.toml` 固定)。

```bash
git clone --recurse-submodules <本仓库地址>
cd easytier-mac

# 1. 构建 easytier-core(在 vendor submodule 内,产物 vendor/EasyTier/target/release/)
scripts/build-core.sh --release

# 2. 构建 supervisor
cargo build --release -p easytier-supervisor

# 3. 安装 supervisor 为 launchd daemon(一次 sudo,幂等可重复执行)
sudo scripts/install.sh \
  --supervisor-bin target/release/easytier-supervisor \
  --core-bin vendor/EasyTier/target/release/easytier-core \
  --owner-uid "$(id -u)"

# 4. 构建并安装 GUI 到 /Applications(不要加 sudo)
scripts/app-install.sh --release
```

卸载:`sudo scripts/uninstall.sh`(保留用户 hooks),再删除 `/Applications/EasyTier.app`。

配置文件:`~/Library/Application Support/EasyTier/config.toml`(EasyTier 标准 TOML 配置;首启缺失时 GUI 写入一次带注释模板,此后永不改写)。

DNS 等联动:把 `scripts/hooks-examples/{up.sh,down.sh}` 改造后装进 `/Library/Application Support/EasyTier/hooks/`(权限要求见脚本头注释与 DESIGN.md「Hooks」)。

### 使用官方 core(非全隧道)

配置不涉及全隧道时,可跳过本地编译,直接用官方 release 二进制(版本自动对齐 vendor pin):

```bash
scripts/fetch-official-core.sh      # 产物 target/official/easytier-core,含冒烟校验

# 替换已安装的 core(GUI 先断开连接,重连后生效)
sudo install -o root -g wheel -m 0755 target/official/easytier-core \
  "/Library/Application Support/EasyTier/bin/easytier-core"
```

全新安装时也可以把上面第 3 步 `install.sh` 的 `--core-bin` 直接指到该产物。切回 fork 版:`scripts/build-core.sh --release` 后用同样方式替换。注意:一旦配置要开全隧道,先换回 fork 构建的 core,否则会踩回上游的路由环路/打洞失败等 bug。

## 开发

```bash
cargo build -p easytier-mac-bridge          # bridge(连带编译 easytier 库,首次较久)
cargo build -p easytier-supervisor          # supervisor
cargo test  -p easytier-supervisor -p easytier-mac-bridge   # bridge 集成测试依赖 supervisor 二进制,先 build 再 test
```

- GUI 联调:`easytier-supervisor --dev-listen <sock> --config <dev.toml>` 用户态跑 supervisor(无需 root),GUI 侧以 `ET_SUPERVISOR_SOCKET` 指向该 socket。
- 实机验证:`scripts/m0a-signal-residue.sh`(sudo,信号残留)、`m0b-daemon-smoke.sh`、`m0c-orphan.sh`(非 root),用法见各脚本头注释。
- app 工程:`cd app && xcodegen generate` 后可用 Xcode 打开;命令行构建走 `scripts/app-install.sh`。

## 升级 vendor/EasyTier

fork 侧(rebase 上游新 release 后 push)完成后,在本仓库:

```bash
cd vendor/EasyTier
git fetch origin
git checkout origin/releases/v2.6.4        # 上游开新 release 分支时换成新分支名
cd ../..
scripts/build-core.sh && cargo build -p easytier-mac-bridge   # 重建验证 API 兼容
git add vendor/EasyTier
git commit -m "chore: bump vendor/EasyTier"
```

注意:fork 的 release 分支用 rebase + force-push 维护,旧 pin 的 commit 会在远端变为不可达,**建议每次 bump 后在 fork 侧给 pin 的 commit 打 tag**(如 `mac-base-20260713`)并 push,防止 GitHub GC 导致后续 `git submodule update` 拉不到对象。

## License

本仓库自有代码(app/、bridge/、supervisor/、scripts/)以 [Apache-2.0](LICENSE) 发布。`vendor/EasyTier` 为 LGPL-3.0;构建产物(静态链接 easytier 库的 bridge/app、easytier-core 二进制)包含 LGPL-3.0 代码,分发时需遵守其条款(本仓库开源已满足源码可得性要求)。
