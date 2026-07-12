# easytier-mac 模块契约(M0)

对应计划:`.omc/plans/easytier-mac-gui-tray-supervisor.md`(已批准)。本文件是 supervisor / 安装脚本 / 验证脚本 / (M1)GUI 之间的接口契约,改动需同步计划书。

## 1. 命名与路径

| 项 | 值 |
|---|---|
| launchd Label | `com.easytier.supervisor` |
| plist 路径 | `/Library/LaunchDaemons/com.easytier.supervisor.plist` |
| 控制 socket | `/var/run/easytier.supervisor.sock` |
| 安装根目录 | `/Library/Application Support/EasyTier/`(root:wheel 0755) |
| 二进制目录 | `<安装根>/bin/`(easytier-supervisor、easytier-core,root:wheel 0755) |
| supervisor 配置 | `<安装根>/supervisor.toml`(root:wheel 0644) |
| 日志目录 | `<安装根>/logs/`(0755,文件 0644 保证普通用户可读;install.sh 预创建 supervisor.err.log / core.out.log 并锁 0644,不依赖 daemon 首次写入时的 umask) |
| crate | `easytier-mac/supervisor`,package `easytier-supervisor`(workspace member,非 default-member) |

## 2. launchd plist 关键键(安装脚本产出)

- `Label` = com.easytier.supervisor
- `ProgramArguments` = ["/Library/Application Support/EasyTier/bin/easytier-supervisor"]
- `Sockets.Listeners` = { `SockPathName` = "/var/run/easytier.supervisor.sock", `SockPathMode` = 438 }(438 = 0666;放宽由 peer 校验兜底,M2 收紧)
- `RunAtLoad` = false,`KeepAlive` = false(零常驻:仅 socket 连接触发激活)
- `ThrottleInterval` = 5(限制激活风暴,客户端需退避重试)
- `StandardErrorPath` = `<安装根>/logs/supervisor.err.log`

## 3. supervisor.toml(安装脚本写入)

```toml
proto = 1
owner_uid = 501            # 安装时的用户 uid,连接鉴权用
core_path = "/Library/Application Support/EasyTier/bin/easytier-core"
log_dir = "/Library/Application Support/EasyTier/logs"
```

## 4. 控制协议 v1(JSON Lines,UTF-8,每行一个对象)

客户端→supervisor 请求(`cmd`),supervisor→客户端 事件(`event`)。连接后必须在 **10s 内**完成 `hello`(超时关连接;owner 确立后无读超时),未 hello 前其他 cmd 一律拒绝。单条控制行上限 **64 KiB**,超限/非法 UTF-8 即关连接。

```jsonc
→ {"cmd":"hello","proto":1,"takeover":false}
← {"event":"hello","proto":1,"version":"0.1.0","core":"stopped","rpc_port":null}
→ {"cmd":"start"}
← {"event":"core_started","pid":12345,"rpc_port":50321}
→ {"cmd":"status"}
← {"event":"status","core":"running","pid":12345,"rpc_port":50321}
→ {"cmd":"stop"}
← {"event":"core_stopped","reason":"requested"}   // core 本就已停止时 reason="already_stopped"
```

- 错误:`{"event":"error","code":"spawn_failed|not_owner|bad_proto|...","msg":"..."}`
- `start` 幂等:core 已在运行时直接返回现有 `core_started`(pid/rpc_port),不报错
- 主动推送(仅 owner 连接):core 意外退出 → `{"event":"core_exited","code":N,"signal":S}`(supervisor 只清理**不重启**,重启决策在客户端)
- **单 owner lease**:第一个通过鉴权的连接成为 owner;后续连接收 `{"event":"busy","owner":true}` 后被关闭;`hello.takeover=true` 时踢掉旧 owner(旧连接收 `{"event":"kicked"}`)并接管
- **owner 断开(任何原因)= stop 语义**:SIGTERM core → 等 5s → SIGKILL → waitpid 确认死亡 → janitor → supervisor 退出(exit 0)
- 激活后 30s 内无鉴权 owner → 自行退出(按需进程不空驻)

## 5. 生命周期与 janitor

