#!/usr/bin/env bash
# Build and run ruos (the OS in the browser; the server is ruos serve).
#   ./run.sh wasm [port]                        build, then serve the OS (open the URL it prints)
#   ./run.sh build                              build only (build/kernel.wasm, build/images, build/extra, build/store)
#   ./run.sh dist                               build everything into ruos/assets
#                                               (what `cargo install ruos` carries) and
#                                               build/release (upload to the release page)
#   ./run.sh publish [-y] [--dry-run]           = ./publish.sh (the release page, then crates.io)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
OUT="$ROOT/build"
mkdir -p "$OUT"

# User programs: wasm modules with an imported shared memory (threads =
# instances on it). Each becomes an image (its Podfile.toml).
USER_PROGRAMS=(init store console shell dock kube overlay wasi local ktest peer hello snoop fault hog writer bench worker echo greet site)
# The example web sites: binaries of one package (apps/webapps).
WEB_APPS=(guestbook todo wiki short weather)
TARGET=wasm32-unknown-unknown
WASM_USER_FLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
 -C link-arg=--import-memory -C link-arg=--shared-memory -C link-arg=--max-memory=67108864 \
 -C link-arg=--export=ros_start -C link-arg=--export=ros_thread_start -C link-arg=--export=__stack_pointer"

build_user() {
    local dir="$OUT/user-wasm32" pkgs=() x
    mkdir -p "$dir"
    for x in "${USER_PROGRAMS[@]}"; do pkgs+=(-p "$x"); done
    pkgs+=(-p webapps)
    (cd "$ROOT" && RUSTFLAGS="$WASM_USER_FLAGS" cargo build --release --quiet -Zbuild-std=core,alloc \
        --target "$TARGET" --target-dir target/wasm-user "${pkgs[@]}" 2>&1 | grep -v -e 'unstable feature specified' -e '^ *|$' -e 'not stably supported' -e '^$' >&2 || true)
    for x in "${USER_PROGRAMS[@]}" "${WEB_APPS[@]}"; do cp "$ROOT/target/wasm-user/$TARGET/release/$x.wasm" "$dir/$x.wasm"; done
}

