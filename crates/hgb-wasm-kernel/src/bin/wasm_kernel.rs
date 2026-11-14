//! # Hagibis WebAssembly Microkernel Native Runner (`wasm_kernel` alias)
#[path = "../main.rs"]
mod main_impl;

fn main() {
    main_impl::main();
}
