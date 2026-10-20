//! Home sources: recognizable folders, cloud-backed folders and Steam.
//!
//! Discovery reads only the reviewed inventory of the granted Home. Steam's
//! library and application metadata is parsed from bounded Valve files under
//! the same pinned grant; no directory is walked a second time and no external
//! library is opened.

use super::steam::{self, EntryCount, Platform, SteamSource};
use super::{
    Fact, Folders, KnownFolder, Label, Node, NodeKind, RootRef, SaveRoot, SteamCatalog, Tree,
    finalize, folder_children, group, leaf,
};
use crate::migration_directory::PinnedDirectory;
use crate::migration_review::SourceInventory;
use crate::nofollow::metadata_nofollow;
use crate::windows_inventory::SteamAppManifest;
use cap_std::fs::Dir;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Valve's own locations for a Steam folder, relative to the Home.
const NATIVE: &str = ".local/share/Steam";
const LEGACY: &str = ".steam/steam";
const FLATPAK: &str = ".var/app/com.valvesoftware.Steam";
const FLATPAK_STEAM: &str = ".var/app/com.valvesoftware.Steam/.local/share/Steam";

pub(crate) fn build(
    pin: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    catalog: SteamCatalog,
) -> std::io::Result<Tree> {
    let (entries, dirs) = entries(inventory)?;
    let root_id = super::root_node_id(&super::root_identity(pin));
    let identity = |relative: &Path| super::relative_directory_identity(pin, relative);
    let skip = steam_roots(&dirs);
    let mut sources = Vec::new();
    for root in &skip {
        sources.push(steam_source(
            pin, &entries, &dirs, root, &root_id, catalog, true,
        )?);
    }
    let plain = plain_sources(
        pin,
        &entries,
        &dirs,
        &skip,
        catalog,
        &mut sources,
        &root_id,
        &identity,
    )?;
    sources.extend(plain);
    Ok(finalize(sources, &entries, vec![RootRef::Home]))
}

/// The reviewed inventory as entry counts plus the directory set. Only the
/// canonical Home inventory is read; nothing is walked again here.
fn entries(inventory: &SourceInventory) -> std::io::Result<(Vec<EntryCount>, BTreeSet<PathBuf>)> {
    let mut entries = Vec::new();
    let mut dirs = BTreeSet::new();
    for item in inventory.entries() {
        let (path, kind, bytes) = item?;
        match kind {
            1 => {
                dirs.insert(path);
            }
            2 => entries.push(EntryCount::local(0, path, bytes)),
            0 | 3 | 4 => entries.push(EntryCount::unsupported(0, path)),
            _ => {}
        }
    }
    Ok((entries, dirs))
}

/// Recognizable folders, cloud-backed folders, loose top-level files and one
/// bounded overflow node. Steam folders are handled as their own sources.
fn plain_sources(
    pin: &Arc<PinnedDirectory>,
    entries: &[EntryCount],
    dirs: &BTreeSet<PathBuf>,
    skip: &[PathBuf],
    catalog: SteamCatalog,
    steam: &mut Vec<Node>,
    root_id: &str,
    identity: &dyn Fn(&Path) -> Option<String>,
) -> std::io::Result<Vec<Node>> {
    let top = dirs
        .iter()
        .filter(|path| path.parent() == Some(Path::new("")))
        .filter(|path| !skip.iter().any(|root| path.starts_with(root)))
        .cloned()
        .collect::<Vec<_>>();
    let mut plain = Vec::new();
    let mut overflow = Vec::new();
    for directory in top {
        if plain.len() >= super::MAX_CHILDREN.saturating_sub(2) {
            overflow.push(directory);
            continue;
        }
        let folder = directory
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(KnownFolder::from_name);
        if folder == Some(KnownFolder::Steam) {
            // A Steam folder outside the recognized locations still becomes a
            // Steam source so its games are not folded into an ordinary folder.
            steam.push(steam_source(
                pin, entries, dirs, &directory, root_id, catalog, false,
            )?);
            continue;
        }
        let mut facts = Vec::new();
        if let Some(provider) = folder.and_then(super::CloudProvider::for_folder) {
            facts.push(Fact::CloudProvider(provider));
            // A provider client keeps real files here; content that exists only
            // on the server cannot be seen, and no connection is claimed.
            facts.push(Fact::RemoteCoverageUnknown);
            facts.push(Fact::ProviderConnectionUnavailable);
        }
        let entry_identity = identity(&directory);
        let id = super::entry_node_id(root_id, &directory, entry_identity.as_deref());
        let folders = Folders {
            root: 0,
            entries,
            dirs,
            skip,
            label: &subfolder_label,
            identity,
        };
        let mut budget = super::MAX_NODES;
        let children = folder_children(&id, &directory, 1, &folders, &mut facts, &mut budget);
        let mut node = group(
            id,
            folder.map_or(Label::Folder, Label::KnownFolder),
            NodeKind::Folder,
            0,
            children,
            facts,
        );
        node.own_paths = vec![directory.clone()];
        node.private_label = directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        plain.push(node);
    }
    let mut loose = entries
        .iter()
        .filter(|entry| entry.path.parent() == Some(Path::new("")) && !dirs.contains(&entry.path))
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    loose.sort();
    loose.dedup();
    if !loose.is_empty() {
        let id = super::semantic_node_id(root_id, "files");
        if loose.len() <= 256 {
            plain.push(leaf(
                id,
                Label::Files,
                NodeKind::Files,
                0,
                loose,
                Vec::new(),
            ));
        } else {
            let mut node = leaf(
                id,
                Label::Files,
                NodeKind::Files,
                0,
                loose.into_iter().take(256).collect(),
                vec![Fact::CoverageIncomplete],
            );
            node.selectable = false;
            plain.push(node);
        }
    }
    if !overflow.is_empty() {
        let material = overflow
            .iter()
            .flat_map(|path| path.as_os_str().as_encoded_bytes().iter().copied())
            .collect::<Vec<_>>();
        let id = super::derived_node_id(root_id, "overflow", &material);
        let mut node = group(
            id,
            Label::Other,
            NodeKind::Folder,
            0,
            Vec::new(),
            vec![Fact::CoverageIncomplete],
        );
        node.own_paths = overflow;
        plain.push(node);
    }
    Ok(plain)
}

