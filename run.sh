#!/usr/bin/env bash
# Build and run ruos (the OS in the browser; the server is ruos serve).
#   ./run.sh wasm [port]                        build, then serve the OS (open the URL it prints)
#   ./run.sh build                              build only (build/kernel.wasm, build/images, build/store)
#   ./run.sh dist                               build everything into ruos/assets
#                                               (what `cargo install ruos` carries)
#   ./run.sh publish [cargo publish args]       dist, then publish the ruos crate
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
OUT="$ROOT/build"
mkdir -p "$OUT"

# User programs: wasm modules with an imported shared memory (threads =
# instances on it). Each becomes an image (its Podfile.toml).
USER_PROGRAMS=(init store console shell dock kube overlay ktest peer hello snoop fault hog writer bench worker echo greet site)
TARGET=wasm32-unknown-unknown
WASM_USER_FLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
 -C link-arg=--import-memory -C link-arg=--shared-memory -C link-arg=--max-memory=67108864 \
 -C link-arg=--export=ros_start -C link-arg=--export=ros_thread_start -C link-arg=--export=__stack_pointer"

build_user() {
    local dir="$OUT/user-wasm32" pkgs=() x
    mkdir -p "$dir"
    for x in "${USER_PROGRAMS[@]}"; do pkgs+=(-p "$x"); done
    (cd "$ROOT" && RUSTFLAGS="$WASM_USER_FLAGS" cargo build --release --quiet -Zbuild-std=core,alloc \
        --target "$TARGET" --target-dir target/wasm-user "${pkgs[@]}" 2>&1 | grep -v -e 'unstable feature specified' -e '^ *|$' -e 'not stably supported' -e '^$' >&2 || true)
    for x in "${USER_PROGRAMS[@]}"; do cp "$ROOT/target/wasm-user/$TARGET/release/$x.wasm" "$dir/$x.wasm"; done
}

# Every program becomes an image (its Podfile.toml) in build/images, which
# is copied into build/store, the registry the server (ruos serve) serves
# (it also keeps what machines push). The kernel embeds init and the pack
# of build/images.
build_images() {
    build_user
    (cd "$ROOT" && cargo build --release --quiet -p podimg)
    rm -rf "$OUT/images"
    local pf
    for pf in "$ROOT"/services/*/Podfile*.toml "$ROOT"/apps/*/Podfile*.toml; do
        "$ROOT/target/release/podimg" build "$pf" --store "$OUT/images" --programs "wasm32=$OUT/user-wasm32" > /dev/null
    done
    mkdir -p "$OUT/store"
    cp -R "$OUT/images/." "$OUT/store/"
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
        du -sh "$a" ;;
    publish)
        # The assets are build output, ignored by git, so cargo calls the
        # tree dirty: check that everything else is committed first.
        if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
            echo "run.sh: commit your changes first:" >&2
            git -C "$ROOT" status --short >&2
            exit 1
        fi
        "$0" dist
        (cd "$ROOT/ruos" && cargo publish --allow-dirty "$@") ;;
    *) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
