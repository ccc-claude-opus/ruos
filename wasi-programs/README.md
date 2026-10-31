# WASI programs

Ordinary programs for `wasm32-wasip1` (Rust with std, or other crates
from crates.io), not written for ruos: ruos runs them through its WASI
layer (`services/wasi`). `run.sh` builds them and packs them into the
`root` image's `/bin`.
