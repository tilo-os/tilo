//! Windows sources: recognizable folders, cloud folders and Steam.
//!
//! Every fact comes from the reviewed Windows inventory, which is built from
//! individually granted roots. Redirected folders that are inside another
//! selected root are covered once, by real directory identity, rather than
//! copied twice or merged by a matching name.

use super::steam::{EntryCount, Platform, SteamSource};
use super::{
    CloudProvider, Counts, Fact, Folders, KnownFolder, Label, NodeKind, RootRef, SaveRoot,
    SteamCatalog, Tree, finalize, folder_children, group,
};
use crate::migration_directory::PinnedDirectory;
use crate::windows_inventory::{Availability, EntryKind, Inventory, RootInventory, RootRole};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn build(inventory: &Inventory, catalog: SteamCatalog) -> Tree {
    let roots = inventory
        .roots
        .iter()
        .map(|root| RootRef::Windows {
            source_id: root.source_id.clone(),
        })
        .collect::<Vec<_>>();
    let mut entries = Vec::new();
    for (index, root) in inventory.roots.iter().enumerate() {
        let Ok(root_index) = u16::try_from(index) else {
            continue;
        };
        for entry in &root.entries {
            entries.push(entry_count(root_index, entry));
        }
    }
    let folders = known_folders(inventory);
    let pins = inventory
        .roots
        .iter()
        .map(|root| PinnedDirectory::open_absolute(&root.path).ok())
        .collect::<Vec<Option<Arc<PinnedDirectory>>>>();
    let mut sources = Vec::new();
    for (index, root) in inventory.roots.iter().enumerate() {
        let Ok(root_index) = u16::try_from(index) else {
            continue;
        };
        let id = super::root_node_id(&root.source_id);
        let private_label = root
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let identity = |relative: &Path| {
            pins.get(index)
                .and_then(Option::as_ref)
                .and_then(|pin| super::relative_directory_identity(pin, relative))
        };
        let mut facts = root_facts(root);
        // The inventory records files; a folder is a reviewed path whose
        // entries exist beneath it. Deriving the ancestors keeps the tree
        // bounded by what was actually inventoried and never reopens a path.
        let mut dirs = BTreeSet::new();
        for entry in &root.entries {
            if entry.kind == EntryKind::Directory && entry.copy_eligible {
                dirs.insert(entry.relative.clone());
            }
            let mut parent = entry.relative.parent();
            while let Some(path) = parent {
                if path.as_os_str().is_empty() {
                    break;
                }
                dirs.insert(path.to_path_buf());
                parent = path.parent();
            }
        }
        let root_dirs = &dirs;
        let node = if let Some(covered) = &root.covered_by {
            let mut node = group(
                id,
                root_label(root),
                NodeKind::Source,
                root_index,
                Vec::new(),
                vec![Fact::AlreadyIncluded {
                    covered_by: covered.clone(),
                }],
            );
            node.selectable = false;
            node.with_private_label(private_label)
        } else if matches!(root.role, RootRole::Steam | RootRole::SteamLibrary) {
            let external = |root_fact: SaveRoot, relative: &str| {
                resolve_external(&folders, &entries, root_fact, relative)
            };
            let source = SteamSource {
                root: root_index,
                entries: &entries,
                relative: PathBuf::new(),
                label: root_label(root),
                platform: Platform::Windows,
                apps: &root.steam_apps,
                libraries: &root.additional_grants,
                catalog: &catalog,
                external: &external,
            };
            super::steam::node(&id, &source, root_dirs).with_private_label(private_label)
        } else {
            let mut node_facts = std::mem::take(&mut facts);
            let folders = Folders {
                root: root_index,
                entries: &entries,
                dirs: root_dirs,
                skip: &[],
                label: &subfolder_label,
                identity: &identity,
            };
            let mut budget = super::MAX_NODES;
            let children = folder_children(
                &id,
                Path::new(""),
                0,
                &folders,
                &mut node_facts,
                &mut budget,
            );
            let mut node = group(
                id,
                root_label(root),
                NodeKind::Source,
                root_index,
                children,
                node_facts,
            );
            node.own_paths = vec![PathBuf::new()];
            node.private_label = private_label;
            node
        };
        sources.push(node);
    }
    finalize(sources, &entries, roots)
}

fn entry_count(root: u16, entry: &crate::windows_inventory::Entry) -> EntryCount {
    match entry.availability {
        // Only files carry bytes; a directory is accounted through its entries.
        Availability::Local if entry.kind == EntryKind::File => {
            EntryCount::local(root, entry.relative.clone(), entry.bytes)
        }
        Availability::Local => EntryCount {
            root,
            path: entry.relative.clone(),
            counts: Counts::default(),
            unsupported: false,
        },
        Availability::Placeholder => {
            EntryCount::online_only(root, entry.relative.clone(), entry.bytes)
        }
        Availability::Unsupported => EntryCount::unsupported(root, entry.relative.clone()),
    }
}

