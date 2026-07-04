//! Build script: regenerate `share/qos-default.bpf` from the in-tree
//! filter so the on-disk blob and the runtime-compiled blob can never
//! disagree. The launcher prefers the runtime path; this is a
//! convenience for tooling that wants to inspect the policy without
//! linking seccompiler.

fn main() {
    println!("cargo:rerun-if-changed=src/seccomp.rs");
}
