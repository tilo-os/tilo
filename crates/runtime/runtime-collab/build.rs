fn main() {
    // Generated module lands in OUT_DIR; lib.rs pulls it in via `include!`.
    println!("cargo:rerun-if-changed=schema/collab.capnp");
    if let Err(err) = capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/collab.capnp")
        .run()
    {
        // `cargo::error` fails the build with this one line. Panicking here
        // would bury the capnp diagnostic — usually a missing `capnp` binary or
        // a syntax error with a line number — under a build-script backtrace.
        println!("cargo::error=capnpc could not compile schema/collab.capnp: {err}");
    }
}
