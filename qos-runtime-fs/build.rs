fn main() {
    // Run capnpc on every schema we own. The generated module lives
    // under OUT_DIR; lib.rs pulls it in via `include!`.
    println!("cargo:rerun-if-changed=schema/fs.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/fs.capnp")
        .run()
        .expect("capnpc compilation of schema/fs.capnp failed");
}
