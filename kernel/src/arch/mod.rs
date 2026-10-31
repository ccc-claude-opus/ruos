//! The host layer: on wasm32 the kernel is a WebAssembly module in the
//! browser's kernel Worker, and the host (web/kernel_host.js) gives it the
//! console, the timer, tasks and threads, and the server connection.
mod wasm32;
pub use wasm32::*;
