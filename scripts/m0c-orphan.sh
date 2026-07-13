#!/usr/bin/env bash
#
# m0c-orphan.sh — M0 实机验证:supervisor 崩溃后 core 是否孤儿化存活。
#
# 背景:Linux 有 prctl(PR_SET_PDEATHSIG),父进程死亡可以让内核给子进程发信号;
# macOS 没有对应机制。easytier-mac 的设计(见 DESIGN.md §5)因此假定:
# supervisor(旧代)崩溃/被杀后,它 spawn 出的 core 子进程会变成孤儿并继续
# 存活,被 launchd(pid 1)收养 —— 这正是新 supervisor 激活时必须先跑
# "孤儿对账"扫杀残留 core 进程的原因。这个脚本用一个"影子 supervisor"
# (wrapper bash 进程)去 spawn core,然后 SIGKILL wrapper,观察 core 是否
# 存活、以及新 ppid 是否变成 1,从而实测而非假设这个前提成立。
#
# 用法:
#   scripts/m0c-orphan.sh [core二进制路径]
#   默认使用 <仓库根>/vendor/EasyTier/target/debug/easytier-core(scripts/build-core.sh 产物)。
#
# 前置条件:非 root;已执行
#   scripts/build-core.sh
# 不传任何组网参数,不建 TUN,无需 sudo。
#
# 产物:~/et-m0-verify/<时间戳>-m0c/(core stdout/stderr 日志、core pid 文件)
# 退出码:0 = 全部 PASS;非 0 = 有 FAIL。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE_BIN="${1:-$REPO_ROOT/vendor/EasyTier/target/debug/easytier-core}"

if [[ ! -x "$CORE_BIN" ]]; then
    echo "FATAL: core binary not found or not executable: $CORE_BIN" >&2
    echo "       run: scripts/build-core.sh" >&2
    exit 1
fi

TS="$(date +%Y%m%d-%H%M%S)"
OUTDIR="$HOME/et-m0-verify/${TS}-m0c"
mkdir -p "$OUTDIR"
LOG="$OUTDIR/core.out.log"
COREPIDFILE="$OUTDIR/core.pid"

find_free_port() {
    python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}

PORT="$(find_free_port)"
echo "[m0c] outdir=$OUTDIR"
echo "[m0c] core=$CORE_BIN"
echo "[m0c] rpc-portal=127.0.0.1:$PORT"

CORE_PID=""
WRAPPER_PID=""
cleanup() {
    if [[ -n "$CORE_PID" ]] && kill -0 "$CORE_PID" 2>/dev/null; then
        echo "[m0c] cleanup: killing leftover core pid=$CORE_PID"
        kill -9 "$CORE_PID" 2>/dev/null || true
    fi
    if [[ -n "$WRAPPER_PID" ]] && kill -0 "$WRAPPER_PID" 2>/dev/null; then
        echo "[m0c] cleanup: killing leftover wrapper pid=$WRAPPER_PID"
        kill -9 "$WRAPPER_PID" 2>/dev/null || true
        wait "$WRAPPER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

PASS=0
FAIL=0
result() {
    local name="$1" ok="$2" detail="$3"
    if [[ "$ok" == "0" ]]; then
        echo "[PASS] $name — $detail"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $name — $detail"
        FAIL=$((FAIL + 1))
    fi
}

# 影子 supervisor:spawn core、把 core pid 写文件、然后常驻(wait)直到被
# SIGKILL —— 模拟真实 supervisor 崩溃时"来不及做任何清理"的场景。
echo "[m0c] starting shadow-supervisor wrapper..."
bash -c '
    core_bin="$1"; port="$2"; log="$3"; pidfile="$4"
    "$core_bin" --daemon --rpc-portal "127.0.0.1:$port" >"$log" 2>&1 &
    echo $! > "$pidfile"
    wait
' _ "$CORE_BIN" "$PORT" "$LOG" "$COREPIDFILE" &
WRAPPER_PID=$!
echo "[m0c] wrapper pid=$WRAPPER_PID, waiting for core pid file..."

for _ in $(seq 1 50); do
    [[ -s "$COREPIDFILE" ]] && break
    sleep 0.2
done
if [[ ! -s "$COREPIDFILE" ]]; then
    echo "FATAL: core pid file not created within 10s" >&2
    exit 1
fi
CORE_PID="$(cat "$COREPIDFILE")"
echo "[m0c] core pid=$CORE_PID (spawned by wrapper=$WRAPPER_PID)"

sleep 2

PPID_BEFORE="$(ps -o ppid= -p "$CORE_PID" 2>/dev/null | tr -d ' ')"
echo "[m0c] before kill: core ppid=$PPID_BEFORE (expect == wrapper $WRAPPER_PID)"
if [[ "$PPID_BEFORE" == "$WRAPPER_PID" ]]; then
    result "core is a direct child of wrapper" 0 "ppid=$PPID_BEFORE"
else
    result "core is a direct child of wrapper" 1 "ppid=$PPID_BEFORE != wrapper $WRAPPER_PID"
fi

echo "[m0c] SIGKILL wrapper pid=$WRAPPER_PID (simulating supervisor crash, no cleanup chance)"
kill -9 "$WRAPPER_PID" 2>/dev/null || true
wait "$WRAPPER_PID" 2>/dev/null || true
WRAPPER_PID=""

echo "[m0c] waiting 3s to observe orphan behavior..."
sleep 3

if kill -0 "$CORE_PID" 2>/dev/null; then
    result "core survives wrapper SIGKILL (no macOS pdeathsig)" 0 "pid=$CORE_PID still alive"
else
    result "core survives wrapper SIGKILL (no macOS pdeathsig)" 1 "pid=$CORE_PID died together with wrapper — macOS behaved AS IF it has pdeathsig, contradicts assumption"
fi

if kill -0 "$CORE_PID" 2>/dev/null; then
    PPID_AFTER="$(ps -o ppid= -p "$CORE_PID" 2>/dev/null | tr -d ' ')"
    if [[ "$PPID_AFTER" == "1" ]]; then
        result "orphan reparented to launchd (ppid==1)" 0 "ppid=$PPID_AFTER"
    else
        result "orphan reparented to launchd (ppid==1)" 1 "ppid=$PPID_AFTER (unexpected, expected 1)"
    fi
else
    result "orphan reparented to launchd (ppid==1)" 1 "n/a — core already dead"
fi

echo ""
echo "[m0c] summary: PASS=$PASS FAIL=$FAIL  log=$LOG"
if [[ "$FAIL" -eq 0 ]]; then
    echo "[m0c] CONCLUSION: macOS 确无 pdeathsig 效果 —— supervisor 崩溃会留下存活的孤儿 core 进程(被 launchd 收养),证实 janitor 的孤儿扫杀逻辑是必要的。"
else
    echo "[m0c] CONCLUSION: 实测结果与预期不符,需要重新评估 janitor 的必要性/实现前提(见上面 FAIL 项)。"
fi

# final cleanup of the (now orphaned, ppid==1) core process
if [[ -n "$CORE_PID" ]] && kill -0 "$CORE_PID" 2>/dev/null; then
    echo "[m0c] final cleanup: killing orphaned core pid=$CORE_PID"
    kill -9 "$CORE_PID" 2>/dev/null || true
fi
CORE_PID=""

if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
exit 0
