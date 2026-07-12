/*
 * easytier_bridge.h — easytier-mac Bridge 的 C API 契约(手写维护,无 cbindgen)。
 *
 * 契约来源:easytier-mac/DESIGN.md §9、docs/adr/0001。本头文件是 Rust 实现
 * (bridge/src/ffi.rs)与 Swift 调用方(app/ 的桥接头)之间唯一的真理源;
 * 两侧改动必须同步此文件。
 *
 * ── 所有权与线程规则 ─────────────────────────────────────────────────────
 * 1. 所有返回 char* 的函数:返回值由 Rust 分配,调用方用完必须且只能经
 *    etb_free_string() 释放;NULL 表示"成功且无内容"(见各函数注释)。
 * 2. 所有传入的 const char* 参数在调用返回后即可释放(Rust 侧即拷贝)。
 * 3. 除 etb_validate / etb_install / etb_uninstall / etb_detect_conflicts
 *    (无句柄纯函数)外,函数均需有效 EtbHandle;句柄非线程安全约定之外:
 *    所有带句柄函数可从任意线程调用(内部经 tokio 运行时串行化)。
 * 4. 带句柄函数为阻塞调用(connect 最长约 6s 有界重试),Swift 侧必须从
 *    后台线程/Task 调用,不得在主线程直呼。
 * 5. 事件回调在 Rust 运行时线程触发;回调内只做最小工作(拷贝 JSON 后
 *    调度回主线程),不得在回调内再调用任何 etb_* 函数。
 *
 * ── 事件 JSON schema(event_cb 收到的 event_json)────────────────────────
 * {"type":"connected","version":"0.1.0","core":"running"|"stopped","rpc_port":u16|null}
 *     — supervisor 控制连接建立(含重连成功)
 * {"type":"disconnected"}                    — 控制连接断开(bridge 自动退避重连)
 * {"type":"core_started","pid":u32,"rpc_port":u16}
 * {"type":"core_stopped","reason":"requested"|"already_stopped"|string}
 * {"type":"core_exited","code":i32|null,"signal":i32|null}
 *     — core 意外退出;重启决策在 Swift(策略归 Swift,DESIGN §9)
 * {"type":"busy","owner":true}               — 另一 GUI 实例持有 owner lease
 * {"type":"kicked"}                          — 被 takeover 踢下线
 * {"type":"error","code":string,"msg":string}
 *
 * ── 结果 JSON schema ────────────────────────────────────────────────────
 * etb_validate:
 *   {"ok":true} | {"ok":false,"error":"..."}
 * etb_status(连接中):
 *   {"ok":{"instance_id":"...","rows":[{
 *      "peer_id":u32,"hostname":"...","ipv4":"...","cost":"local"|"direct"|"relay(N)",
 *      "latency_ms":f64,"loss_rate":f64,"rx_bytes":u64,"tx_bytes":u64,
 *      "nat_type":"...","version":"...","is_local":bool,"protos":["udp","tcp",...]}]}}
 *   | {"err":"..."}     (未连接/core 未运行时为 err)
 * etb_supervisor_status:
 *   {"connected":bool,"core_running":bool,"rpc_port":u16|null,"installed":bool}
 * etb_detect_conflicts:
 *   与 gui/src-tauri/src/conflict.rs 的 Conflicts 序列化一致
 */

#ifndef EASYTIER_BRIDGE_H
#define EASYTIER_BRIDGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* 不透明句柄:一个 bridge 会话 = 内置 tokio 运行时 + supervisor 驱动(含退避重连)。*/
typedef struct EtbHandle EtbHandle;

/* 事件回调:event_json 仅在回调期间有效,需立即拷贝;ctx 为 etb_init 透传。*/
typedef void (*etb_event_cb)(const char *event_json, void *ctx);

/* 创建会话并启动 supervisor 驱动。失败返回 NULL(极罕见:运行时创建失败)。
 * env ET_SUPERVISOR_SOCKET 可覆盖 socket 路径(dev 联调,DESIGN §8)。*/
EtbHandle *etb_init(etb_event_cb event_cb, void *ctx);

/* 优雅关停:断开控制连接(= supervisor stop 语义)、停 tokio、释放句柄。
 * 调用后句柄失效。*/
void etb_shutdown(EtbHandle *handle);

/* 连接:确保 core 运行(触发 launchd 激活)+ 以 toml_text 运行网络实例。
 * bridge 内部记录 instance_id 供 status/disconnect 使用。
 * 返回 NULL = 成功;否则为错误信息字符串。*/
char *etb_connect(EtbHandle *handle, const char *toml_text);

/* 断开:删除当前实例并停掉 core(零常驻)。幂等;返回 NULL = 成功。*/
char *etb_disconnect(EtbHandle *handle);

/* 当前实例的节点状态快照(schema 见文件头)。总是返回非 NULL JSON。*/
char *etb_status(EtbHandle *handle);

/* supervisor/core/安装状态(schema 见文件头)。总是返回非 NULL JSON。*/
char *etb_supervisor_status(EtbHandle *handle);

/* 请求接管另一实例的 owner lease(busy 事件后经用户确认调用)。*/
void etb_takeover(EtbHandle *handle);

/* 校验配置文本(TomlConfigLoader + NetworkConfig 双层解析,不触盘不改文件)。
 * 无需句柄。总是返回非 NULL JSON。*/
char *etb_validate(const char *toml_text);

/* 安装/卸载特权 supervisor(osascript 一次密码,内部走 scripts/install.sh)。
 * supervisor_bin/core_bin 传 NULL 用默认路径。返回 NULL = 成功。*/
char *etb_install(const char *supervisor_bin, const char *core_bin);
char *etb_uninstall(void);

/* 冲突检测(非托管 easytier-core 进程、mihomo TUN 默认路由)。非 NULL JSON。*/
char *etb_detect_conflicts(void);

/* 释放本库返回的任何 char*。传 NULL 为 no-op。*/
void etb_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* EASYTIER_BRIDGE_H */
