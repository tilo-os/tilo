//! Steam sources: installed titles, per-account user data, Proton prefixes and
//! documented save locations.
//!
//! Installed game files are reinstallable; save state is not. The two are
//! separate children so a person can copy saves without copying tens of
//! gigabytes of assets. Saves that live outside the Steam folder stay owned by
//! the source that contains them; the game reports where they are documented.
//! A title with no documented location keeps an explicit unknown fact: it is
//! never shown as having zero saved progress. A Steam Cloud label is never
//! treated as proof that a save was uploaded or that every save is covered.

use super::{Counts, Fact, Label, Node, NodeKind, SaveRoot, SteamCatalog, group, leaf};
use crate::windows_inventory::SteamAppManifest;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// Per-entry accounting handed to the tree builder. Only files are counted, so
/// summing a subtree cannot double count a directory and its contents.
#[derive(Clone, Debug)]
pub(crate) struct EntryCount {
    pub root: u16,
    pub path: PathBuf,
    pub counts: Counts,
    pub unsupported: bool,
}

impl EntryCount {
    pub(super) fn local(root: u16, path: impl Into<PathBuf>, bytes: Option<u64>) -> Self {
        let mut counts = Counts::default();
        match bytes {
            Some(bytes) => {
                counts.local_files = 1;
                counts.local_bytes = bytes;
            }
            None => counts.unknown_size_files = 1,
        }
        Self {
            root,
            path: path.into(),
            counts,
            unsupported: false,
        }
    }

    pub(super) fn online_only(root: u16, path: impl Into<PathBuf>, bytes: Option<u64>) -> Self {
        let mut counts = Counts::default();
        match bytes {
            Some(bytes) => {
                counts.online_only_files = 1;
                counts.online_only_bytes = bytes;
            }
            None => counts.unknown_size_files = 1,
        }
        Self {
            root,
            path: path.into(),
            counts,
            unsupported: false,
        }
    }

    pub(super) fn unsupported(root: u16, path: impl Into<PathBuf>) -> Self {
        Self {
            root,
            path: path.into(),
            counts: Counts::default(),
            unsupported: true,
        }
    }
}

pub(super) fn unsupported_under(entries: &[EntryCount], root: u16, prefix: &Path) -> bool {
    entries
        .iter()
        .any(|entry| entry.root == root && entry.unsupported && entry.path.starts_with(prefix))
}

/// A documented-location resolver, general over the borrowed relative path.
pub(crate) type External<'a> = dyn for<'r> Fn(SaveRoot, &'r str) -> Option<(u16, PathBuf)> + 'a;

/// Which save-location vocabulary applies to this Steam folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Platform {
    Windows,
    Linux,
}

/// One Steam folder inside an already reviewed root.
pub(super) struct SteamSource<'a> {
    /// Index of the granted root that contains this Steam folder.
    pub root: u16,
    /// The reviewed inventory of that root.
    pub entries: &'a [EntryCount],
    /// The folder that contains `steamapps`, relative to the granted root.
    pub relative: PathBuf,
    pub label: Label,
    pub platform: Platform,
    pub apps: &'a [SteamAppManifest],
    /// External library locations named by Valve's metadata. Suggestions only.
    pub libraries: &'a [PathBuf],
    pub catalog: &'a SteamCatalog,
    /// Resolve a documented location in another reviewed root, if it is there.
    pub external: &'a External<'a>,
}

