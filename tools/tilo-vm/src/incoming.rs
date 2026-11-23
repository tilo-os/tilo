//! The directory an installation leaves for a machine's first boot: one
//! prebuilt layer per component, which `apply-incoming` imports and swaps
//! through this machine's own daemon before the units that need it start.
//!
//! Format, one pair per component:
//!
//! - `<component>.tar`: the uncompressed tar `tilo-vm layer-pack` produces
//!   (what `import-layer --archive` takes), laid out by
//!   `os/scripts/install-components.py`;
//! - `<component>.json`: that layer's provenance, as
//!   `import-layer --provenance` takes it.
//!
//! `tilo-vm layer-pack --incoming <dir>` writes exactly that pair; its
//! `--component` and `--profile` choose what it packs. Components apply in
//! `os/components.toml` declaration order; a `vm`-scope component goes into
//! the `vm` account and a `session`-scope one into the `device` account,
//! which merges it VM-wide.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use tilo_vm_protocol::ComponentInfo;
use tilo_vm_protocol::manifest::{Manifest, Scope};

use crate::error::CliError;

/// Where an installation puts the layers its first boot applies.
pub const INCOMING_DIR: &str = "/var/lib/tilo-vm/incoming";
/// The manifest an installed image ships (`vmd` in os/components.toml).
pub const INSTALLED_MANIFEST: &str = "/usr/share/tilo/components.toml";

/// One component's prebuilt layer and the provenance that pins it.
#[derive(Debug)]
pub struct Entry {
    pub component: String,
    pub scope: Scope,
    pub info: ComponentInfo,
    pub archive: PathBuf,
    pub provenance: PathBuf,
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::InvalidInput(format!("incoming layers: {}", message.into()))
}

/// The SHA-256 of a layer archive's exact bytes, which `import-layer` checks
/// before it contacts the daemon.
pub fn archive_sha256(path: &Path) -> Result<String, CliError> {
    let mut file =
        std::fs::File::open(path).map_err(CliError::io(format!("open {}", path.display())))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 64 << 10];
    loop {
        let count = std::io::Read::read(&mut file, &mut buffer)
            .map_err(CliError::io(format!("read {}", path.display())))?;
        if count == 0 {
            break;
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| invalid("an archive read exceeded its buffer"))?;
        hash.update(chunk);
    }
    Ok(hex::encode(hash.finalize()))
}

