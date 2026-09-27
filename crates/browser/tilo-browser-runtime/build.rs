//! Links the `dylink` web core as a dynamic-linking main module: app side
//! modules import its symbols, the function table and the stack pointer.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dylink = std::env::var_os("CARGO_FEATURE_DYLINK").is_some();
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "wasm32");
    if dylink && wasm {
        // `--export-all` leaves out the linker-synthesised stack pointer.
        for arg in [
            "--export-all",
            "--export-table",
            "--growable-table",
            "--export=__stack_pointer",
        ] {
            println!("cargo:rustc-cdylib-link-arg={arg}");
        }
    }
}
