//! What the policy reads of every app's committed catalogue entry (`fleet.json`, beside a native
//! app's manifest or a tilolang package's `app.tl`): whose its windows are and which of its kinds
//! of record are sealed. The rest of the entry is `fleet_catalogue`'s to read.
//!
//! The entries this reads are the ones `apps/fleet-index.json` names (`tilolang describe --fleet
//! --write`), and those files are all it watches. Watching the app directories instead would rerun
//! this script for any file in any app, so every edit to an app would rebuild everything above it;
//! the index is what changes when an app is added or removed.

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// The names of every app's entry, in the order the fleet reads them, from the root of the tree.
const FLEET_INDEX: &str = "apps/fleet-index.json";

/// The fields of an app's entry the policy reads.
#[derive(Deserialize)]
struct Entry {
    id: String,
    windows_personal: bool,
    #[serde(default)]
    sealed: Vec<String>,
    /// Whether search and the Store offer it.
    listed: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut entries = Vec::new();
    // A packaged crate carries no apps and compiles none.
    if root.join("apps").is_dir() {
        let index = root.join(FLEET_INDEX);
        println!("cargo::rerun-if-changed={}", index.display());
        let text = fs::read_to_string(&index).map_err(|error| {
            format!(
                "{}: {error}; write it with `tilolang describe --fleet --write`",
                index.display()
            )
        })?;
        let paths: Vec<String> =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", index.display()))?;
        for path in paths {
            let file = root.join(path);
            println!("cargo::rerun-if-changed={}", file.display());
            let text = fs::read_to_string(&file)
                .map_err(|error| format!("{}: {error}", file.display()))?;
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
    out.push_str("];\npub(super) const SEALED_KINDS: &[(&str, &str)] = &[");
    for entry in &entries {
        for kind in &entry.sealed {
            write!(out, "({:?}, {kind:?}), ", entry.id)?;
        }
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR")?).join("kinds.rs"), out)?;
    Ok(())
}
