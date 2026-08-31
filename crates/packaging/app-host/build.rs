// Generate Rust bindings for the Cap'n Proto control-socket schema.
// Output goes to $OUT_DIR; `src/lib.rs` `include!`s the generated module.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=schema/app-host.capnp");
    capnpc::CompilerCommand::new()
        .file("schema/app-host.capnp")
        .src_prefix("schema")
        .run()
        .map_err(|e| {
            format!("capnpc codegen failed (is the `capnp` tool installed in distrobox?): {e}")
        })?;
    Ok(())
}
