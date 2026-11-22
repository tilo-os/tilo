//! The boot step `apply-incoming`: the prebuilt component layers an installation left for the
//! machine's first boot, imported and swapped on the device's own state before the units that
//! need them start. It runs in its boot unit as root; no caller is involved.
//!
//! Format, one pair per component:
//!
//! - `<component>.tar`: the uncompressed tar `tilo-vm layer-pack --incoming` writes, laid out by
//!   `os/scripts/install-components.py`;
//! - `<component>.json`: that layer's provenance.
//!
//! Components apply in `os/components.toml` declaration order; a `vm`-scope component goes into
//! the `vm` account and a `session`-scope one into the `device` account, which merges it VM-wide.
//! Each pair is removed once it applied, so a retry after a failure resumes where it stopped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use tilo_device_protocol::manifest::{Manifest, Scope};
use tilo_device_protocol::{
    ComponentInfo, DEVICE_ACCOUNT, LayerImport, LayerImportResult, LayerSource, Provenance, Swap,
    VM_ACCOUNT,
};

use crate::daemon::Daemon;
use crate::error::DaemonError;

/// Where an installation puts the layers its first boot applies.
pub const INCOMING_DIR: &str = "/var/lib/tilo-vm/incoming";

/// One component's prebuilt layer and the provenance that pins it.
#[derive(Debug)]
pub struct Entry {
    pub component: String,
    pub scope: Scope,
    pub info: ComponentInfo,
    pub archive: PathBuf,
    pub provenance: PathBuf,
}

/// One component applied to an account.
#[derive(Debug, PartialEq, Eq)]
pub struct Applied {
    pub component: String,
    pub layer: String,
    pub account: &'static str,
}

fn invalid(message: impl Into<String>) -> DaemonError {
    DaemonError::InvalidLayer(format!("incoming layers: {}", message.into()))
}

