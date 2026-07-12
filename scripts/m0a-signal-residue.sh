#!/usr/bin/env bash
#
# m0a-signal-residue.sh — M0 实机验证:core 装好全隧道路由后被
# SIGTERM / SIGKILL,会残留哪些 utun 接口 / 路由条目。
#
# 背景:easytier-mac 新 supervisor 的 janitor(DESIGN.md §5)目前只做进程
# 扫杀,"路由/utun 残留清理逻辑待 M0 实测结果确定后补充"。这个脚本对 TERM 和
# KILL 各跑一轮全隧道组网,在信号前后拍快照做三方 diff,直接回答:
#   - 优雅终止(SIGTERM,内核给应用层清理的机会)干净吗?
#   - 强杀(SIGKILL,应用层来不及清理)残留了什么?
# 结果决定 janitor 是否需要、以及需要清理哪些具体的路由模式。
#
# 依赖 m0b-daemon-smoke.sh 已实证的结论:--daemon 不 fork/detach,所以
# `"$CORE_BIN" ... &` 之后的 `$!` 从头到尾就是要跟踪、发信号、waitpid 的
# 那个 pid,不存在"真正干活的进程是孙进程"的问题。
#
# macOS 全隧道路由的已知形态(见 easytier/src/instance/virtual_nic.rs 的
# MACOS_SPLIT_DEFAULT_ROUTES 与 macos_bypass 模块,commit c23ce668):
#   - 0.0.0.0/0 被拆成 8 条经由 TUN 自身地址的网关型路由:
#       1.0.0.0/8  2.0.0.0/7  4.0.0.0/6  8.0.0.0/5
#       16.0.0.0/4 32.0.0.0/3 64.0.0.0/2 128.0.0.0/1
#     (故意跳过保留的 0.0.0.0/8;老版本可能残留 0.0.0.0/1 拆分路由)
#   - 若干 /32 host 路由,经由物理网关(add_ipv4_route_via_gateway,不是
#     `route add -host` 形式),给 underlay 端点(对端 IP)做 bypass。实测
#     (见下方 assert_clean_baseline 的设计依据)这类路由在 netstat -rn 里
#     保留 /32 后缀、走 UGSc 标志,不是"裸地址 + H 标志"的经典 host 路由
#     形态——因此识别时用「目的地址以 /32 结尾 且 网关是可路由的点分十进制
#     IPv4(排除 link#N/MAC 网关,那些是系统自动生成的 on-link/ARP 条目)」,
#     而不是按 Flags 列找 H(本机实测 H 标志几乎全部来自 ARP 缓存 UHLWI*
#     条目,按 H 匹配会把局域网邻居全部误判成"残留路由")。
#   - 一个新增的 utunN 接口
#
# 用法:
#   sudo easytier-mac/scripts/m0a-signal-residue.sh [--round term|kill|both] <core二进制> <config.toml>
#   --round 默认 both(TERM 轮 + KILL 轮各跑一次)。
#   若 TERM 轮结束后 utun/路由没有回到 baseline,KILL 轮会被跳过(报告里会
#   说明原因,避免把 TERM 的残留也算成 KILL 的);清理残留后可以用
#   `--round kill` 单独补跑 KILL 轮。
#
# <config.toml> 必须是一份能真实联网、并会装 0.0.0.0/0 拆分全隧道路由的配置,
# 由使用者提供(需要真实的 easytier 网络/对端才能建立隧道并触发路由安装,
# 因此本脚本不自带默认配置)。
#
# 需要 sudo:核心进程要建 TUN、改路由表。
#
# 每轮流程:
#   基线快照(ifconfig -l / netstat -rn -f inet)
#   → 断言 baseline 干净(不含拆分默认路由、不含疑似残留的 bypass /32,含
#     则打印证据并中止整个脚本——机器上有其它全隧道工具或上次跑残留时,
#     测出的"残留"结论不可信)
#   → 启动 core(记录 pid)
#   → 轮询等待 utun 出现 且 拆分路由装好(超时 90s,超时则报错并清理)
#   → 装好后快照
#   → 发送信号
#   → 等进程消失(≤10s)
#   → 信号后快照
#   → 只对"本轮新增"的 utun/路由(而不是粗暴地拿 up 和 post 求交集)计算
#     残留,避免把 baseline 里本来就存在的其它 utun/路由(比如 mihomo 自己
#     的 utun)误判成"这次信号造成的残留"
#   → 三方 diff,写入报告
#
# 产物:~/et-m0-verify/<时间戳>-m0a/report.md 及各阶段快照文件。
#
# 安全:
#   - 只 kill 本脚本自己记录的 pid,绝不按名字 pkill/killall。
#   - trap 保证异常退出时也会尝试收尸(kill 记录的 pid),但不会动路由表
#     (路由残留正是本脚本要观测的对象,不做善后清理;信号后的路由残留由
#     使用者根据报告自行判断是否需要手工清理)。
#   - --rpc-portal 显式传 127.0.0.1:<空闲高位端口>,避免撞到 15888..15900
#     范围内可能已存在的其它 easytier-core 实例。
#
# 已知的、故意不处理的边角:
#   - $HOME 路径若含空格,report 路径拼接可能出问题(权衡实现复杂度后接受)。
#   - find_free_port 探测后立即释放端口再启动 core,存在极小的 TOCTOU 竞态
#     窗口(和真实 supervisor 的 spawn 策略一致,DESIGN.md §5 也接受这一点)。
#
# 本文件可以被 source(自测用),此时只加载函数定义、不会触发 root 检查或
# 真正跑一轮测试——只有直接执行(`./m0a-signal-residue.sh ...` 或
# `bash m0a-signal-residue.sh ...`)才会调用 main()。

