fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Generated module lands in OUT_DIR; lib.rs pulls it in via `include!`.
    println!("cargo:rerun-if-changed=schema/fs.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/fs.capnp")
        .run()?;
    Ok(())
}