- **激活时(main 入口)先跑孤儿对账**:枚举进程(`ps -axww -o pid=,args=`),**argv[0] 精确等于 `core_path`**(args == core_path 或以 "core_path␣" 开头;不能按空白切 token——core_path 含空格)的残留 easytier-core → SIGKILL(macOS 无 pdeathsig,上代 supervisor 崩溃会留 root 孤儿);路由/utun 残留清理逻辑待 M0 实测(scripts/m0a)结果确定后补充——v0 只做进程扫杀,`janitor.rs` 预留 `cleanup_routes`。
- **spawn**:随机空闲端口(bind 127.0.0.1:0 探测后释放,竞态窗口接受);argv = `[core_path, --daemon, --rpc-portal, 127.0.0.1:<port>]`,并用 `CommandExt::arg0` 显式设 argv[0]=core_path 配合 janitor 识别;**不传** `--rpc-portal-whitelist`(core 默认白名单已是 `127.0.0.0/8, ::1/128`,该旗标冗余);清空 `ET_*` 环境变量(env_clear 后按需补 PATH);cwd = 安装根(log_dir 的父目录);子进程 stdout/stderr 追加到 `logs/core.out.log`。
- **停止升级**:SIGTERM → 100ms 轮询共 5s → SIGKILL;必须 waitpid 收尸后才能宣告 stopped / 退出(防两代竞态)。
- `--daemon` 已实证不 fork/detach(easytier/src/core.rs:1375 仅注册 DaemonGuard),子进程跟踪成立;此假设写进代码注释。

## 6. 鉴权(M0 = dev 级)

- 连接建立即 `getsockopt(SOL_LOCAL, LOCAL_PEERCRED)` 取 `xucred`:peer uid ∈ {0, owner_uid} 才放行,否则立即关闭并记日志。
- 代码签名校验(SecCode,pin Team ID)为 M2,预留 `auth.rs` 接口 + `signed-peers` feature 空实现。

## 7. 运行模式

- 默认:从 launchd 领 socket(`launch_activate_socket("Listeners")` FFI,拿不到 = 非 launchd 环境,报错退出)。
- `--dev-listen <path>`:自行 bind unix socket(先 unlink),鉴权退化为 peer uid == 进程 uid;供无 root 集成测试与 GUI 联调。
- `--config <path>`:覆盖 supervisor.toml 路径(默认见 §1)。

## 8. GUI(M1)契约【已存档】

> Tauri 版 GUI 已在 native 版功能对等验收后整体删除(2026-07-13),本节仅作历史契约存档;现行 GUI 契约见 §9。

| 项 | 值 |
|---|---|
| 位置 | `easytier-mac/gui`(Tauri 2 + Vue3;src-tauri 加入 Cargo workspace,gui 加入 pnpm workspace) |
| 标识 | bundle id `com.easytier.mac`,产品名 EasyTier |
| 复用 | 前端组件 `easytier-frontend-lib`;RPC client 用 easytier crate(path 依赖,仅公开 API:`StandAloneClient<TcpTunnelConnector>` + proto 类型) |
| profile 存储 | `~/Library/Application Support/EasyTier/profiles/<uuid>.toml`(TomlConfigLoader 兼容格式)+ `state.json`(上次运行集合、自启动开关、设置) |
| supervisor socket | 默认 `/var/run/easytier.supervisor.sock`;env `ET_SUPERVISOR_SOCKET` 覆盖(dev 联调用) |

行为契约:

