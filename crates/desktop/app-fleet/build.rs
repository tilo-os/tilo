//! What the policy reads of every app's committed catalogue entry (`fleet.json`, beside a native
//! app's manifest or a tilolang package's `app.tl`): whose its windows are and which of its kinds
//! of record are private or sealed. The rest of the entry is `fleet_catalogue`'s to read.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The fields of an app's entry the policy reads.
#[derive(Deserialize)]
struct Entry {
    id: String,
    windows_personal: bool,
    #[serde(default)]
    private: Vec<String>,
    #[serde(default)]
    sealed: Vec<String>,
    /// Whether search and the Store offer it.
    listed: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let apps_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    // The native apps first, in the order their ids sort, then the tilolang packages; a packaged
    // crate carries no apps and compiles none.
    let native = dirs(&apps_dir)
        .into_iter()
        .filter(|dir| dir.join("tilo-app.toml").is_file());
    let mut entries = Vec::new();
    for dir in native.chain(dirs(&apps_dir.join("tl"))) {
        let file = dir.join("fleet.json");
        if file.is_file() {
            println!("cargo::rerun-if-changed={}", file.display());
            let text = fs::read_to_string(&file)?;
            let entry: Entry = serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", file.display()))?;
            entries.push(entry);
        }
    }
    let listed: Vec<&Entry> = entries.iter().filter(|entry| entry.listed).collect();
    let mut out = String::from("// Gathered by app-fleet's build script. Not edited by hand.\n\n");
    out.push_str("pub(super) const OWN_WINDOWS: &[&str] = &[");
    for entry in listed.iter().filter(|entry| entry.windows_personal) {
        write!(out, "{:?}, ", entry.id)?;
    }
    out.push_str("];\npub(super) const PRIVATE_KINDS: &[(&str, &str)] = &[");
    for entry in &listed {
        for kind in &entry.private {
            write!(out, "({:?}, {kind:?}), ", entry.id)?;
        }
    }
    out.push_str("];\npub(super) const SEALED_KINDS: &[(&str, &str)] = &[");
    for entry in &entries {
        for kind in &entry.sealed {
            write!(out, "({:?}, {kind:?}), ", entry.id)?;
        }
    }
    out.push_str("];\n");
    fs::write(Path::new(&std::env::var("OUT_DIR")?).join("kinds.rs"), out)?;
    Ok(())
}

/// The directories under `dir`, in the order files sort, each watched for an app added or
/// removed; none when `dir` does not exist.
fn dirs(dir: &Path) -> Vec<PathBuf> {
    println!("cargo::rerun-if-changed={}", dir.display());
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in &dirs {
        println!("cargo::rerun-if-changed={}", dir.display());
    }
    dirs
}