/// The names in `os/components.toml` order: its `[[component]]` array keeps the order it was
/// written in, while [`Manifest`] is keyed by name.
fn declared_order(text: &str) -> Vec<String> {
    let Ok(root) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    match root.get("component") {
        Some(toml::Value::Array(entries)) => entries
            .iter()
            .filter_map(|entry| entry.get("name").and_then(toml::Value::as_str))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Take one file of the directory into what it names: a `<component>.tar` into `archives`, a
/// `<component>.json` into `provenances`. Anything else is a refusal: the boot unit fails rather
/// than skipping a layer a packer meant to apply.
fn take_entry(
    entry: &fs::DirEntry,
    archives: &mut BTreeMap<String, PathBuf>,
    provenances: &mut BTreeSet<String>,
) -> Result<(), DaemonError> {
    let name = entry.file_name().to_string_lossy().into_owned();
    let file_type = entry.file_type().map_err(DaemonError::io(format!(
        "inspect {}",
        entry.path().display()
    )))?;
    if !file_type.is_file() {
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

/// The components `dir` holds, in the declaration order of the manifest at `manifest_path`.
///
/// # Errors
/// An unreadable manifest or directory, a pair missing its other half, a stray file, or a
/// component the manifest does not declare, takes only on a cold boot, or is an app.
pub fn entries(dir: &Path, manifest_path: &Path) -> Result<Vec<Entry>, DaemonError> {
    let text = fs::read_to_string(manifest_path)
        .map_err(DaemonError::io(format!("read {}", manifest_path.display())))?;
    let manifest = Manifest::parse(&text)
        .map_err(|detail| invalid(format!("{}: {detail}", manifest_path.display())))?;
    let mut archives = BTreeMap::new();
    let mut provenances = BTreeSet::new();
    let listing = fs::read_dir(dir).map_err(DaemonError::io(format!("list {}", dir.display())))?;
    for entry in listing {
        let entry = entry.map_err(DaemonError::io(format!("list {}", dir.display())))?;
        take_entry(&entry, &mut archives, &mut provenances)?;
    }
    if let Some(component) = archives.keys().find(|c| !provenances.contains(*c)) {
        return Err(invalid(format!(
            "{component}.tar has no {component}.json provenance beside it"
        )));
    }
    if let Some(component) = provenances.iter().find(|c| !archives.contains_key(*c)) {
        return Err(invalid(format!(
            "{component}.json has no {component}.tar layer beside it"
        )));
    }
    let mut order: Vec<String> = declared_order(&text)
        .into_iter()
        .filter(|name| archives.contains_key(name))
        .collect();
    // A manifest given as a table (`[components.<name>]`) declares no order; the rest follow by
    // name.
    for name in archives.keys() {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    let mut entries = Vec::with_capacity(order.len());
    for component in order {
        let declared = manifest.components.get(&component).ok_or_else(|| {
            invalid(format!(
                "{component}.tar names a component {} does not declare",
                manifest_path.display()
            ))
        })?;
        if declared.boot {
            return Err(invalid(format!(
                "'{component}' takes effect only on a cold boot; install it through its scenario, not the incoming directory"
            )));
        }
        if declared.info.scope == Scope::App {
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
            scope: declared.info.scope,
            info: declared.info.clone(),
            archive,
            provenance,
        });
    }
    Ok(entries)
}

/// The account a component's scope merges into on a machine that runs no named accounts.
fn account_for(entry: &Entry) -> Result<&'static str, DaemonError> {
    match entry.scope {
        Scope::Vm => Ok(VM_ACCOUNT),
        Scope::Session => Ok(DEVICE_ACCOUNT),
        Scope::App => Err(invalid(format!(
            "'{}' is an app; incoming layers carry components",
            entry.component
        ))),
    }
}

fn provenance(entry: &Entry) -> Result<Provenance, DaemonError> {
    let bytes = fs::read(&entry.provenance).map_err(DaemonError::io(format!(
        "read {}",
        entry.provenance.display()
    )))?;
    let provenance: Provenance = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("{}: {error}", entry.provenance.display())))?;
    if provenance.component != entry.component {
        return Err(invalid(format!(
            "{} describes '{}', not '{}'",
            entry.provenance.display(),
            provenance.component,
            entry.component
        )));
    }
    let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    if provenance.target.as_deref() != Some(target.as_str()) {
        return Err(invalid(format!(
            "{} is for {}, but this machine is {target}",
            entry.component,
            provenance.target.as_deref().unwrap_or("an unknown target")
        )));
    }
    Ok(provenance)
}

/// Import each layer in `dir` and swap it into its account, in manifest order, removing each
/// pair once it applied. Each applied component is printed as it lands, so a later failure
/// still shows what did; the first failure stops the run with the rest left in place.
///
/// # Errors
/// What [`entries`] refuses, a provenance that is not this component's or this machine's, or
/// the import or swap failing.
pub fn apply_incoming(
    daemon: &mut Daemon,
    dir: &Path,
    manifest: &Path,
) -> Result<Vec<Applied>, DaemonError> {
    let entries = entries(dir, manifest)?;
    let mut applied = Vec::with_capacity(entries.len());
    for entry in &entries {
        let account = account_for(entry)?;
        let imported = daemon.call::<LayerImport>(LayerImport {
            source: LayerSource::Path(entry.archive.display().to_string()),
            provenance: Box::new(provenance(entry)?),
        })?;
        let LayerImportResult { hash, .. } = serde_json::from_value(imported)
            .map_err(|error| invalid(format!("the import answered {error}")))?;
        daemon.call::<Swap>(Swap {
            account: account.into(),
            component: entry.component.clone(),
            layer: hash.clone(),
            info: Some(entry.info.clone()),
            partner: None,
        })?;
        for path in [&entry.archive, &entry.provenance] {
            fs::remove_file(path).map_err(DaemonError::io(format!(
                "remove applied {}",
                path.display()
            )))?;
        }
        eprintln!("applied {} {hash} {account}", entry.component);
        applied.push(Applied {
            component: entry.component.clone(),
            layer: hash,
            account,
        });
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use tilo_device_protocol::BaseInfo;

    use super::*;
    use crate::apply::RecordingApply;

    fn manifest(order: &[(&str, &str)]) -> String {
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

    /// A pair as `tilo-vm layer-pack --incoming` writes it: a tar of `usr/bin/<component>` and
    /// a provenance built for this machine.
    fn plant(dir: &Path, component: &str) {
        let tree = tempfile::tempdir().unwrap();
        fs::create_dir_all(tree.path().join("usr/bin")).unwrap();
        fs::write(tree.path().join("usr/bin").join(component), component).unwrap();
        let mut archive =
            tar::Builder::new(fs::File::create(dir.join(format!("{component}.tar"))).unwrap());
        archive
            .append_dir_all("usr", tree.path().join("usr"))
            .unwrap();
        archive.finish().unwrap();
        let mut provenance = Provenance::local(component);
        provenance.source_commit = Some("ab".repeat(20));
        provenance.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
        provenance.cargo_lock_hash = Some("cd".repeat(32));
        fs::write(
            dir.join(format!("{component}.json")),
            serde_json::to_vec(&provenance).unwrap(),
        )
        .unwrap();
    }

    tilo_test_env::scenario! {
        fn an_installation_s_layers_apply_at_first_boot;

        phase the_directory_is_read_in_manifest_order_and_refuses_what_it_cannot_apply;
        {
            let dir = tempfile::tempdir().unwrap();
            let incoming = dir.path().join("incoming");
            fs::create_dir(&incoming).unwrap();
            let manifest_path = dir.path().join("components.toml");
            fs::write(&manifest_path, manifest(&[("alpha", "vm")])).unwrap();
            assert!(entries(&incoming, &manifest_path).unwrap().is_empty());
            // Alpha sorts first but is declared second: order comes from the manifest, not from
            // the directory listing.
            fs::write(&manifest_path, manifest(&[("zulu", "session"), ("alpha", "vm")])).unwrap();
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

            fs::remove_file(incoming.join("zulu.json")).unwrap();
            let error = entries(&incoming, &manifest_path).unwrap_err();
            assert!(error.to_string().contains("zulu.tar has no zulu.json"), "{error}");

            plant(&incoming, "zulu");
            fs::write(incoming.join("stray.txt"), b"x").unwrap();
            let error = entries(&incoming, &manifest_path).unwrap_err();
            assert!(error.to_string().contains("neither"), "{error}");

            fs::remove_file(incoming.join("stray.txt")).unwrap();
            plant(&incoming, "ghost");
            let error = entries(&incoming, &manifest_path).unwrap_err();
            assert!(error.to_string().contains("ghost.tar names a component"), "{error}");
        }

        phase each_layer_is_imported_swapped_into_its_scope_account_and_removed;
        {
            let root = tempfile::tempdir().unwrap();
            let incoming = root.path().join("incoming");
            fs::create_dir(&incoming).unwrap();
            let manifest_path = root.path().join("components.toml");
            fs::write(&manifest_path, manifest(&[("alpha", "vm")])).unwrap();
            plant(&incoming, "alpha");
            let mut daemon = Daemon::new(
                root.path(),
                Box::new(RecordingApply::default()),
                None,
                BaseInfo::default(),
            );
            let applied = apply_incoming(&mut daemon, &incoming, &manifest_path).unwrap();
            assert_eq!(applied.len(), 1);
            assert_eq!((applied[0].component.as_str(), applied[0].account), ("alpha", VM_ACCOUNT));
            assert!(root.path().join("layers").join(&applied[0].layer).is_dir());
            assert_eq!(fs::read_dir(&incoming).unwrap().count(), 0, "applied pairs are removed");
            // Nothing is left, so a second boot has nothing to do.
            assert!(apply_incoming(&mut daemon, &incoming, &manifest_path).unwrap().is_empty());
        }

        phase a_layer_built_for_another_machine_is_refused_before_it_is_imported;
        {
            let root = tempfile::tempdir().unwrap();
            let incoming = root.path().join("incoming");
            fs::create_dir(&incoming).unwrap();
            let manifest_path = root.path().join("components.toml");
            fs::write(&manifest_path, manifest(&[("alpha", "vm")])).unwrap();
            plant(&incoming, "alpha");
            let path = incoming.join("alpha.json");
            let mut provenance: Provenance =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            provenance.target = Some("riscv64-unknown-linux-gnu".into());
            fs::write(&path, serde_json::to_vec(&provenance).unwrap()).unwrap();
            let mut daemon = Daemon::new(
                root.path(),
                Box::new(RecordingApply::default()),
                None,
                BaseInfo::default(),
            );
            let error = apply_incoming(&mut daemon, &incoming, &manifest_path).unwrap_err();
            assert!(error.to_string().contains("riscv64-unknown-linux-gnu"), "{error}");
            assert!(incoming.join("alpha.tar").is_file(), "a refused pair stays for a retry");
        }
    }
}
