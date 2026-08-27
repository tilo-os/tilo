//! Regenerates `share/qos-default.bpf` from the in-tree filter so the
//! on-disk and runtime-compiled blobs can never disagree — a convenience
//! for tooling that wants to inspect the policy without linking seccompiler.

fn main() {
    println!("cargo:rerun-if-changed=src/seccomp.rs");
}