set -euo pipefail

# split-route destinations we expect to see once the tunnel is fully up
# (network/prefix pairs from MACOS_SPLIT_DEFAULT_ROUTES)
SPLIT_ROUTE_NETS=(1.0.0.0/8 2.0.0.0/7 4.0.0.0/6 8.0.0.0/5 16.0.0.0/4 32.0.0.0/3 64.0.0.0/2 128.0.0.0/1)

find_free_port() {
    python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

snapshot() {
    # snapshot <label> <outfile-prefix>
    local label="$1" prefix="$2"
    ifconfig -l >"${prefix}.ifaces.txt"
    netstat -rn -f inet >"${prefix}.routes.txt"
    echo "[m0a] snapshot '$label' -> ${prefix}.{ifaces,routes}.txt"
}

count_split_route_hits() {
    # crude check: netstat prints destinations abbreviated (e.g. "1/8", "2/7",
    # "128.0.0.0/1" depending on the row); grep loosely for the octets so this
    # doesn't depend on exact column formatting.
    local routes_file="$1"
    local hits=0
    local net octet
    for net in "${SPLIT_ROUTE_NETS[@]}"; do
        octet="${net%%.*}"
        if grep -qE "^${octet}(\.0){0,3}(/[0-9]+)?[[:space:]]" "$routes_file"; then
            hits=$((hits + 1))
        fi
    done
    echo "$hits"
}

routes_look_installed() {
    local routes_file="$1"
    local hits
    hits="$(count_split_route_hits "$routes_file")"
    # require a majority of the 8 split routes to be visible before declaring
    # "installed" — avoids false negatives from netstat's abbreviation quirks
    # while still detecting a genuinely-not-installed-yet state.
    [[ "$hits" -ge 5 ]]
}

# suspicious_bypass_routes <routes_file>
# Matches lines whose destination ends in "/32" and whose gateway is a
# routable dotted-quad IPv4 address — the on-the-wire signature of
# add_ipv4_route_via_gateway() bypass routes (see header comment for why
# this is more reliable here than an H-flag check). Excludes link#N / MAC
# gateways, which are system-generated on-link/ARP entries, not ours.
suspicious_bypass_routes() {
    local routes_file="$1"
    awk '$1 ~ /\/32$/ && $2 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/' "$routes_file"
}

# assert_clean_baseline <routes_file> — the caller must abort the whole
# script (not just the current round) when this returns non-zero; a
# contaminated baseline makes any "residue" conclusion drawn from this run
# untrustworthy.
assert_clean_baseline() {
    local routes_file="$1"
    local hits
    hits="$(count_split_route_hits "$routes_file")"
    if [[ "$hits" -gt 0 ]]; then
        echo "FATAL: baseline 已经存在 $hits/8 条拆分全隧道路由(1.0.0.0/8..128.0.0.0/1 形态)。" >&2
        echo "       疑似有其它全隧道 VPN/代理工具(比如 mihomo 的 TUN 模式)或上次异常退出的" >&2
        echo "       easytier-core 占用着这些路由,请清理后在干净状态下重跑。命中的路由行:" >&2
        local octet_alt
        octet_alt="$(printf '%s|' "${SPLIT_ROUTE_NETS[@]%%.*}")"
        octet_alt="${octet_alt%|}"
        grep -E "^(${octet_alt})(\.0){0,3}(/[0-9]+)?[[:space:]]" "$routes_file" >&2 || true
        return 1
    fi
    if grep -qE '^0\.0\.0\.0' "$routes_file"; then
        echo "FATAL: baseline 已经存在以 0.0.0.0 为目的地址的路由(老版本拆分残留或其它全隧道工具),请清理后重跑:" >&2
        grep -E '^0\.0\.0\.0' "$routes_file" >&2 || true
        return 1
    fi
    local suspicious
    suspicious="$(suspicious_bypass_routes "$routes_file")"
    if [[ -n "$suspicious" ]]; then
        echo "FATAL: baseline 已经存在疑似残留的 bypass /32 路由(目的地址 /32 + 网关是可路由 IPv4,匹配" >&2
        echo "       add_ipv4_route_via_gateway 的输出形态),很可能是上一次 core 异常退出留下的:" >&2
        echo "$suspicious" >&2
        return 1
    fi
    return 0
}

# --- residue computation (single source of truth for both the report text
# and the term->kill round-skip decision) ---
NEW_UTUN_THIS_ROUND=""
RESIDUAL_UTUN=""
NEW_ROUTES_THIS_ROUND=""
RESIDUAL_ROUTES=""

compute_utun_residue() {
    # compute_utun_residue <base-ifaces-file> <up-ifaces-file> <post-ifaces-file>
    local base="$1" up="$2" post="$3"
    NEW_UTUN_THIS_ROUND="$(comm -13 <(tr ' ' '\n' <"$base" | sort) <(tr ' ' '\n' <"$up" | sort) | grep '^utun' || true)"
    if [[ -n "$NEW_UTUN_THIS_ROUND" ]]; then
        RESIDUAL_UTUN="$(comm -12 <(printf '%s\n' "$NEW_UTUN_THIS_ROUND" | sort) <(tr ' ' '\n' <"$post" | sort) || true)"
    else
        RESIDUAL_UTUN=""
    fi
}

compute_route_residue() {
    # compute_route_residue <base-routes-file> <up-routes-file> <post-routes-file>
    local base="$1" up="$2" post="$3"
    NEW_ROUTES_THIS_ROUND="$(comm -13 <(sort "$base") <(sort "$up") || true)"
    if [[ -n "$NEW_ROUTES_THIS_ROUND" ]]; then
        RESIDUAL_ROUTES="$(comm -12 <(printf '%s\n' "$NEW_ROUTES_THIS_ROUND" | sort) <(sort "$post") || true)"
    else
        RESIDUAL_ROUTES=""
    fi
}

print_block() {
    # print_block <content> [empty-label]
    local content="$1" empty_label="${2:-(none)}"
    echo '```'
    if [[ -n "$content" ]]; then
        printf '%s\n' "$content"
    else
        echo "$empty_label"
    fi
    echo '```'
}

highlight_lines_in() {
    # highlight_lines_in <routes_file> — union of the "pay attention to"
    # signatures, deduped; used both in the report and could be reused by a
    # human eyeballing a snapshot directly.
    local routes_file="$1"
    local octet_alt
    octet_alt="$(printf '%s|' "${SPLIT_ROUTE_NETS[@]%%.*}")"
    octet_alt="${octet_alt%|}"
    {
        grep -E '^0\.0\.0\.0' "$routes_file" || true
        grep -E "^(${octet_alt})(\.0){0,3}(/[0-9]+)?[[:space:]]" "$routes_file" || true
        suspicious_bypass_routes "$routes_file" || true
    } | sort -u
}

three_way_diff() {
    # three_way_diff <base-prefix> <up-prefix> <post-prefix> <label> <report-file>
    # assumes compute_utun_residue / compute_route_residue already ran for
    # this round and populated the NEW_*/RESIDUAL_* globals above.
    local post="$3" label="$4" report="$5"
    local highlighted
    highlighted="$(highlight_lines_in "${post}.routes.txt")"
    {
        echo "### $label"
        echo
        echo "**本轮新增的 utun 接口(baseline -> up):**"
        print_block "$NEW_UTUN_THIS_ROUND"
        echo
        echo "**信号后仍存在的、本轮新增的 utun 接口(残留;不含 baseline 里本来就有的其它 utun,比如 mihomo 的):**"
        print_block "$RESIDUAL_UTUN"
        echo
        echo "**本轮新增的路由条目(baseline -> up):**"
        print_block "$NEW_ROUTES_THIS_ROUND"
        echo
        echo "**信号后仍残留的、本轮新增的路由条目:**"
        print_block "$RESIDUAL_ROUTES"
        echo
        echo "**重点关注:0.0.0.0 目标 / 拆分默认路由(1.0.0.0/8..128.0.0.0/1)/ 疑似 bypass /32(目的地址 /32 + 可路由 IPv4 网关)是否残留(对信号后快照做全表扫描,不限于本轮新增,可能包含无关的既存条目):**"
        print_block "$highlighted" "(none matched)"
        echo
    } >>"$report"
}

wait_for_tunnel_up() {
    # wait_for_tunnel_up <baseline-ifaces-file> <timeout-secs>
    local baseline_ifaces="$1" timeout="$2"
    local waited=0
    while [[ "$waited" -lt "$timeout" ]]; do
        local cur_ifaces cur_routes new_utun
        cur_ifaces="$(ifconfig -l)"
        new_utun="$(comm -13 <(tr ' ' '\n' <"$baseline_ifaces" | sort) <(echo "$cur_ifaces" | tr ' ' '\n' | sort) | grep '^utun' || true)"
        cur_routes="$(mktemp)"
        netstat -rn -f inet >"$cur_routes"
        if [[ -n "$new_utun" ]] && routes_look_installed "$cur_routes"; then
            rm -f "$cur_routes"
            echo "$new_utun"
            return 0
        fi
        rm -f "$cur_routes"
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}

run_round() {
    # run_round <signal-name> <signal-flag>
    # Returns: 0 = clean, 2 = ran fine but left residue, 1 = tunnel never
    # came up (see core.out.log). Aborts the whole script (exit 1) if the
    # baseline itself is contaminated — that's not a per-round failure.
    local sig_name="$1" sig_flag="$2"
    local round_dir="$OUTDIR/$sig_name"
    mkdir -p "$round_dir"
    local base="$round_dir/baseline" up="$round_dir/up" post="$round_dir/post-signal"

    echo ""
    echo "[m0a] ===== round: $sig_name ====="

    echo "[m0a] baseline snapshot"
    snapshot "baseline" "$base"

    if ! assert_clean_baseline "${base}.routes.txt"; then
        echo "[m0a] ABORTING: baseline is not clean, see FATAL messages above." >&2
        exit 1
    fi

    local port
    port="$(find_free_port)"
    # --daemon 与真实 supervisor 的 spawn argv(DESIGN.md §5)保持一致;
    # 该 flag 只影响"零网络实例时是否自动退出",不影响路由/utun 安装逻辑。
    echo "[m0a] starting core (rpc-portal 127.0.0.1:$port): $CORE_BIN --daemon -c $CONFIG_FILE --rpc-portal 127.0.0.1:$port"
    "$CORE_BIN" --daemon -c "$CONFIG_FILE" --rpc-portal "127.0.0.1:$port" >"$round_dir/core.out.log" 2>&1 &
    CORE_PID=$!
    echo "[m0a] core pid=$CORE_PID"

    echo "[m0a] waiting up to 90s for utun + split routes to appear..."
    local new_utun
    if ! new_utun="$(wait_for_tunnel_up "$base.ifaces.txt" 90)"; then
        echo "[m0a] FATAL: tunnel did not come up within 90s for round $sig_name; see $round_dir/core.out.log" >&2
        kill -9 "$CORE_PID" 2>/dev/null || true
        wait "$CORE_PID" 2>/dev/null || true
        CORE_PID=""
        return 1
    fi
    echo "[m0a] tunnel up, new utun: $new_utun"

    echo "[m0a] up snapshot"
    snapshot "up" "$up"

    echo "[m0a] sending $sig_name to pid=$CORE_PID"
    kill "$sig_flag" "$CORE_PID" 2>/dev/null || true

    local waited=0
    local gone=1
    while [[ "$waited" -lt 10 ]]; do
        if ! kill -0 "$CORE_PID" 2>/dev/null; then
            gone=0
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [[ "$gone" == "0" ]]; then
        wait "$CORE_PID" 2>/dev/null || true
        echo "[m0a] process exited within ${waited}s after $sig_name"
    else
        echo "[m0a] WARNING: process still alive 10s after $sig_name — forcing SIGKILL before snapshotting"
        kill -9 "$CORE_PID" 2>/dev/null || true
        wait "$CORE_PID" 2>/dev/null || true
    fi
    CORE_PID=""

    echo "[m0a] post-signal snapshot"
    snapshot "post-signal" "$post"

    compute_utun_residue "${base}.ifaces.txt" "${up}.ifaces.txt" "${post}.ifaces.txt"
    compute_route_residue "${base}.routes.txt" "${up}.routes.txt" "${post}.routes.txt"
    three_way_diff "$base" "$up" "$post" "信号: $sig_name" "$REPORT"

    if [[ -n "$RESIDUAL_UTUN" || -n "$RESIDUAL_ROUTES" ]]; then
        return 2
    fi
    return 0
}

cleanup() {
    if [[ -n "${CORE_PID:-}" ]] && kill -0 "$CORE_PID" 2>/dev/null; then
        echo "[m0a] cleanup: killing leftover core pid=$CORE_PID"
        kill -9 "$CORE_PID" 2>/dev/null || true
        wait "$CORE_PID" 2>/dev/null || true
    fi
}

usage() {
    cat >&2 <<EOF
usage: sudo $0 [--round term|kill|both] <core-binary> <config.toml>
  --round   which round(s) to run (default: both)
EOF
}

main() {
    if [[ "$(id -u)" != "0" ]]; then
        echo "FATAL: must run as root (sudo) — core needs to create a TUN device and modify routes." >&2
        exit 1
    fi

    local round="both"
    local positional=()
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --round)
                round="${2:-}"
                shift 2
                ;;
            --round=*)
                round="${1#--round=}"
                shift
                ;;
            -h | --help)
                usage
                exit 0
                ;;
            *)
                positional+=("$1")
                shift
                ;;
        esac
    done
    if [[ "$round" != "term" && "$round" != "kill" && "$round" != "both" ]]; then
        echo "FATAL: --round must be one of term|kill|both, got: $round" >&2
        exit 1
    fi
    if [[ "${#positional[@]}" -lt 2 ]]; then
        usage
        exit 1
    fi

    CORE_BIN="${positional[0]}"
    CONFIG_FILE="${positional[1]}"

    if [[ ! -x "$CORE_BIN" ]]; then
        echo "FATAL: core binary not found or not executable: $CORE_BIN" >&2
        exit 1
    fi
    if [[ ! -f "$CONFIG_FILE" ]]; then
        echo "FATAL: config file not found: $CONFIG_FILE" >&2
        exit 1
    fi
    CORE_BIN="$(cd "$(dirname "$CORE_BIN")" && pwd)/$(basename "$CORE_BIN")"
    CONFIG_FILE="$(cd "$(dirname "$CONFIG_FILE")" && pwd)/$(basename "$CONFIG_FILE")"

    # sudo 下 $HOME 可能仍是原用户的 home(常见于 macOS sudo 默认不重置 HOME);
    # 用 SUDO_USER 的 home 更稳妥,拿不到就退回当前 $HOME。
    local real_home=""
    if [[ -n "${SUDO_USER:-}" ]]; then
        real_home="$(dscl . -read "/Users/$SUDO_USER" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
    fi
    REAL_HOME="${real_home:-$HOME}"

    TS="$(date +%Y%m%d-%H%M%S)"
    OUTDIR="$REAL_HOME/et-m0-verify/${TS}-m0a"
    mkdir -p "$OUTDIR"
    if [[ -n "${SUDO_USER:-}" ]]; then
        chown -R "$SUDO_USER" "$REAL_HOME/et-m0-verify" 2>/dev/null || true
    fi
    REPORT="$OUTDIR/report.md"

    echo "[m0a] outdir=$OUTDIR"
    echo "[m0a] core=$CORE_BIN"
    echo "[m0a] config=$CONFIG_FILE"
    echo "[m0a] round=$round"

    CORE_PID=""
    trap cleanup EXIT

    {
        echo "# M0a 信号残留实测报告"
        echo
        echo "- 时间: $(date)"
        echo "- core: $CORE_BIN"
        echo "- config: $CONFIG_FILE"
        echo "- round: $round"
        echo
    } >"$REPORT"

    local term_status=3 kill_status=3 kill_skipped=0

    if [[ "$round" == "term" || "$round" == "both" ]]; then
        # 有意的 `run_round ... && a=0 || a=$?` 写法:run_round 处于 && 的
        # 左侧,函数体内 set -e 会被 bash 暂时关闭(这是 bash 的既知行为,不
        # 是 bug)。所以 run_round 内部不能依赖 set -e 自动中止——所有关键
        # 失败路径(tunnel 没起来、baseline 不干净)都已经显式 return/exit,
        # 不指望顶层 errexit 兜底。
        run_round "TERM" "-TERM" && term_status=0 || term_status=$?
    fi

    if [[ "$round" == "kill" || "$round" == "both" ]]; then
        if [[ "$round" == "both" && "$term_status" == "2" ]]; then
            echo "[m0a] SKIPPING KILL round: TERM round left residue (utun 或路由未回到 baseline,见报告),继续跑会把 TERM 的残留也算成 KILL 的。" >&2
            echo "[m0a]   请根据报告清理残留后,用 --round kill 单独补跑 KILL 轮。" >&2
            kill_skipped=1
        else
            run_round "KILL" "-KILL" && kill_status=0 || kill_status=$?
        fi
    fi

    {
        echo "## 结论"
        echo
        if [[ "$round" == "term" || "$round" == "both" ]]; then
            case "$term_status" in
                0) echo "- **TERM 干净吗?** 是——见上面 \"信号: TERM\" 一节,本轮新增的 utun/路由信号后全部消失。" ;;
                2) echo "- **TERM 干净吗?** 否——见上面 \"信号: TERM\" 一节的残留区块,有本轮新增的 utun/路由在信号后依然存在。" ;;
                *) echo "- **TERM 轮次执行异常**(超时或进程未在 10s 内退出),结论不可靠,需要重跑。" ;;
            esac
        fi
        if [[ "$kill_skipped" == "1" ]]; then
            echo "- **KILL 轮被跳过**:TERM 轮未把 utun/路由清理回 baseline,继续跑 KILL 轮会把 TERM 的残留也算成 KILL 的残留,结论不可信。清理残留后用 \`--round kill\` 单独补跑。"
        elif [[ "$round" == "kill" || "$round" == "both" ]]; then
            case "$kill_status" in
                0) echo "- **KILL 残留了什么?** 无——见上面 \"信号: KILL\" 一节,本轮新增的 utun/路由信号后全部消失(不太可能,但如实汇报)。" ;;
                2) echo "- **KILL 残留了什么?** 见上面 \"信号: KILL\" 一节的残留区块;这些就是 janitor 必须清理的对象(典型预期:拆分默认路由 1.0.0.0/8..128.0.0.0/1、bypass /32、utun 接口本身)。" ;;
                *) echo "- **KILL 轮次执行异常**(超时或进程未在 10s 内退出),结论不可靠,需要重跑。" ;;
            esac
        fi
        echo "- **janitor 需要清什么?** 以 KILL 轮的残留列表为准 —— 若 TERM 轮已经干净,说明只有强杀路径才需要 janitor 兜底;若 TERM 轮也有残留,则每次 core 退出都需要 janitor 校验。"
        echo
    } >>"$REPORT"

    echo ""
    echo "[m0a] report written to $REPORT"
    if [[ -n "${SUDO_USER:-}" ]]; then
        chown -R "$SUDO_USER" "$OUTDIR" 2>/dev/null || true
    fi

    local term_bad=0 kill_bad=0
    if [[ "$round" == "term" || "$round" == "both" ]] && [[ "$term_status" != "0" ]]; then
        term_bad=1
    fi
    if [[ "$kill_skipped" == "1" ]]; then
        kill_bad=1
    elif [[ "$round" == "kill" || "$round" == "both" ]] && [[ "$kill_status" != "0" ]]; then
        kill_bad=1
    fi
    if [[ "$term_bad" == "1" || "$kill_bad" == "1" ]]; then
        exit 1
    fi
    exit 0
}

# Allow this file to be sourced (e.g. by a self-test harness) without
# executing main() — only run when invoked directly.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