/// Build the Steam source node. `entries` is the reviewed inventory of the
/// whole granted root, so every count comes from data the person already
/// granted rather than from a second scan.
pub(super) fn node(id: &str, source: &SteamSource<'_>, relative_dirs: &BTreeSet<PathBuf>) -> Node {
    let mut games = BTreeMap::<u32, Node>::new();
    let mut unnamed = Vec::new();
    let mut installed = BTreeSet::new();
    for app in source.apps {
        let assets = source
            .relative
            .join("steamapps/common")
            .join(&app.install_dir);
        let present = relative_dirs.contains(&assets);
        if present {
            installed.insert(assets.clone());
        }
        games.insert(
            app.app_id,
            game_node(
                super::semantic_node_id(id, &format!("game-{}", app.app_id)),
                Some(app.app_id),
                Some(app.name.clone()),
                present.then_some(assets),
                source,
            ),
        );
    }
    for directory in relative_dirs {
        if installed_title(directory, &source.relative).is_none() || installed.contains(directory) {
            continue;
        }
        unnamed.push(game_node(
            super::derived_node_id(id, "game", directory.as_os_str().as_encoded_bytes()),
            None,
            None,
            Some(directory.clone()),
            source,
        ));
    }
    // Saves for titles that are no longer installed are still irreplaceable.
    for app_id in userdata_apps(relative_dirs, &source.relative) {
        games.entry(app_id).or_insert_with(|| {
            let title = source.catalog.app(app_id).map(|app| app.name.to_string());
            game_node(
                super::semantic_node_id(id, &format!("game-{app_id}")),
                Some(app_id),
                title,
                None,
                source,
            )
        });
    }

    let mut list = games.into_values().collect::<Vec<_>>();
    list.sort_by(|a, b| {
        a.title
            .as_deref()
            .unwrap_or("")
            .cmp(b.title.as_deref().unwrap_or(""))
            .then_with(|| a.id.cmp(&b.id))
    });
    list.extend(unnamed);

    let mut children = Vec::new();
    if !list.is_empty() {
        children.push(group(
            super::semantic_node_id(id, "games"),
            Label::Steam,
            NodeKind::Folder,
            source.root,
            list,
            Vec::new(),
        ));
    }
    for library in source.libraries {
        let mut reference = leaf(
            super::derived_node_id(id, "library", library.as_os_str().as_encoded_bytes()),
            Label::SteamLibrary,
            NodeKind::Unavailable,
            source.root,
            Vec::new(),
            vec![Fact::NeedsIndependentGrant],
        );
        reference.selectable = false;
        children.push(reference);
    }

    let mut facts = Vec::new();
    if unsupported_under(source.entries, source.root, &source.relative) {
        facts.push(Fact::NotCopyableAtSource);
    }
    if children.iter().any(|child| {
        child
            .children
            .iter()
            .any(|game| game.facts.contains(&Fact::SaveLocationUnknown))
    }) {
        facts.push(Fact::SaveLocationUnknown);
    }
    let mut root = group(
        id.to_string(),
        source.label,
        NodeKind::Folder,
        source.root,
        children,
        facts,
    );
    root.own_paths = vec![source.relative.clone()];
    root
}

fn installed_title(directory: &Path, steam: &Path) -> Option<String> {
    let relative = directory.strip_prefix(steam).ok()?;
    let parts = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [first, second, title] if first == "steamapps" && second == "common" => Some(title.clone()),
        _ => None,
    }
}

fn userdata_apps(directories: &BTreeSet<PathBuf>, steam: &Path) -> Vec<u32> {
    let mut apps = BTreeSet::new();
    let userdata = steam.join("userdata");
    for directory in directories {
        let Ok(relative) = directory.strip_prefix(&userdata) else {
            continue;
        };
        let parts = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if let [_, app] = parts.as_slice()
            && let Ok(app_id) = app.parse::<u32>()
            && app_id > 0
        {
            apps.insert(app_id);
        }
    }
    apps.into_iter().collect()
}

fn game_node(
    id: String,
    app_id: Option<u32>,
    title: Option<String>,
    assets: Option<PathBuf>,
    source: &SteamSource<'_>,
) -> Node {
    let private_label = if title.is_none() {
        assets
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    } else {
        None
    };
    let mut children = Vec::new();
    let mut facts = Vec::new();
    let prefix = app_id.map(|app_id| {
        source
            .relative
            .join("steamapps/compatdata")
            .join(app_id.to_string())
    });
    let documented = app_id.and_then(|app_id| source.catalog.app(app_id));

    if let Some(assets) = assets {
        let mut asset_facts = vec![Fact::GameAssets];
        if unsupported_under(source_entries(source), source.root, &assets) {
            asset_facts.push(Fact::NotCopyableAtSource);
        }
        children.push(leaf(
            super::semantic_node_id(&id, "assets"),
            Label::GameAssets,
            NodeKind::GameAssets,
            source.root,
            vec![assets],
            asset_facts,
        ));
    }

    let saves = in_steam_saves(app_id, prefix.as_deref(), documented, source);
    if !saves.is_empty() {
        let mut save_facts = vec![Fact::IrreplaceableSaves, Fact::SteamCloudUnverified];
        if saves
            .iter()
            .any(|path| unsupported_under(source_entries(source), source.root, path))
        {
            save_facts.push(Fact::NotCopyableAtSource);
        }
        children.push(leaf(
            super::semantic_node_id(&id, "saves"),
            Label::GameSaves,
            NodeKind::GameSaves,
            source.root,
            saves,
            save_facts,
        ));
    }

    if let Some(prefix) = prefix {
        let mut prefix_facts = vec![Fact::ProtonPrefix];
        if unsupported_under(source_entries(source), source.root, &prefix) {
            prefix_facts.push(Fact::NotCopyableAtSource);
        }
        children.push(leaf(
            super::semantic_node_id(&id, "prefix"),
            Label::ProtonPrefix,
            NodeKind::ProtonPrefix,
            source.root,
            vec![prefix],
            prefix_facts,
        ));
    }

    // Documented locations outside this Steam folder. They stay owned by their
    // own source; the fact tells the person where the saves are.
    let locations = documented.map_or(&[][..], |app| match source.platform {
        Platform::Windows => app.windows,
        Platform::Linux => app.linux,
    });
    for location in locations {
        if location.root == SaveRoot::SteamUserdata {
            continue;
        }
        facts.push(Fact::ExternalSaveLocation(location.root));
        if (source.external)(location.root, location.relative).is_none() {
            // Not inside any reviewed source: say so instead of implying it is
            // covered by the Steam folder.
            facts.push(Fact::NeedsIndependentGrant);
        }
    }
    facts.dedup();
    if documented.is_none() {
        facts.push(Fact::SaveLocationUnknown);
    }
    let mut node = group(
        id,
        Label::Game,
        NodeKind::Game,
        source.root,
        children,
        facts,
    );
    node.title = title;
    node.private_label = private_label;
    node
}

