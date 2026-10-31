#!/usr/bin/env bash
# Tests ruos: the host crates (cargo test), then the OS (wasm) booted twice
# on a fresh disk by web/test/boot.mjs, which starts its own server:
#   boot 1: tests/m3.input is typed into the shell; the log must match every
#           tests/mN.expect (and tests/mN.wasm.expect), teach.expect, shell.expect;
#   boot 2: tests/boot2.input; tests/boot2.expect.
# Then two machines on one server (cluster_test).
#   ./test.sh [host] [wasm]    (default: both)
# Logs: build/test-<target>.log, build/test-wasm-boot2.log
set -uo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
cd "$ROOT"
mkdir -p build
TIMEOUT=${TIMEOUT:-300}
platforms=("$@")
[ ${#platforms[@]} -eq 0 ] && platforms=(host wasm)
failed=()

# check LOG PREFIX PLATFORM: the patterns of tests/PREFIX*.expect.
check() {
    local log=$1 prefix=$2 p=$3 ok=1 pat f
    for f in tests/$prefix.expect tests/$prefix.$p.expect; do
        [ -f "$f" ] || continue
        while IFS= read -r pat; do
            [ -z "$pat" ] && continue
            if ! grep -Eq -- "$pat" "$log"; then
                echo "  missing ($(basename "$f")): $pat"
                ok=0
            fi
        done < "$f"
    done
    [ $ok = 1 ]
}

check_boot1() {
    local log=$1 p=$2 ok=1 f
    for f in tests/m[0-9]*.expect tests/teach.expect tests/shell.expect; do
        case $(basename "$f" .expect) in *.*) continue ;; esac
        check "$log" "$(basename "$f" .expect)" "$p" || ok=0
    done
    if grep -Eq '^panic:|FAILED' "$log"; then
        grep -E '^panic:|FAILED' "$log" | sed 's/^/  /'
        ok=0
    fi
    [ $ok = 1 ]
}

# Two machines on one server are one cluster: node-b comes up first and
# leaves after a while; test schedules pods on both, then sees node-b's
# pods made again on itself (tests/cluster*.input, tests/cluster.expect).
cluster_test() {
    local log=build/test-wasm-cluster.log logb=build/test-wasm-node-b.log srv port b ok=1
    target/release/ruos serve --port 0 --web web --kernel build/kernel.wasm --store build/store > build/cluster-srv.log 2>&1 &
    srv=$!
    for _ in $(seq 600); do
        port=$(sed -n 's/.*listening on http:\/\/127.0.0.1:\([0-9]*\).*/\1/p' build/cluster-srv.log)
        [ -n "$port" ] && break
        sleep 0.05
    done
    rm -f build/store/disks/node-b.img
    timeout "$TIMEOUT" node web/test/boot.mjs --server "http://127.0.0.1:$port" --input tests/cluster-b.input --machine node-b > "$logb" 2>&1 &
    b=$!
    for _ in $(seq 1200); do
        grep -q '^shell: ready' "$logb" 2>/dev/null && break
        sleep 0.05
    done
    timeout "$TIMEOUT" node web/test/boot.mjs --server "http://127.0.0.1:$port" --input tests/cluster.input --machine test > "$log" 2>&1 || ok=0
    wait $b || ok=0
    kill $srv 2>/dev/null
    wait $srv 2>/dev/null
    check "$log" cluster wasm || ok=0
    if grep -Eq '^panic:|FAILED' "$log" "$logb"; then
        grep -E '^panic:|FAILED' "$log" "$logb" | sed 's/^/  /'
        ok=0
    fi
    if [ $ok = 1 ]; then
        echo "  cluster: $(grep -c . tests/cluster.expect) checks on test + node-b"
    else
        echo "  cluster failed (build/test-wasm-cluster.log, build/test-wasm-node-b.log)"
    fi
    [ $ok = 1 ]
}

boot() { # log input
    timeout "$TIMEOUT" node web/test/boot.mjs --input "$2" --machine test ${DROP:+--drop-remote "$DROP"} > "$1" 2>&1
}

# Build first: building rewrites build/store, which the server serves.
if printf '%s\n' "${platforms[@]}" | grep -qx wasm; then
    ./run.sh build || failed+=(wasm)
fi
cargo build --release --quiet -p ruos -p podimg
rm -rf build/store/pods build/store/disks build/store/cluster

for p in "${platforms[@]}"; do
    printf '%s\n' "${failed[@]:-}" | grep -qx "$p" && continue
    log="build/test-$p.log"
    echo "== $p"
    if [ "$p" = host ]; then
        if cargo test --release --quiet > "$log" 2>&1; then
            grep -E '^test result' "$log" | sed 's/^/  /'
        else
            cat "$log"; failed+=("$p")
        fi
        continue
    fi
    # The server connection drops once (after 50 answers): the kernel host reconnects.
    DROP=50 boot "$log" tests/m3.input
    status=$?
    if [ $status -ne 0 ]; then
        echo "  boot 1: exit status $status"
        tail -5 "$log" | sed 's/^/  | /'
        failed+=("$p")
        continue
    fi
    check_boot1 "$log" "$p" || { failed+=("$p"); continue; }
    grep -E '^M[0-9]+ OK' "$log" | tr '\n' ' ' | sed 's/^/  boot 1: /; s/ $/\n/'
    log2="build/test-$p-boot2.log"
    if boot "$log2" tests/boot2.input && check "$log2" boot2 "$p"; then
        grep -E '^store: disk' "$log2" | sed 's/^/  boot 2: /'
    else
        echo "  boot 2 failed"
        tail -5 "$log2" | sed 's/^/  | /'
        failed+=("$p")
        continue
    fi
    cluster_test || failed+=("$p")
done

if [ ${#failed[@]} -eq 0 ]; then
    echo "all passed: ${platforms[*]}"
else
    echo "FAILED: ${failed[*]}"
    exit 1
fi
