fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        // Link arguments do not propagate from the runtime's rlib into this
        // final cdylib. Apply its UI stack policy to the desktop bundle too;
        // the separate, bounded compute module keeps its existing policy.
        println!("cargo::rustc-link-arg-cdylib=-zstack-size=16777216");
    }
}
