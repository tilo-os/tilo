//! Record the Tilo commit this CLI was built from. An app scaffolded outside a
//! checkout pins its SDK git dependency to that commit, so the templates, the
//! seeded lockfile and the SDK all come from one tested tree.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TILO_SDK_REV");
    let rev = std::env::var("TILO_SDK_REV")
        .ok()
        .filter(|rev| !rev.trim().is_empty())
        .or_else(checkout_head)
        .or_else(detached_head);
    if let Some(rev) = rev {
        println!("cargo:rustc-env=TILO_SDK_REV={}", rev.trim());
    }
    watch_head();
}

/// Rerun when the checkout moves to another commit. A build directory that
/// outlives one checkout (a developer's `target`, or a CI runner's shared
/// one) would otherwise keep the first commit it saw and scaffold apps
/// pinned to it.
fn watch_head() {
    let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return;
    };
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let out = String::from_utf8(output.stdout).ok()?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.to_owned())
    };
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    for name in ["HEAD", "packed-refs"].into_iter().chain(branch.as_deref()) {
        let Some(path) = git(&["rev-parse", "--git-path", name]) else {
            continue;
        };
        let path = std::path::Path::new(&dir).join(path);
        // A path that does not exist would rerun this script on every build.
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
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

/// The same commit without a git binary. Cargo checks `--git` sources out
/// itself, with `HEAD` naming a branch at the requested commit.
fn detached_head() -> Option<String> {
    let dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let git = dir
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|git| git.join("HEAD").is_file())?;
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    let rev = match head.strip_prefix("ref: ") {
        Some(name) => std::fs::read_to_string(git.join(name)).ok().or_else(|| {
            let packed = std::fs::read_to_string(git.join("packed-refs")).ok()?;
            packed.lines().find_map(|line| {
                let (rev, packed_name) = line.split_once(' ')?;
                (packed_name == name).then(|| rev.to_owned())
            })
        })?,
        None => head.to_owned(),
    };
    let rev = rev.trim();
    (rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())).then(|| rev.to_owned())
}
