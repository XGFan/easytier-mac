#!/usr/bin/env bash
#
# m0b-daemon-smoke.sh — M0 实机验证:easytier-core --daemon 是否真的不 fork/detach。
#
# 背景:easytier-mac 的 supervisor 假定 `--daemon` 只是让 core 注册一个
# DaemonGuard 以避免"无网络实例时自动退出"(见 easytier/src/core.rs:1375,
# 1414-1421 及 manager.wait() 的 select 循环),core 进程本身保持前台、不
# fork、不脱离控制终端、不建子进程 —— 因此 supervisor spawn 时拿到的那个
# pid,从头到尾就是它需要 SIGTERM/SIGKILL/waitpid 的那个 pid。这个假设是
# supervisor 进程跟踪与 janitor 逻辑成立的前提,必须实测验证,不能只靠读码。
#
# 用法:
#   easytier-mac/scripts/m0b-daemon-smoke.sh [core二进制路径]
#   默认使用 <仓库根>/target/debug/easytier-core。
#
# 前置条件:非 root;已执行
#   cargo build -p easytier --bin easytier-core
# 不传任何组网参数,不建 TUN,无需 sudo。
#
# 验证点:
#   1. 启动 10s 后进程仍存活(空跑不该秒退 —— DaemonGuard 生效)
#   2. 该 pid 的父进程就是本脚本自身(未 fork/detach 出新会话)
#   3. 该 pid 没有子进程(没有二次 fork 出真正干活的孙进程)
#   4. RPC 端口确实在监听
#   5. SIGTERM 后能在 10s 内退出并被本脚本 waitpid 收尸
#
# 产物:~/et-m0-verify/<时间戳>-m0b/(core stdout/stderr 日志)
# 退出码:0 = 全部 PASS;非 0 = 有 FAIL。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CORE_BIN="${1:-$REPO_ROOT/target/debug/easytier-core}"

if [[ ! -x "$CORE_BIN" ]]; then
    echo "FATAL: core binary not found or not executable: $CORE_BIN" >&2
    echo "       run: cargo build -p easytier --bin easytier-core" >&2
    exit 1
fi

TS="$(date +%Y%m%d-%H%M%S)"
OUTDIR="$HOME/et-m0-verify/${TS}-m0b"
mkdir -p "$OUTDIR"
LOG="$OUTDIR/core.out.log"

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
echo "[m0b] outdir=$OUTDIR"
echo "[m0b] core=$CORE_BIN"
echo "[m0b] rpc-portal=127.0.0.1:$PORT"

CORE_PID=""
cleanup() {
    if [[ -n "$CORE_PID" ]] && kill -0 "$CORE_PID" 2>/dev/null; then
        echo "[m0b] cleanup: killing leftover core pid=$CORE_PID"
        kill -9 "$CORE_PID" 2>/dev/null || true
        wait "$CORE_PID" 2>/dev/null || true
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

echo "[m0b] starting: $CORE_BIN --daemon --rpc-portal 127.0.0.1:$PORT"
"$CORE_BIN" --daemon --rpc-portal "127.0.0.1:$PORT" >"$LOG" 2>&1 &
CORE_PID=$!
echo "[m0b] spawned pid=$CORE_PID (script pid=$$), waiting 10s..."
sleep 10

# 1. still alive after 10s of idling (no network config)?
if kill -0 "$CORE_PID" 2>/dev/null; then
    result "process alive after 10s idle" 0 "pid=$CORE_PID"
else
    result "process alive after 10s idle" 1 "pid=$CORE_PID exited early; see $LOG"
fi

# 2. ppid check: direct parent must be this script (no fork/detach into a new session)
ACTUAL_PPID="$(ps -o ppid= -p "$CORE_PID" 2>/dev/null | tr -d ' ')"
if [[ "$ACTUAL_PPID" == "$$" ]]; then
    result "no fork/detach (ppid==spawner pid)" 0 "core ppid=$ACTUAL_PPID == script pid=$$"
else
    result "no fork/detach (ppid==spawner pid)" 1 "core ppid=$ACTUAL_PPID != script pid=$$ (DETACHED)"
fi

# 3. no child processes (no secondary fork doing the real work)
CHILDREN="$(pgrep -P "$CORE_PID" 2>/dev/null || true)"
if [[ -z "$CHILDREN" ]]; then
    result "no child processes spawned" 0 "pgrep -P $CORE_PID => (none)"
else
    result "no child processes spawned" 1 "pgrep -P $CORE_PID => $CHILDREN"
fi

# 4. rpc port listening
if nc -z 127.0.0.1 "$PORT" 2>/dev/null; then
    result "rpc port listening" 0 "127.0.0.1:$PORT"
else
    result "rpc port listening" 1 "127.0.0.1:$PORT not reachable"
fi

# 5. graceful stop within 10s, reaped by this script
echo "[m0b] sending SIGTERM to pid=$CORE_PID"
kill -TERM "$CORE_PID" 2>/dev/null || true
STILL_ALIVE=1
for _ in $(seq 1 50); do
    if ! kill -0 "$CORE_PID" 2>/dev/null; then
        STILL_ALIVE=0
        break
    fi
    sleep 0.2
done
if [[ "$STILL_ALIVE" == "0" ]]; then
    wait "$CORE_PID" 2>/dev/null || true
    result "SIGTERM exits within 10s" 0 "pid=$CORE_PID gone, waitpid reaped"
else
    result "SIGTERM exits within 10s" 1 "pid=$CORE_PID still alive after 10s, force killing"
    kill -9 "$CORE_PID" 2>/dev/null || true
    wait "$CORE_PID" 2>/dev/null || true
fi
CORE_PID=""

echo ""
echo "[m0b] summary: PASS=$PASS FAIL=$FAIL  log=$LOG"
if [[ "$FAIL" -gt 0 ]]; then
    echo "[m0b] CONCLUSION: --daemon behavior DEVIATES from the no-fork/no-detach assumption in easytier/src/core.rs:1375 — supervisor process-tracking logic needs review."
    exit 1
fi
echo "[m0b] CONCLUSION: --daemon does NOT fork/detach and does NOT exit-on-idle; confirms the assumption behind easytier/src/core.rs:1375."
exit 0