fn source_entries<'a>(source: &SteamSource<'a>) -> &'a [EntryCount] {
    source.entries
}

/// Save locations inside the Steam folder: per-account user data and the
/// documented locations inside the Proton prefix.
fn in_steam_saves(
    app_id: Option<u32>,
    prefix: Option<&Path>,
    documented: Option<&super::SteamApp>,
    source: &SteamSource<'_>,
) -> Vec<PathBuf> {
    let Some(app_id) = app_id else {
        return Vec::new();
    };
    let entries = source_entries(source);
    let mut paths = BTreeSet::new();
    let userdata = source.relative.join("userdata");
    for account in accounts_with(&userdata, entries, source.root) {
        let path = userdata.join(account).join(app_id.to_string());
        if entries.iter().any(|entry| entry.path.starts_with(&path)) {
            paths.insert(path);
        }
    }
    if let Some(prefix) = prefix {
        for location in documented.map_or(&[][..], |app| app.windows) {
            if location.root == SaveRoot::SteamUserdata {
                continue;
            }
            let path = proton_path(prefix, location.root, location.relative);
            if entries.iter().any(|entry| entry.path.starts_with(&path)) {
                paths.insert(path);
            }
        }
    }
    paths.into_iter().collect()
}

fn accounts_with(userdata: &Path, entries: &[EntryCount], root: u16) -> Vec<String> {
    let mut accounts = BTreeSet::new();
    for entry in entries {
        if entry.root != root {
            continue;
        }
        let Ok(relative) = entry.path.strip_prefix(userdata) else {
            continue;
        };
        if let Some(account) = relative
            .components()
            .next()
            .map(|name| name.as_os_str().to_string_lossy().into_owned())
        {
            accounts.insert(account);
        }
    }
    accounts.into_iter().collect()
}

fn proton_path(prefix: &Path, root: SaveRoot, relative: &str) -> PathBuf {
    let base = prefix.join("pfx/drive_c/users/steamuser");
    let mut path = match root {
        SaveRoot::Documents => base.join("Documents"),
        SaveRoot::SavedGames => base.join("Saved Games"),
        SaveRoot::AppDataRoaming => base.join("AppData/Roaming"),
        SaveRoot::AppDataLocal => base.join("AppData/Local"),
        SaveRoot::AppDataLocalLow => base.join("AppData/LocalLow"),
        SaveRoot::SteamUserdata
        | SaveRoot::HomeLocalShare
        | SaveRoot::HomeConfig
        | SaveRoot::HomeData => base,
    };
    for part in relative.split('/') {
        path.push(part);
    }
    path
}

/// Resolve a documented Linux location against a reviewed Home inventory.
pub(super) fn home_external(
    root: u16,
    entries: &[EntryCount],
    root_fact: SaveRoot,
    relative: &str,
) -> Option<(u16, PathBuf)> {
    let base = match root_fact {
        SaveRoot::HomeConfig => PathBuf::from(".config"),
        SaveRoot::HomeLocalShare | SaveRoot::HomeData => PathBuf::from(".local/share"),
        SaveRoot::Documents => PathBuf::from("Documents"),
        SaveRoot::SavedGames => PathBuf::from("Saved Games"),
        SaveRoot::AppDataRoaming | SaveRoot::AppDataLocal | SaveRoot::AppDataLocalLow => {
            return None;
        }
        SaveRoot::SteamUserdata => return None,
    };
    let mut path = base;
    for part in relative.split('/') {
        path.push(part);
    }
    entries
        .iter()
        .any(|entry| entry.root == root && entry.path.starts_with(&path))
        .then_some((root, path))
}
