#!/usr/bin/env bash
# Build and run ruos.
#   ./run.sh riscv64 | aarch64 [qemu args...]   boot the kernel in QEMU
#   ./run.sh aarch64-el2                        same, entered at EL2 (virtualization=on)
#   ./run.sh wasm [port]                        serve web/ with ruos serve (open the URL it prints)
#   ./run.sh build riscv64|aarch64|wasm         build only
#   ./run.sh dist                               build everything into ruos/assets
#                                               (what `cargo install ruos` carries)
#   ./run.sh publish [cargo publish args]       dist, then publish the ruos crate
# NOBUILD=1 boots what is built; PODSRV=HOST:PORT gives QEMU a registry;
# SMP=N CPUs (default 4).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
OUT="$ROOT/build"
mkdir -p "$OUT"

target_of() {
    case $1 in
        riscv64) echo riscv64imac-unknown-none-elf ;;
        aarch64) echo aarch64-unknown-none-softfloat ;;
        wasm) echo wasm32-unknown-unknown ;;
        *) echo "unknown platform $1" >&2; exit 2 ;;
    esac
}

objcopy() {
    local sysroot host
    sysroot=$(rustc --print sysroot)
    host=$(rustc -vV | sed -n 's/^host: //p')
    "$sysroot/lib/rustlib/$host/bin/llvm-objcopy" "$@"
}

# User programs (boot images until the object store): native ELF, or wasm
# modules with an imported shared memory (threads = instances on it).
USER_PROGRAMS=(init blk net store console shell dock overlay wasmrt ktest peer hello snoop fault hog writer bench)
ARCHES=(riscv64 aarch64 wasm)
WASM_USER_FLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
 -C link-arg=--import-memory -C link-arg=--shared-memory -C link-arg=--max-memory=67108864 \
 -C link-arg=--export=ros_start -C link-arg=--export=ros_thread_start -C link-arg=--export=__stack_pointer"

arch_of() { [ "$1" = wasm ] && echo wasm32 || echo "$1"; }

# User programs use the hardware floating point (the kernel saves it per
# thread); the kernel itself is built without it.
user_target_of() {
    case $1 in
        riscv64) echo riscv64gc-unknown-none-elf ;;
        aarch64) echo aarch64-unknown-none ;;
        *) target_of "$1" ;;
    esac
}

build_user() {
    local p=$1 t dir pkgs=()
    t=$(user_target_of "$p")
    dir="$OUT/user-$(arch_of "$p")"
    mkdir -p "$dir"
    for x in "${USER_PROGRAMS[@]}"; do pkgs+=(-p "$x"); done
    if [ "$p" = wasm ]; then
        (cd "$ROOT" && RUSTFLAGS="$WASM_USER_FLAGS" cargo build --release --quiet -Zbuild-std=core,alloc \
            --target "$t" --target-dir target/wasm-user "${pkgs[@]}" 2>&1 | grep -v -e 'unstable feature specified' -e '^ *|$' -e 'not stably supported' -e '^$' >&2 || true)
        for x in "${USER_PROGRAMS[@]}"; do cp "$ROOT/target/wasm-user/$t/release/$x.wasm" "$dir/$x.wasm"; done
    else
        (cd "$ROOT" && cargo build --release --quiet --target "$t" "${pkgs[@]}")
        for x in "${USER_PROGRAMS[@]}"; do cp "$ROOT/target/$t/release/$x" "$dir/$x"; done
    fi
}

# Every program becomes a multi-architecture image (its Podfile.toml) in
# build/images, which is copied into build/store, the registry the server (ruos serve)
# serves (it also keeps what machines push). The kernel embeds init and the
# pack of build/images for its architecture.
build_images() {
    local a args=()
    for a in "${ARCHES[@]}"; do
        build_user "$a"
        args+=(--programs "$(arch_of "$a")=$OUT/user-$(arch_of "$a")")
    done
    (cd "$ROOT" && cargo build --release --quiet -p podimg)
    rm -rf "$OUT/images"
    local pf
    for pf in "$ROOT"/services/*/Podfile*.toml "$ROOT"/apps/*/Podfile*.toml; do
        "$ROOT/target/release/podimg" build "$pf" --store "$OUT/images" "${args[@]}" > /dev/null
    done
    mkdir -p "$OUT/store"
    cp -R "$OUT/images/." "$OUT/store/"
}