- **生命周期**:启动 → 连 supervisor(触发 launchd 激活;失败按 ThrottleInterval 指数退避重试并提示)→ hello → `start` 拿 rpc_port → RPC 管理网络实例。关窗 = hide(拦截 CloseRequested)+ ActivationPolicy 切 Accessory;托盘"退出" = DeleteNetworkInstance 可跳过(断连即全体退出,由 supervisor 收尾)→ 断开控制连接 → app exit。
- **单实例**:tauri-plugin-single-instance;第二实例唤起第一实例窗口。takeover 由 busy 事件驱动:收到 busy → driver 暂停自动重连 + 弹模态确认框("检测到另一个 EasyTier 会话正持有控制连接,是否接管?接管会断开对方") → 确认后以 takeover=true 重连一次(失败回落常规退避),取消则保持暂停。无主动残留检测入口(后续项)。
- **网络启停**:`RunNetworkInstance`(config 文本随请求传,GUI 落盘 profiles/;保存时 loader.dump() 归一化并回写 id,保证 profile id == instance_id)/`DeleteNetworkInstance`;**最后一个实例停止时 GUI 主动 stop core**(不留空转进程);状态页轮询 peer/route(easytier-cli 同款 RPC)。core_exited 事件 → 通知用户 + 指数退避自动重启开关(默认开,连续 3 次失败停手)。
- **自启动**:tauri-plugin-autostart(login item),启动参数 `--hidden` 时不显窗口;启动后恢复 state.json 里"上次在跑"的 profile。
- **安装引导**:检测 plist/二进制缺失或版本不符 → 引导页一键安装(osascript 包装 scripts/install.sh,一次密码);卸载入口同理走 uninstall.sh。
- **冲突检测**:发现非托管 easytier-core 进程或 mihomo TUN 默认路由 → 明确提示(启用全隧道 profile 时警告共存风险)。
- **dev 联调**:用户态跑 `easytier-supervisor --dev-listen <sock> --config <dev.toml>`(core_path 指 target/debug/easytier-core),GUI 以 `ET_SUPERVISOR_SOCKET` 接入;no-tun 配置的实例可在无 root 下端到端验证(建实例/peer/状态/停止)。

## 9. M2:native GUI(SwiftUI + Bridge)契约增补

决策记录:GUI 转原生(docs/adr/0001)+ 单一用户所有只读配置(docs/adr/0002);术语见 CONTEXT.md。本节生效后,§8 仅对过渡期保留的 Tauri 版继续有效。

- **目录**:`easytier-mac/app`(SwiftUI 菜单栏应用)+ `easytier-mac/bridge`(cdylib,cargo workspace member);`gui/`(Tauri)已在功能对等验收后整体删除(历史契约存档于 §8)。
- **配置**:唯一 `~/Library/Application Support/EasyTier/config.toml`,GUI 只读;仅文件缺失时写入一次带注释模板(模板即格式文档,含常用字段说明);**不迁移** profiles/ 存量。校验 = `TomlConfigLoader` + `NetworkConfig::new_from_config` 双层解析。窗口激活时重读重校;连接前必校,失败禁用连接;连接中文件与运行中配置不一致 → 提示「配置已修改,重新连接后生效」。
- **Bridge 边界**:机制归 Rust——supervisor 协议驱动(含重连退避/takeover)、RPC(run/delete/status)、校验、事件回调;内置 tokio 运行时;结构化数据以 JSON 字符串过 FFI,事件走 C 回调。策略归 Swift——自动重启、启动恢复、设置持久化(UserDefaults;`state.json` 废弃,running 集合退化为 was_connected 布尔)。现有 `gui/src-tauri` 的 supervisor_client/rpc/install/conflict 模块迁入 bridge 复用。
- **instance_id**:废除「profile id == instance_id」契约,不再写进配置;Swift 侧内存跟踪 run 返回的 id。
- **UI**:窗口顶部「网络 / 设置」两 tab。网络页自上而下:连接条(状态、连接/断开、校验结果、打开配置)→ 速率图(轮询 rx/tx 差分)→ 节点表(虚拟IPv4/主机名/路由/协议/延迟/上下行/丢包/NAT/版本,本机行置顶;协议列需 bridge 在状态行中补 `get_conn_protos()`)。不做 VPN 门户与事件日志。未安装 supervisor 时网络页显示引导卡。托盘菜单 = 连接/断开 + 打开主窗口 + 退出。关窗进托盘、单实例、busy/takeover 交互沿用 §8。
- **系统**:目标 macOS 14+;自启 SMAppService(替代 tauri-plugin-autostart);打包 xcodebuild + ad-hoc 签名,安装脚本沿用 scripts/ 模式。

## 10. M0 测试策略

- 单测:协议编解码、状态机(hello/owner/takeover/stop 升级)。
- 集成测(非 root,`--dev-listen` + 假 core):假 core 为脚本(可选 trap TERM 不退,验证 SIGKILL 升级);覆盖 start/stop/断连即杀/takeover/busy/孤儿扫杀。
- 实机验证(scripts/,见各脚本头注释):m0a 信号残留(需 sudo + 真实网络配置,用户执行)、m0b --daemon 冒烟(非 root)、m0c 孤儿行为(非 root)。