fn subfolder_label(path: &Path) -> Label {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(KnownFolder::from_name)
        .map_or(Label::Folder, Label::KnownFolder)
}

fn root_label(root: &RootInventory) -> Label {
    if root.role == RootRole::OneDrive {
        return Label::Cloud(CloudProvider::OneDrive);
    }
    if root.role == RootRole::Outlook {
        return Label::Outlook;
    }
    if matches!(root.role, RootRole::Steam | RootRole::SteamLibrary) {
        return Label::Steam;
    }
    root.path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(KnownFolder::from_name)
        .map_or(Label::Folder, Label::KnownFolder)
}

fn root_facts(root: &RootInventory) -> Vec<Fact> {
    let mut facts = Vec::new();
    let provider = root
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(KnownFolder::from_name)
        .and_then(CloudProvider::for_folder);
    let provider = (root.role == RootRole::OneDrive)
        .then_some(CloudProvider::OneDrive)
        .or(provider);
    if let Some(provider) = provider {
        facts.push(Fact::CloudProvider(provider));
    }
    let placeholders = root
        .entries
        .iter()
        .any(|entry| entry.availability == Availability::Placeholder);
    if placeholders {
        facts.push(Fact::OnlineOnlyNotCopied);
        facts.push(Fact::DownloadBeforeCopy);
    }
    if facts
        .iter()
        .any(|fact| matches!(fact, Fact::CloudProvider(_)))
    {
        // A provider connection is only offered when a real integration exists;
        // none is claimed here, so the person can only copy or leave it out.
        facts.push(Fact::ProviderConnectionUnavailable);
    }
    if root.entries.iter().any(|entry| entry.bytes.is_none()) {
        facts.push(Fact::UnknownSize);
    }
    if root
        .entries
        .iter()
        .any(|entry| entry.availability == Availability::Unsupported)
    {
        facts.push(Fact::NotCopyableAtSource);
    }
    if !root.gaps.is_empty() {
        facts.push(Fact::CoverageIncomplete);
    }
    facts
}

/// Recognizable folders mapped to the root that actually inventories them and
/// the prefix inside that root. A redirected folder covered by a cloud root
/// resolves through the containing root, which is the one that owns its data.
fn known_folders(inventory: &Inventory) -> BTreeMap<KnownFolder, (usize, PathBuf)> {
    let mut folders = BTreeMap::new();
    for (index, root) in inventory.roots.iter().enumerate() {
        let Some(folder) = root
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(KnownFolder::from_name)
        else {
            continue;
        };
        let resolved = match &root.covered_by {
            None => (index, PathBuf::new()),
            Some(container) => {
                let Some(outer) = inventory
                    .roots
                    .iter()
                    .position(|candidate| &candidate.source_id == container)
                else {
                    continue;
                };
                let Some(container) = inventory.roots.get(outer) else {
                    continue;
                };
                let Ok(prefix) = root.path.strip_prefix(&container.path) else {
                    continue;
                };
                (outer, prefix.to_path_buf())
            }
        };
        folders.entry(folder).or_insert(resolved);
    }
    folders
}

/// Resolve a documented save location to another reviewed root, if that root is
/// one of the recognizable folders and the location is in its inventory.
fn resolve_external(
    folders: &BTreeMap<KnownFolder, (usize, PathBuf)>,
    entries: &[EntryCount],
    root_fact: SaveRoot,
    relative: &str,
) -> Option<(u16, PathBuf)> {
    let folder = match root_fact {
        SaveRoot::Documents => KnownFolder::Documents,
        SaveRoot::SavedGames => KnownFolder::SavedGames,
        SaveRoot::AppDataRoaming | SaveRoot::AppDataLocal | SaveRoot::AppDataLocalLow => {
            KnownFolder::AppData
        }
        SaveRoot::SteamUserdata
        | SaveRoot::HomeLocalShare
        | SaveRoot::HomeConfig
        | SaveRoot::HomeData => return None,
    };
    let (index, prefix) = folders.get(&folder)?;
    let root = u16::try_from(*index).ok()?;
    let mut path = prefix.clone();
    for part in relative.split('/') {
        path.push(part);
    }
    entries
        .iter()
        .any(|entry| entry.root == root && entry.path.starts_with(&path))
        .then_some((root, path))
}

#[cfg(test)]
pub(super) fn entry_count_for_test(
    root: u16,
    entry: &crate::windows_inventory::Entry,
) -> EntryCount {
    entry_count(root, entry)
}