# WASI programs (wasm32-wasip1, not written for ruos; services/wasi runs
# them): the crates in wasi-programs/ go with the user programs (and into
# the root image's /bin); uutils coreutils (crates.io, `cargo install`
# caches it in target/wasi-tools) becomes its own image, only in the
# registry (build/store): it is too big for the boot pack. Crates with C
# in them (sqlited: SQLite) need wasi-sdk: WASI_SDK, by default
# ~/opt/wasi-sdk-34.0-arm64-macos (github.com/WebAssembly/wasi-sdk releases).
WASI_SDK=${WASI_SDK:-$HOME/opt/wasi-sdk-34.0-arm64-macos}
# SQLite's amalgamation (sqlite.org/download.html: shell.c, sqlite3.c) for
# the sqlite3 shell.
SQLITE_SRC=${SQLITE_SRC:-$HOME/opt/sqlite-src/sqlite-amalgamation-3540100}
build_wasi() {
    local d n
    for d in "$ROOT"/wasi-programs/*/Cargo.toml; do
        n=$(basename "$(dirname "$d")")
        if [ "$n" = sqlited ]; then
            if [ ! -x "$WASI_SDK/bin/clang" ]; then
                echo "run.sh: no wasi-sdk at $WASI_SDK: sqlited (local SQLite) not built" >&2
                continue
            fi
            # SQLite needs wasi-libc's emulated mmap, getpid, signals and clocks.
            local lib="$WASI_SDK/share/wasi-sysroot/lib/wasm32-wasip1" e flags=""
            for e in mman getpid signal process-clocks; do flags+=" -C link-arg=$lib/libwasi-emulated-$e.a"; done
            CC_wasm32_wasip1="$WASI_SDK/bin/clang" AR_wasm32_wasip1="$WASI_SDK/bin/llvm-ar" \
                CFLAGS_wasm32_wasip1="--sysroot=$WASI_SDK/share/wasi-sysroot" RUSTFLAGS="$flags" \
                cargo build --release --quiet --target wasm32-wasip1 --manifest-path "$d"
        else
            cargo build --release --quiet --target wasm32-wasip1 --manifest-path "$d"
        fi
        cp "$(dirname "$d")/target/wasm32-wasip1/release/$n.wasm" "$OUT/user-wasm32/$n.wasm"
    done
    if [ -x "$WASI_SDK/bin/clang" ] && [ -f "$SQLITE_SRC/shell.c" ]; then
        local out="$OUT/user-wasm32/sqlite3.wasm"
        if [ ! -f "$out" ] || [ "$SQLITE_SRC/sqlite3.c" -nt "$out" ] || [ "$ROOT/wasi-programs/sqlite3/nosystem.c" -nt "$out" ]; then
            "$WASI_SDK/bin/clang" --target=wasm32-wasip1 --sysroot="$WASI_SDK/share/wasi-sysroot" -Os \
                -DSQLITE_THREADSAFE=0 -DSQLITE_OMIT_LOAD_EXTENSION -DSQLITE_OMIT_POPEN -DSQLITE_OMIT_WAL -DHAVE_LOCALTIME_R \
                -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_GETPID -D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS \
                "$SQLITE_SRC/shell.c" "$SQLITE_SRC/sqlite3.c" "$ROOT/wasi-programs/sqlite3/nosystem.c" \
                -lwasi-emulated-mman -lwasi-emulated-getpid -lwasi-emulated-signal -lwasi-emulated-process-clocks -o "$out"
        fi
    else
        echo "run.sh: no wasi-sdk or no SQLite source at $SQLITE_SRC: the sqlite3 shell not built" >&2
    fi
    cargo install --quiet --root "$ROOT/target/wasi-tools" --target-dir "$ROOT/target/wasi-tools-build" \
        --target wasm32-wasip1 coreutils@0.12.0 --no-default-features --features feat_wasm 2> /dev/null || true
    # Only the installed binary is needed again (cargo install skips what is
    # installed): its build directory is about 500 MB.
    rm -rf "$ROOT/target/wasi-tools-build"
    mkdir -p "$OUT/wasi-tools"
    cp "$ROOT/target/wasi-tools/bin/coreutils.wasm" "$OUT/wasi-tools/coreutils.wasm" 2> /dev/null || true
}

# Every program becomes an image (its Podfile.toml) in build/images, which
# is copied into build/store, the registry the server (ruos serve) serves
# (it also keeps what machines push). The kernel embeds init and the pack
# of build/images.
build_images() {
    build_user
    build_wasi
    (cd "$ROOT" && cargo build --release --quiet -p podimg)
    rm -rf "$OUT/images" "$OUT/extra" "$OUT/upstream"
    local pf
    for pf in "$ROOT"/services/*/Podfile*.toml "$ROOT"/apps/*/Podfile*.toml; do
        case $pf in */apps/webapps/*) continue ;; esac
        "$ROOT/target/release/podimg" build "$pf" --store "$OUT/images" --programs "wasm32=$OUT/user-wasm32" > /dev/null
    done
    # Images only in the registry (build/extra), not in the boot pack: a
    # machine pulls them when first used.
    for pf in "$ROOT"/apps/webapps/Podfile*.toml; do
        "$ROOT/target/release/podimg" build "$pf" --store "$OUT/extra" --programs "wasm32=$OUT/user-wasm32" > /dev/null
    done
    # Too big for the crate: a release carries their refs, and the server
    # fetches their objects from the release page when first asked
    # (ruos/src/upstream.rs).
    for x in sqlited sqlite3; do
        if [ -f "$OUT/user-wasm32/$x.wasm" ]; then
            "$ROOT/target/release/podimg" build "$ROOT/wasi-programs/$x/Podfile.toml" --store "$OUT/upstream" --programs "wasm32=$OUT/user-wasm32" > /dev/null
        fi
    done
    if [ -f "$OUT/wasi-tools/coreutils.wasm" ]; then
        "$ROOT/target/release/podimg" build "$ROOT/wasi-programs/coreutils/Podfile.toml" --store "$OUT/extra" --programs "wasm32=$OUT/wasi-tools" > /dev/null
    fi
    mkdir -p "$OUT/store"
    mkdir -p "$OUT/upstream"
    cp -R "$OUT/images/." "$OUT/extra/." "$OUT/upstream/." "$OUT/store/"
}

build() {
    build_images
    local boot="$OUT/boot-wasm"
    rm -rf "$boot"
    mkdir -p "$boot"
    cp "$OUT/user-wasm32/init.wasm" "$boot/"
    "$ROOT/target/release/podimg" pack --store "$OUT/images" --arch wasm32 --out "$boot/boot.pack" > /dev/null
    # The kernel is downloaded by the page: no debug info.
    (cd "$ROOT/kernel" && ROS_BOOT="$boot" CARGO_PROFILE_RELEASE_DEBUG=false cargo build --release --quiet --target "$TARGET")
    cp "$ROOT/kernel/target/$TARGET/release/kernel.wasm" "$OUT/kernel.wasm"
    (cd "$ROOT" && cargo build --release --quiet -p ruos)
}

cmd=${1:-}
[ -n "$cmd" ] && shift
case $cmd in
    wasm)
        build
        exec "$ROOT/target/release/ruos" serve --port "${1:-8780}" --web "$ROOT/web" --kernel "$OUT/kernel.wasm" --store "$OUT/store" ;;
    build) build ;;
    dist)
        # A release: the page, the kernel and the registry inside the ruos
        # binary (ruos/build.rs packs ruos/assets).
        build
        a="$ROOT/ruos/assets"
        rm -rf "$a"
        mkdir -p "$a/web"
        cp "$ROOT"/web/*.html "$ROOT"/web/*.js "$ROOT"/web/*.css "$a/web/"
        cp "$OUT/kernel.wasm" "$a/"
        cp -R "$OUT/images" "$a/store"
        cp -R "$OUT/extra/." "$a/store/"
        # The upstream images: refs here, objects on the release page.
        rel="$OUT/release"
        rm -rf "$rel"
        mkdir -p "$rel"
        [ -d "$OUT/upstream/refs" ] && cp -R "$OUT/upstream/refs/." "$a/store/refs/"
        : > "$a/store/upstream"
        for o in "$OUT"/upstream/objects/*; do
            [ -f "$o" ] || continue
            [ -f "$a/store/objects/$(basename "$o")" ] && continue
            basename "$o" >> "$a/store/upstream"
            cp "$o" "$rel/"
        done
        du -sh "$a"
        echo "build/release: $(ls "$rel" | wc -l | tr -d ' ') objects for the release page (./publish.sh puts them there)" ;;
    publish) exec "$ROOT/publish.sh" "$@" ;;
    *) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
