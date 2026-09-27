//! Record the Tilo commit this CLI was built from. An app scaffolded outside a
//! checkout pins its SDK git dependency to that commit, so the templates, the
//! seeded lockfile and the SDK all come from one tested tree.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TILO_SDK_REV");
    let rev = std::env::var("TILO_SDK_REV")
        .ok()
        .filter(|rev| !rev.trim().is_empty())
        .or_else(checkout_head);
    if let Some(rev) = rev {
        println!("cargo:rustc-env=TILO_SDK_REV={}", rev.trim());
    }
}

/// `cargo install --git` builds from a clean clone of the requested commit.
fn checkout_head() -> Option<String> {
    let dir = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .ok()?;
    let rev = String::from_utf8(output.stdout).ok()?;
    let rev = rev.trim();
    (output.status.success() && rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| rev.to_owned())
}
