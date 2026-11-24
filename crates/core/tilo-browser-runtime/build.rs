fn main() {
    // The splitter reads the linked module's function names and relocations
    // (tools/web/wasm-split); a build that is split keeps them, and every
    // other build carries neither.
    println!("cargo::rerun-if-env-changed=TILO_WASM_SPLIT");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        // UI construction uses the same 16 MiB floor as native thread stacks
        // (.cargo/config.toml). Rust's default 1 MiB is insufficient for nested
        // application/layout constructors in an unoptimized browser bundle.
        // This reserves stack space; it does not increase the memory maximum.
        println!("cargo::rustc-link-arg-cdylib=-zstack-size=16777216");
        if std::env::var_os("TILO_WASM_SPLIT").is_some() {
            println!("cargo::rustc-link-arg-cdylib=--emit-relocs");
        }
    }
}
