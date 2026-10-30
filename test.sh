#!/usr/bin/env bash
# Boots every platform twice on a fresh disk and checks the console:
#   boot 1: tests/m3.input is typed into the shell; the log must match every
#           tests/mN.expect (all platforms) and tests/mN.<platform>.expect;
#   boot 2: tests/boot2.input; tests/boot2.expect (+ .<platform>).
# After each boot the server is checked too (writable layers, the pushed
# image), adding `server:` / `verified` lines to the log. The QEMU machines
# reach a server (ruos serve) started here as 10.0.2.100:80 (QEMU guestfwd); the wasm
# test (web/test/boot.mjs) starts its own.
#   ./test.sh [host] [riscv64] [aarch64] [aarch64-el2] [wasm]    (default: all)
# Logs: build/test-<platform>.log, build/test-<platform>-boot2.log
set -uo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
cd "$ROOT"
mkdir -p build
TIMEOUT=${TIMEOUT:-300}
platforms=("$@")
[ ${#platforms[@]} -eq 0 ] && platforms=(host riscv64 aarch64 aarch64-el2 wasm)
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

# What the machines left on the server at URL.
post_checks() {
    local url=$1 log=$2 p c
    echo "server: writable layers $(curl -s "$url/v1/pods" | tr '\n' ' ')" >> "$log"
    for p in $(curl -s "$url/v1/pods"); do
        if c=$(curl -sf "$url/v1/pods/$p/files/data/count"); then
            echo "server: $p/files/data/count = $c" >> "$log"
        fi
    done
    target/release/podimg verify "$url" snap >> "$log" 2>&1
}

run_qemu() { # platform log input
    local p=$1 log=$2 input=$3 fifo pid status
    fifo=$(mktemp -u "${TMPDIR:-/tmp}/ruos-test.XXXXXX")
    mkfifo "$fifo"
    NOBUILD=1 PODSRV=127.0.0.1:$PODSRV_PORT timeout "$TIMEOUT" ./run.sh "$p" < "$fifo" > "$log.raw" 2>&1 &
    pid=$!
    exec 3> "$fifo"
    for _ in $(seq $((TIMEOUT * 50))); do
        grep -q '^shell: ready' "$log.raw" 2>/dev/null && break
        kill -0 $pid 2>/dev/null || break
        sleep 0.02
    done
    # The guest reaches the server on the host as 10.0.2.2:PORT.
    sed "s/@PODSRV_PORT@/$PODSRV_PORT/g" "${INPUT:-$input}" >&3
    wait $pid
    status=$?
    exec 3>&-
    rm -f "$fifo"
    tr -d '\r' < "$log.raw" > "$log"
    rm -f "$log.raw"
    return $status
}

boot() { # platform log input
    local p=$1 log=$2 input=$3
    case $p in
        riscv64|aarch64|aarch64-el2)
            run_qemu "$p" "$log" "$input" || return
            post_checks "http://127.0.0.1:$PODSRV_PORT" "$log" ;;
        wasm) timeout "$TIMEOUT" node web/test/boot.mjs --input "$input" --machine test ${DROP:+--drop-remote "$DROP"} > "$log" 2>&1 ;;
    esac
}

# Build first: building rewrites build/store, which the server serves.
for p in "${platforms[@]}"; do
    case $p in
        riscv64|aarch64|wasm) ./run.sh build "$p" || failed+=("$p") ;;
        aarch64-el2) ./run.sh build aarch64 || failed+=("$p") ;;
    esac
done
cargo build --release --quiet -p ruos -p podimg
rm -rf build/store/pods build/store/disks

PODSRV_PID=
cleanup() { [ -n "$PODSRV_PID" ] && kill "$PODSRV_PID" 2>/dev/null; }
trap cleanup EXIT
if printf '%s\n' "${platforms[@]}" | grep -Eq '^(riscv64|aarch64|aarch64-el2)$'; then
    target/release/ruos serve --port 0 --web web --store build/store > build/podsrv.log 2>&1 &
    PODSRV_PID=$!
    for _ in $(seq 600); do
        PODSRV_PORT=$(sed -n 's/.*listening on http:\/\/127.0.0.1:\([0-9]*\).*/\1/p' build/podsrv.log)
        [ -n "$PODSRV_PORT" ] && break
        sleep 0.05
    done
fi

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
    rm -f "build/disk-$p.img" build/store/disks/test.img
    # wasm: the server connection drops once, the kernel host reconnects.
    DROP=50 boot "$p" "$log" tests/m3.input
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
    if boot "$p" "$log2" tests/boot2.input && check "$log2" boot2 "$p"; then
        grep -E '^store: disk' "$log2" | sed 's/^/  boot 2: /'
    else
        echo "  boot 2 failed"
        tail -5 "$log2" | sed 's/^/  | /'
        failed+=("$p")
    fi
done

if [ ${#failed[@]} -eq 0 ]; then
    echo "all passed: ${platforms[*]}"
else
    echo "FAILED: ${failed[*]}"
    exit 1
fi
