fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Generated modules land in OUT_DIR; lib.rs pulls them in via `include!`.
    println!("cargo:rerun-if-changed=schema/fs.capnp");
    println!("cargo:rerun-if-changed=schema/blobs.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/fs.capnp")
        .file("schema/blobs.capnp")
        .run()?;
    Ok(())
}