build() {
    local p=$1 t a boot
    t=$(target_of "$p")
    a=$(arch_of "$p")
    [ -n "${IMAGES_BUILT:-}" ] || { build_images; IMAGES_BUILT=1; }
    boot="$OUT/boot-$p"
    rm -rf "$boot"
    mkdir -p "$boot"
    if [ "$p" = wasm ]; then cp "$OUT/user-$a/init.wasm" "$boot/"; else cp "$OUT/user-$a/init" "$boot/"; fi
    "$ROOT/target/release/podimg" pack --store "$OUT/images" --arch "$a" --out "$boot/boot.pack" > /dev/null
    # The wasm kernel is downloaded by the page: leave out debug info there.
    local dbg=true
    [ "$p" = wasm ] && dbg=false
    (cd "$ROOT/kernel" && ROS_BOOT="$OUT/boot-$p" CARGO_PROFILE_RELEASE_DEBUG=$dbg cargo build --release --quiet --target "$t")
    local elf="$ROOT/kernel/target/$t/release/kernel"
    case $p in
        wasm) cp "$elf.wasm" "$OUT/kernel.wasm" ;;
        *) cp "$elf" "$OUT/kernel-$p.elf"; objcopy -O binary "$elf" "$OUT/kernel-$p.bin" ;;
    esac
}

build_server() {
    (cd "$ROOT" && cargo build --release --quiet -p ruos)
}

# Devices: a virtio-blk disk (DISK, default build/disk-PLATFORM.img, 16 MiB,
# kept between runs) and virtio-net on QEMU's user network, both virtio-mmio
# version 2. With PODSRV=HOST:PORT the guest reaches it as 10.0.2.100:80.
qemu() {
    local p=$1; shift
    local disk=${DISK:-$OUT/disk-$p.img}
    [ -f "$disk" ] || dd if=/dev/zero of="$disk" bs=1048576 count=16 2> /dev/null
    local net=user,id=n0
    [ -n "${PODSRV:-}" ] && net="$net,guestfwd=tcp:10.0.2.100:80-tcp:$PODSRV"
    local dev=(-smp "${SMP:-4}" -global virtio-mmio.force-legacy=false
        -drive "file=$disk,if=none,format=raw,id=hd" -device virtio-blk-device,drive=hd
        -netdev "$net" -device virtio-net-device,netdev=n0)
    case $p in
        riscv64)
            exec qemu-system-riscv64 -machine virt -m 256M -nographic -bios default \
                -kernel "$OUT/kernel-riscv64.bin" "${dev[@]}" "$@" ;;
        aarch64)
            exec qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 256M -nographic \
                -kernel "$OUT/kernel-aarch64.bin" "${dev[@]}" "$@" ;;
        aarch64-el2)
            exec qemu-system-aarch64 -machine virt,virtualization=on -cpu cortex-a72 -m 256M \
                -nographic -kernel "$OUT/kernel-aarch64.bin" "${dev[@]}" "$@" ;;
    esac
}

cmd=${1:-}
[ -n "$cmd" ] && shift
case $cmd in
    riscv64|aarch64) [ -n "${NOBUILD:-}" ] || build "$cmd"; qemu "$cmd" "$@" ;;
    aarch64-el2) [ -n "${NOBUILD:-}" ] || build aarch64; qemu aarch64-el2 "$@" ;;
    wasm)
        build wasm; build_server
        exec "$ROOT/target/release/ruos" serve --port "${1:-8780}" --web "$ROOT/web" --kernel "$OUT/kernel.wasm" --store "$OUT/store" ;;
    dist)
        # A release: the page, the kernels and the registry inside the ruos
        # binary (ruos/build.rs packs ruos/assets).
        for p in "${ARCHES[@]}"; do build "$p"; done
        a="$ROOT/ruos/assets"
        rm -rf "$a"
        mkdir -p "$a/web"
        cp "$ROOT"/web/*.html "$ROOT"/web/*.js "$ROOT"/web/*.css "$a/web/"
        cp "$OUT/kernel.wasm" "$OUT/kernel-riscv64.bin" "$OUT/kernel-aarch64.bin" "$a/"
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
    build)
        for p in "$@"; do
            build "$p"
            if [ "$p" = wasm ]; then build_server; fi
        done ;;
    *) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
