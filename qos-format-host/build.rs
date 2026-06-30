// Generate Rust bindings for the Cap'n Proto router schema. The output lives in
// $OUT_DIR; src/capnp_schema.rs `include!`s the result.

fn main() {
    println!("cargo:rerun-if-changed=format-router.capnp");
    capnpc::CompilerCommand::new()
        .file("format-router.capnp")
        .src_prefix(".")
        .run()
        .expect("capnpc codegen failed (is capnp-tools installed inside distrobox?)");
}