fn subfolder_label(_path: &Path) -> Label {
    Label::Folder
}

fn steam_roots(dirs: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    [NATIVE, LEGACY, FLATPAK_STEAM, FLATPAK]
        .iter()
        .map(PathBuf::from)
        .filter(|root| dirs.contains(&root.join("steamapps")))
        .fold(Vec::new(), |mut roots: Vec<PathBuf>, root| {
            if !roots.iter().any(|other| root.starts_with(other)) {
                roots.push(root);
            }
            roots
        })
}

fn steam_source(
    pin: &Arc<PinnedDirectory>,
    entries: &[EntryCount],
    dirs: &BTreeSet<PathBuf>,
    root: &Path,
    root_id: &str,
    catalog: SteamCatalog,
    with_metadata: bool,
) -> std::io::Result<Node> {
    let id = super::entry_node_id(
        root_id,
        root,
        super::relative_directory_identity(pin, root).as_deref(),
    );
    let apps = if with_metadata {
        manifests(pin, root)?
    } else {
        Vec::new()
    };
    let libraries = if with_metadata {
        libraries(pin, root)
    } else {
        Vec::new()
    };
    let external =
        |root_fact: SaveRoot, relative: &str| steam::home_external(0, entries, root_fact, relative);
    let source = SteamSource {
        root: 0,
        entries,
        relative: root.to_path_buf(),
        label: Label::Steam,
        platform: Platform::Linux,
        apps: &apps,
        libraries: &libraries,
        catalog: &catalog,
        external: &external,
    };
    Ok(steam::node(&id, &source, dirs).with_private_label(
        root.file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    ))
}

fn manifests(pin: &Arc<PinnedDirectory>, steam: &Path) -> std::io::Result<Vec<SteamAppManifest>> {
    let steamapps = steam.join("steamapps");
    let Ok(directory) = open_relative(pin, &steamapps) else {
        return Ok(Vec::new());
    };
    let mut names = Dir::from_std_file(directory.file.try_clone()?)
        .entries()?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.starts_with("appmanifest_")
                && Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("acf"))
        })
        .collect::<Vec<_>>();
    names.sort();
    let mut apps = Vec::new();
    for name in names.into_iter().take(256) {
        let Ok(meta) = metadata_nofollow(&directory.file, Path::new(&name)) else {
            continue;
        };
        let Ok(text) = crate::windows_inventory::metadata::read_bounded(
            &directory,
            std::ffi::OsStr::new(&name),
            &meta,
        ) else {
            continue;
        };
        if let Ok(Some(app)) = crate::windows_inventory::metadata::app_manifest(&text)
            && !apps.contains(&app)
        {
            apps.push(app);
        }
    }
    Ok(apps)
}

fn libraries(pin: &Arc<PinnedDirectory>, steam: &Path) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for relative in [
        steam.join("steamapps/libraryfolders.vdf"),
        steam.join("config/libraryfolders.vdf"),
    ] {
        let Some((parent, name)) = split(&relative) else {
            continue;
        };
        let Ok(directory) = open_relative(pin, &parent) else {
            continue;
        };
        let Ok(meta) = metadata_nofollow(&directory.file, Path::new(&name)) else {
            continue;
        };
        let Ok(text) = crate::windows_inventory::metadata::read_bounded(
            &directory,
            std::ffi::OsStr::new(&name),
            &meta,
        ) else {
            continue;
        };
        if let Ok(paths) = crate::windows_inventory::metadata::libraries(&text) {
            for path in paths {
                if !libraries.contains(&path) && libraries.len() < 64 {
                    libraries.push(path);
                }
            }
        }
    }
    libraries
}

fn split(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?.to_string();
    Some((path.parent()?.to_path_buf(), name))
}

fn open_relative(
    pin: &Arc<PinnedDirectory>,
    relative: &Path,
) -> std::io::Result<Arc<PinnedDirectory>> {
    let mut directory = Arc::clone(pin);
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Steam metadata path is not a normalized relative path",
            ));
        };
        let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Steam metadata path is not a directory",
            ));
        }
        directory = directory.open_child(name, &metadata)?;
    }
    Ok(directory)
}