/// The names in `os/components.toml` order: its `[[component]]` array keeps
/// the order it was written in, while [`Manifest`] is keyed by name.
fn declared_order(text: &str) -> Vec<String> {
    let Ok(root) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    match root.get("component") {
        Some(toml::Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| {
                entry
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The components a directory holds, in manifest declaration order.
///
/// Anything else in the directory is a refusal: the boot unit fails rather
/// than skipping a layer a packer meant to apply.
/// Take one file of the components directory into what it names: a `<component>.tar` into
/// `archives`, a `<component>.json` into `provenances`, and nothing else.
fn take_entry(
    entry: &std::fs::DirEntry,
    archives: &mut BTreeMap<String, PathBuf>,
    provenances: &mut BTreeSet<String>,
) -> Result<(), CliError> {
    let name = entry.file_name().to_string_lossy().into_owned();
    if !entry
        .file_type()
        .map_err(CliError::io(format!("inspect {}", entry.path().display())))?
        .is_file()
    {
        return Err(invalid(format!(
            "{} is not a file; only <component>.tar and <component>.json belong here",
            entry.path().display()
        )));
    }
    if let Some(component) = name.strip_suffix(".tar") {
        if component.is_empty()
            || archives
                .insert(component.to_owned(), entry.path())
                .is_some()
        {
            return Err(invalid(format!("duplicate or empty layer name {name}")));
        }
    } else if let Some(component) = name.strip_suffix(".json") {
        if component.is_empty() || !provenances.insert(component.to_owned()) {
            return Err(invalid(format!(
                "duplicate or empty provenance name {name}"
            )));
        }
    } else {
        return Err(invalid(format!(
            "{} is neither <component>.tar nor <component>.json",
            entry.path().display()
        )));
    }
    Ok(())
}

pub fn entries(dir: &Path, manifest_path: &Path) -> Result<Vec<Entry>, CliError> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(CliError::io(format!("read {}", manifest_path.display())))?;
    let manifest = Manifest::parse(&text).map_err(|detail| CliError::Config {
        path: manifest_path.display().to_string(),
        detail,
    })?;
    if !dir.is_dir() {
        return Err(invalid(format!("{} is not a directory", dir.display())));
    }
    let mut archives: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut provenances: BTreeSet<String> = BTreeSet::new();
    let listing =
        std::fs::read_dir(dir).map_err(CliError::io(format!("list {}", dir.display())))?;
    for entry in listing {
        let entry = entry.map_err(CliError::io(format!("list {}", dir.display())))?;
        take_entry(&entry, &mut archives, &mut provenances)?;
    }
    for component in archives.keys() {
        if !provenances.contains(component) {
            return Err(invalid(format!(
                "{component}.tar has no {component}.json provenance beside it"
            )));
        }
    }
    for component in &provenances {
        if !archives.contains_key(component) {
            return Err(invalid(format!(
                "{component}.json has no {component}.tar layer beside it"
            )));
        }
    }
    let order: Vec<String> = {
        let mut order: Vec<String> = declared_order(&text)
            .into_iter()
            .filter(|name| archives.contains_key(name))
            .collect();
        // A manifest given as a table (`[components.<name>]`) declares no
        // order; the rest follow by name.
        for name in archives.keys() {
            if !order.contains(name) {
                order.push(name.clone());
            }
        }
        order
    };
    let mut entries = Vec::with_capacity(order.len());
    for component in order {
        let entry = manifest.components.get(&component).ok_or_else(|| {
            invalid(format!(
                "{component}.tar names a component {} does not declare",
                manifest_path.display()
            ))
        })?;
        if entry.boot {
            return Err(invalid(format!(
                "'{component}' takes effect only on a cold boot; install it through its scenario, not the incoming directory"
            )));
        }
        if entry.info.scope == Scope::App {
            return Err(invalid(format!(
                "'{component}' is an app; incoming layers carry components"
            )));
        }
        let archive = archives
            .remove(&component)
            .ok_or_else(|| invalid(format!("{component}.tar is missing")))?;
        let provenance = dir.join(format!("{component}.json"));
        entries.push(Entry {
            component,
            scope: entry.info.scope,
            info: entry.info.clone(),
            archive,
            provenance,
        });
    }
    Ok(entries)
}

/// Remove a layer this machine has imported and applied, so a retry does not
/// import it twice. A failure here is loud: the boot unit reports it.
pub fn remove_applied(entry: &Entry) -> Result<(), CliError> {
    for path in [&entry.archive, &entry.provenance] {
        std::fs::remove_file(path)
            .map_err(CliError::io(format!("remove applied {}", path.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(order: &[(&str, &str)]) -> String {
        use std::fmt::Write as _;
        let mut text = String::new();
        for (name, scope) in order {
            write!(
                text,
                "[[component]]\nname = \"{name}\"\nkind = \"userspace\"\nscope = \"{scope}\"\npackages = [\"{name}\"]\nbins = [\"{name}\"]\nfiles = [{{ from = \"{name}\", to = \"/usr/bin/{name}\", mode = \"0755\" }}]\n\n"
            )
            .unwrap();
        }
        text
    }

    fn plant(dir: &Path, component: &str) {
        std::fs::write(dir.join(format!("{component}.tar")), b"tar").unwrap();
        std::fs::write(dir.join(format!("{component}.json")), b"{}").unwrap();
    }

    #[test]
    fn a_directory_applies_in_manifest_order_and_refuses_what_it_cannot() {
        let dir = tempfile::tempdir().unwrap();
        let incoming = dir.path().join("incoming");
        std::fs::create_dir(&incoming).unwrap();
        let manifest_path = dir.path().join("components.toml");
        // Alpha sorts first but is declared second: order comes from the
        // manifest, not from the directory listing.
        std::fs::write(
            &manifest_path,
            manifest(&[("zulu", "session"), ("alpha", "vm")]),
        )
        .unwrap();
        plant(&incoming, "alpha");
        plant(&incoming, "zulu");
        let planned = entries(&incoming, &manifest_path).unwrap();
        assert_eq!(
            planned
                .iter()
                .map(|entry| (entry.component.as_str(), entry.scope))
                .collect::<Vec<_>>(),
            [("zulu", Scope::Session), ("alpha", Scope::Vm)]
        );

        std::fs::remove_file(incoming.join("zulu.json")).unwrap();
        let error = entries(&incoming, &manifest_path).unwrap_err();
        assert!(
            error.to_string().contains("zulu.tar has no zulu.json"),
            "{error}"
        );

        std::fs::write(incoming.join("zulu.json"), b"{}").unwrap();
        std::fs::write(incoming.join("stray.txt"), b"x").unwrap();
        let error = entries(&incoming, &manifest_path).unwrap_err();
        assert!(error.to_string().contains("neither"), "{error}");

        std::fs::remove_file(incoming.join("stray.txt")).unwrap();
        std::fs::write(incoming.join("ghost.tar"), b"tar").unwrap();
        std::fs::write(incoming.join("ghost.json"), b"{}").unwrap();
        let error = entries(&incoming, &manifest_path).unwrap_err();
        assert!(
            error.to_string().contains("ghost.tar names a component"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_directory_has_nothing_to_apply() {
        let dir = tempfile::tempdir().unwrap();
        let incoming = dir.path().join("incoming");
        std::fs::create_dir(&incoming).unwrap();
        let manifest_path = dir.path().join("components.toml");
        std::fs::write(&manifest_path, manifest(&[("alpha", "vm")])).unwrap();
        assert!(entries(&incoming, &manifest_path).unwrap().is_empty());
    }
}
