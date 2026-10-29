//! Copies the build environment's dbus-daemon into `OUT_DIR`, where `main.rs` embeds it.
//!
//! A machine without one (some CI images) builds the crate with nothing embedded, so workspace
//! builds and checks pass there; the executable then refuses to run, and the nested-session
//! build, which runs it once while staging, fails.

use std::path::{Path, PathBuf};

fn find(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    let out = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("dbus-daemon");
    let daemon = match find("dbus-daemon") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", path.display());
            std::fs::read(&path).expect("reading dbus-daemon")
        }
        None => {
            println!(
                "cargo:warning=no dbus-daemon in this build environment: nested-session-bus embeds none"
            );
            Vec::new()
        }
    };
    std::fs::write(&out, daemon).expect("writing the embedded dbus-daemon");
}
