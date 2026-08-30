fn main() {
    // Generated module lands in OUT_DIR; lib.rs pulls it in via `include!`.
    println!("cargo:rerun-if-changed=schema/collab.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/collab.capnp")
        .run()
        .expect("capnpc compilation of schema/collab.capnp failed");
}
