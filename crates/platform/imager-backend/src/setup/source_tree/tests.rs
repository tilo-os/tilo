use super::*;
use crate::windows_inventory::{Category, Decision as WindowsDecision, Inventory, RootRole};
use std::fs;
use std::sync::atomic::AtomicBool;

#[test]
fn placeholder_entry_counting_separates_local_online_and_unknown() {
    // A file that is present locally contributes local bytes; the same file as
    // an online-only placeholder contributes online-only bytes; an entry with
    // no readable size is counted as unknown rather than as zero.
    let local = crate::windows_inventory::Entry {
        relative: std::path::PathBuf::from("a.bin"),
        kind: crate::windows_inventory::EntryKind::File,
        category: Category::PotentiallyUnique,
        bytes: Some(10),
        availability: crate::windows_inventory::Availability::Local,
        copy_eligible: true,
        sync_verified: false,
    };
    let placeholder = crate::windows_inventory::Entry {
        availability: crate::windows_inventory::Availability::Placeholder,
        copy_eligible: false,
        ..local.clone()
    };
    let unknown = crate::windows_inventory::Entry {
        bytes: None,
        ..local.clone()
    };
    let local = windows::entry_count_for_test(0, &local);
    let placeholder = windows::entry_count_for_test(0, &placeholder);
    let unknown = windows::entry_count_for_test(0, &unknown);
    assert_eq!(local.counts.local_bytes, 10);
    assert_eq!(placeholder.counts.online_only_bytes, 10);
    assert_eq!(placeholder.counts.local_bytes, 0);
    assert_eq!(unknown.counts.unknown_size_files, 1);
    assert_eq!(unknown.counts.local_bytes, 0);
}

fn grant(path: &std::path::Path, role: RootRole) -> crate::windows_inventory::SelectedRoot {
    crate::windows_inventory::SelectedRoot {
        path: path.into(),
        role,
    }
}

fn source(tree: &Tree, index: usize) -> &Node {
    &tree.sources[index]
}

fn child(node: &Node, label: Label) -> Option<&Node> {
    node.children.iter().find(|child| child.label == label)
}

#[test]
fn redirected_folder_inside_a_cloud_root_is_covered_once_without_name_merging() {
    let fixture = tempfile::tempdir().unwrap();
    let onedrive = fixture.path().join("OneDrive");
    let documents = onedrive.join("Documents");
    fs::create_dir_all(&documents).unwrap();
    fs::write(documents.join("letter"), b"unique letter").unwrap();
    fs::write(onedrive.join("photo"), b"unique photo").unwrap();
    let inventory = Inventory::discover(&[
        grant(&onedrive, RootRole::OneDrive),
        grant(&documents, RootRole::KnownFolder),
    ])
    .unwrap();
    assert_eq!(inventory.roots.len(), 2);
    assert_eq!(
        inventory.roots[1].covered_by.as_deref(),
        Some(inventory.roots[0].source_id.as_str())
    );
    assert!(
        inventory.roots[1].entries.is_empty(),
        "the redirected folder is not inventoried twice"
    );
    let tree = discover_windows(&inventory, &SteamCatalog::builtin());
    assert_eq!(
        tree.totals.local_bytes,
        (b"unique letter".len() + b"unique photo".len()) as u64
    );
    assert_eq!(
        source(&tree, 0).label,
        Label::Cloud(CloudProvider::OneDrive)
    );
    assert!(!source(&tree, 1).selectable);
    assert!(source(&tree, 1).facts.contains(&Fact::AlreadyIncluded {
        covered_by: inventory.roots[0].source_id.clone()
    }));
    let mut selection = tree.clone().selection();
    assert_eq!(
        selection.state(&source(&tree, 1).id),
        Some(SelectionState::Unavailable)
    );
    selection
        .set(&source(&tree, 0).id, Decision::Exclude)
        .unwrap();
    let exclusions = selection.exclusions();
    assert_eq!(
        exclusions.windows_whole(),
        &[inventory.roots[0].source_id.clone()]
    );

    // Two folders that merely share a name are different data: both stay
    // selectable and both are copied.
    let other = fixture.path().join("Elsewhere/Documents");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("letter"), b"different letter").unwrap();
    let inventory = Inventory::discover(&[
        grant(&documents, RootRole::KnownFolder),
        grant(&other, RootRole::KnownFolder),
    ])
    .unwrap();
    assert!(inventory.roots.iter().all(|root| root.covered_by.is_none()));
    let tree = discover_windows(&inventory, &SteamCatalog::builtin());
    assert_eq!(tree.sources.len(), 2);
    assert!(tree.sources.iter().all(|source| source.selectable));
    assert_eq!(
        tree.totals.local_bytes,
        (b"unique letter".len() + b"different letter".len()) as u64
    );
}

const STARDEW_MANIFEST: &str =
    r#""AppState" { "appid" "413150" "name" "Stardew Valley" "installdir" "Stardew Valley" }"#;

fn steam_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("steamapps/common/Stardew Valley/Content")).unwrap();
    fs::write(
        root.join("steamapps/common/Stardew Valley/Content/game.bin"),
        vec![b'g'; 4096],
    )
    .unwrap();
    fs::write(
        root.join("steamapps/appmanifest_413150.acf"),
        STARDEW_MANIFEST,
    )
    .unwrap();
    fs::create_dir_all(root.join("userdata/123/413150/remote")).unwrap();
    fs::write(root.join("userdata/123/413150/remote/save"), b"saved farm").unwrap();
    // An installed title Valve did not describe in a manifest: its save
    // location stays unknown instead of being reported as zero.
    fs::create_dir_all(root.join("steamapps/common/Mystery Title")).unwrap();
    fs::write(root.join("steamapps/common/Mystery Title/data"), b"mystery").unwrap();
}

#[test]
fn steam_assets_and_saves_are_separate_and_unknown_locations_stay_visible() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("Steam");
    steam_fixture(&root);
    let mut inventory = Inventory::discover(&[grant(&root, RootRole::Steam)]).unwrap();
    let tree = discover_windows(&inventory, &SteamCatalog::builtin());
    let steam = source(&tree, 0);
    let games = child(steam, Label::Steam).unwrap();
    let stardew = games
        .children
        .iter()
        .find(|game| game.title.as_deref() == Some("Stardew Valley"))
        .unwrap();
    let assets = child(stardew, Label::GameAssets).unwrap();
    let saves = child(stardew, Label::GameSaves).unwrap();
    assert_eq!(assets.counts.local_bytes, 4096);
    assert_eq!(saves.counts.local_bytes, b"saved farm".len() as u64);
    assert!(assets.facts.contains(&Fact::GameAssets));
    assert!(saves.facts.contains(&Fact::IrreplaceableSaves));
    assert!(
        saves.facts.contains(&Fact::SteamCloudUnverified),
        "a cloud label is never proof of a durable save"
    );
    let mystery = games
        .children
        .iter()
        .find(|game| game.title.is_none())
        .unwrap();
    assert!(mystery.facts.contains(&Fact::SaveLocationUnknown));
    assert!(
        mystery
            .children
            .iter()
            .all(|child| child.label != Label::GameSaves)
    );
    assert_eq!(
        tree.totals.local_bytes,
        (4096 + b"saved farm".len() + b"mystery".len() + STARDEW_MANIFEST.len()) as u64
    );

    // Selecting saves only: the assets stay, the save subtree is excluded.
    let mut selection = tree.clone().selection();
    selection.set(&saves.id, Decision::Exclude).unwrap();
    let exclusions = selection.exclusions();
    let prefixes = exclusions
        .windows()
        .get(&inventory.roots[0].source_id)
        .unwrap();
    assert!(prefixes.contains(&std::path::PathBuf::from("userdata/123/413150")));
    assert_eq!(
        selection.counts().local_bytes,
        tree.totals.local_bytes - b"saved farm".len() as u64
    );
    let source_id = inventory.roots[0].source_id.clone();
    inventory.select_exclusions(&source_id, prefixes).unwrap();
    assert_eq!(
        inventory.budget().explicitly_excluded_bytes,
        b"saved farm".len() as u64
    );
    inventory
        .select(&source_id, WindowsDecision::Copy, None)
        .unwrap();
    let destination = fixture.path().join("copied");
    let report = inventory
        .copy_selected(&source_id, &destination, &AtomicBool::new(false))
        .unwrap();
    assert!(report.complete(), "{report:?}");
    assert!(destination.join("steamapps/common/Stardew Valley").exists());
    assert!(!destination.join("userdata/123/413150/remote/save").exists());
    assert_eq!(
        fs::read(root.join("userdata/123/413150/remote/save")).unwrap(),
        b"saved farm",
        "exclusion never deletes the original"
    );
}

#[test]
fn documented_saves_outside_steam_are_reported_and_selectable_in_their_own_source() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("Steam");
    let documents = fixture.path().join("Documents");
    steam_fixture(&root);
    fs::create_dir_all(documents.join("My Games/Terraria")).unwrap();
    fs::write(
        documents.join("My Games/Terraria/player.plr"),
        b"terraria save",
    )
    .unwrap();
    fs::write(
        root.join("steamapps/appmanifest_105600.acf"),
        r#""AppState" { "appid" "105600" "name" "Terraria" "installdir" "Terraria" }"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("steamapps/common/Terraria")).unwrap();
    fs::write(root.join("steamapps/common/Terraria/t"), b"terraria").unwrap();
    let inventory = Inventory::discover(&[
        grant(&root, RootRole::Steam),
        grant(&documents, RootRole::KnownFolder),
    ])
    .unwrap();
    let tree = discover_windows(&inventory, &SteamCatalog::builtin());
    let games = child(source(&tree, 0), Label::Steam).unwrap();
    let terraria = games
        .children
        .iter()
        .find(|game| game.title.as_deref() == Some("Terraria"))
        .unwrap();
    assert!(
        terraria
            .facts
            .contains(&Fact::ExternalSaveLocation(SaveRoot::Documents)),
        "the documented save location is visible on the game"
    );
    // The save itself stays owned by the Documents source and is selectable
    // there, so it is copied without copying the whole game.
    let documents_source = source(&tree, 1);
    let my_games = documents_source
        .children
        .iter()
        .find(|child| child.label == Label::Folder)
        .unwrap();
    assert_eq!(my_games.counts.local_bytes, b"terraria save".len() as u64);
    let mut selection = tree.clone().selection();
    selection.set(&games.id, Decision::Exclude).unwrap();
    let exclusions = selection.exclusions();
    let steam_prefixes = exclusions
        .windows()
        .get(&inventory.roots[0].source_id)
        .unwrap();
    assert!(
        steam_prefixes
            .iter()
            .any(|prefix| prefix.starts_with("steamapps/common")),
        "excluding the games group excludes every game's data"
    );
    assert!(
        steam_prefixes
            .iter()
            .any(|prefix| prefix.starts_with("userdata")),
        "uninstalled or per-account saves are excluded too"
    );
    assert!(
        exclusions
            .windows()
            .get(&inventory.roots[1].source_id)
            .is_none(),
        "the save in Documents stays selected"
    );
}

/// Independent baseline: every byte the fixture actually holds.
#[cfg(target_os = "linux")]
fn fixture_bytes(directory: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let metadata = entry.metadata().unwrap();
        if metadata.is_dir() {
            total = total.saturating_add(fixture_bytes(&entry.path()));
        } else if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}

#[cfg(target_os = "linux")]
fn home_fixture(home: &std::path::Path) {
    fs::create_dir_all(home.join("Documents/Work")).unwrap();
    fs::create_dir_all(home.join("Documents/Private")).unwrap();
    fs::write(home.join("Documents/Work/plan"), b"work plan").unwrap();
    fs::write(home.join("Documents/Private/note"), b"private note").unwrap();
    fs::create_dir_all(home.join(".local/share/Steam/steamapps/common/Stardew Valley")).unwrap();
    fs::write(
        home.join(".local/share/Steam/steamapps/common/Stardew Valley/game"),
        vec![b'g'; 1024],
    )
    .unwrap();
    fs::write(
        home.join(".local/share/Steam/steamapps/appmanifest_413150.acf"),
        r#""AppState" { "appid" "413150" "name" "Stardew Valley" "installdir" "Stardew Valley" }"#,
    )
    .unwrap();
    fs::create_dir_all(home.join(".local/share/Steam/userdata/1/413150/remote")).unwrap();
    fs::write(
        home.join(".local/share/Steam/userdata/1/413150/remote/save"),
        b"native save",
    )
    .unwrap();
    fs::create_dir_all(
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/Dota 2"),
    )
    .unwrap();
    fs::write(
        home.join(
            ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/common/Dota 2/game",
        ),
        vec![b'd'; 2048],
    )
    .unwrap();
    fs::write(
        home.join(
            ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/appmanifest_570.acf",
        ),
        r#""AppState" { "appid" "570" "name" "Dota 2" "installdir" "Dota 2" }"#,
    )
    .unwrap();
    fs::create_dir_all(
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/userdata/1/570/remote"),
    )
    .unwrap();
    fs::write(
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/userdata/1/570/remote/save"),
        b"flatpak save",
    )
    .unwrap();
    fs::create_dir_all(home.join(".config/StardewValley/Saves")).unwrap();
    fs::write(
        home.join(".config/StardewValley/Saves/farm"),
        b"stardew save",
    )
    .unwrap();
    fs::create_dir_all(home.join("Nextcloud/Team")).unwrap();
    fs::write(home.join("Nextcloud/Team/agenda"), b"agenda").unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn home_sources_cover_native_and_flatpak_steam_without_double_counting() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    home_fixture(&home);
    let tree = discover_home(&home, &SteamCatalog::builtin()).unwrap();
    let steam = tree
        .sources
        .iter()
        .filter(|source| source.label == Label::Steam)
        .collect::<Vec<_>>();
    assert_eq!(
        steam.len(),
        2,
        "native and Flatpak Steam are separate sources"
    );
    let games = steam
        .iter()
        .flat_map(|source| source.children.iter())
        .flat_map(|group| group.children.iter())
        .filter(|game| game.title.is_some())
        .collect::<Vec<_>>();
    assert!(
        games
            .iter()
            .any(|game| game.title.as_deref() == Some("Dota 2"))
    );
    assert!(
        games
            .iter()
            .any(|game| game.title.as_deref() == Some("Stardew Valley"))
    );
    assert_eq!(
        tree.totals.local_bytes,
        fixture_bytes(&home),
        "each file is counted exactly once"
    );
    // The Flatpak Steam folder is a Steam source, not an ordinary folder.
    assert!(
        tree.sources
            .iter()
            .any(|source| { source.label == Label::Steam && source.counts.local_bytes >= 2048 })
    );
    // A native Stardew save is documented in the catalog as living in
    // .config/StardewValley, which stays owned by the .config source.
    let stardew = games
        .iter()
        .find(|game| game.title.as_deref() == Some("Stardew Valley"))
        .unwrap();
    assert!(
        stardew
            .facts
            .iter()
            .any(|fact| matches!(fact, Fact::ExternalSaveLocation(SaveRoot::HomeConfig))),
        "the native save in .config is reported, not silently claimed"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn serialized_report_never_contains_source_paths_or_private_names() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/alice-private-project")).unwrap();
    fs::write(home.join("Documents/alice-private-project/secret"), b"x").unwrap();
    let tree = discover_home(&home, &SteamCatalog::builtin()).unwrap();
    let nested_node = tree
        .sources
        .iter()
        .find(|source| source.label == Label::KnownFolder(KnownFolder::Documents))
        .unwrap()
        .children
        .first()
        .unwrap();
    assert_eq!(nested_node.private_label(), Some("alice-private-project"));
    let serialized_tree = serde_json::to_string(&tree).unwrap();
    assert!(!serialized_tree.contains("alice-private-project"));
    assert!(!serialized_tree.contains(home.to_string_lossy().as_ref()));
    let nested = nested_node.id.clone();
    let mut selection = tree.selection();
    selection.set(&nested, Decision::Exclude).unwrap();
    let text = selection.report().to_string();
    assert!(!text.contains("alice-private-project"));
    assert!(!text.contains(home.to_string_lossy().as_ref()));
    assert!(!text.contains("secret"));
    assert!(text.contains("documents"));
    assert!(text.contains("exclusions"));
    assert_eq!(selection.exclusions().home().len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn nested_home_exclusions_are_enforced_by_the_canonical_producer() {
    use crate::migration_review::SourceInventory;
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/Work")).unwrap();
    fs::create_dir_all(home.join("Documents/Private")).unwrap();
    fs::write(home.join("Documents/Work/plan"), b"work plan").unwrap();
    fs::write(home.join("Documents/Private/note"), b"private note").unwrap();
    fs::write(home.join("top"), b"top").unwrap();
    let pin = crate::migration::PinnedDirectory::open_absolute(&home).unwrap();
    let full = SourceInventory::capture_portable_home(&pin, &home).unwrap();
    let excluded = vec![std::path::PathBuf::from("Documents/Private")];
    let selected = SourceInventory::capture_portable_home(&pin, &home)
        .unwrap()
        .with_exclusions(&pin, &excluded)
        .unwrap();
    assert_eq!(
        selected.summary().unwrap().bytes,
        full.summary().unwrap().bytes - b"private note".len() as u64
    );
    assert!(
        selected
            .excluded_paths()
            .contains(&std::path::Path::new("Documents/Private")),
        "the archive invocation receives the exclusion prefix"
    );
    assert!(
        selected
            .entries()
            .all(|entry| !entry.unwrap().0.starts_with("Documents/Private"))
    );
    // The same producer refuses a path that is not under the source.
    assert!(
        SourceInventory::capture_portable_home(&pin, &home)
            .unwrap()
            .with_exclusions(&pin, &[std::path::PathBuf::from("missing")])
            .is_err()
    );
    assert!(
        SourceInventory::capture_portable_home(&pin, &home)
            .unwrap()
            .with_exclusions(&pin, &[std::path::PathBuf::from("../escape")])
            .is_err()
    );
    // The full review still sees everything: an exclusion never deletes or
    // rewrites the original.
    assert_eq!(
        fs::read(home.join("Documents/Private/note")).unwrap(),
        b"private note"
    );
    full.validate(&pin).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn reviewed_home_selection_keeps_grant_and_source_proofs() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/Private")).unwrap();
    fs::create_dir_all(home.join("Documents/Work")).unwrap();
    fs::write(home.join("Documents/Private/note"), b"private note").unwrap();
    fs::write(home.join("Documents/Work/plan"), b"work plan").unwrap();
    let exclusions = vec![std::path::PathBuf::from("Documents/Private")];
    let cancel = AtomicBool::new(false);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("XDG_STATE_HOME", fixture.path().join("state"));
    let source =
        crate::setup::portable_home_media::HomeSource::review_selected(&home, &exclusions, &cancel)
            .unwrap_or_else(|error| panic!("selected Home review failed: {error}"));
    let summary = source.summary().unwrap();
    assert_eq!(summary.bytes, b"work plan".len() as u64);
    assert_eq!(source.exclusions(), exclusions.as_slice());
    source.validate().unwrap();
    // Replacing the granted home invalidates the source even with a selection.
    let moved = fixture.path().join("moved");
    fs::rename(&home, &moved).unwrap();
    fs::create_dir_all(home.join("Documents/Work")).unwrap();
    fs::write(home.join("Documents/Work/plan"), b"work plan").unwrap();
    assert!(source.validate().is_err());
    assert_eq!(
        fs::read(moved.join("Documents/Private/note")).unwrap(),
        b"private note"
    );
    // An exclusion that is not under the granted Home is refused before review.
    assert!(
        crate::setup::portable_home_media::HomeSource::review_selected(
            &moved,
            &[std::path::PathBuf::from("Documents/Missing")],
            &AtomicBool::new(false),
        )
        .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sealed_export_omits_the_excluded_subtree_and_verifies_against_it() {
    use crate::setup::portable_transfer::{self, ExportWork, Limits, Purpose};
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/Private")).unwrap();
    fs::create_dir_all(home.join("Documents/Work")).unwrap();
    fs::write(home.join("Documents/Private/note"), b"private note").unwrap();
    fs::write(home.join("Documents/Work/plan"), b"work plan").unwrap();
    let pin = crate::migration::PinnedDirectory::open_absolute(&home).unwrap();
    let excluded = vec![std::path::PathBuf::from("Documents/Private")];
    let inventory = crate::migration_review::SourceInventory::capture_portable_home(&pin, &home)
        .unwrap()
        .with_exclusions(&pin, &excluded)
        .unwrap();
    let mut file = tempfile::tempfile().unwrap();
    let cancel = AtomicBool::new(false);
    let mut observer = |_progress| Ok(());
    let mut work = ExportWork {
        password: b"a long destination password",
        cancel: &cancel,
        setup_binding: Some("reviewed-binding"),
        observer: &mut observer,
    };
    let limits = Limits {
        plaintext_bytes: 8 * 1024 * 1024,
        entries: 1024,
        metadata_bytes: 1024 * 1024,
    };
    // The exporter verifies the archive it produced against this inventory, so a
    // successful export is proof that the excluded subtree was not written.
    let info = portable_transfer::export_stream_reporting(
        &pin,
        &inventory,
        Purpose::Home,
        limits,
        &mut file,
        &mut work,
    )
    .unwrap();
    assert!(info.plaintext_bytes > 0);
    assert_eq!(info.logical_bytes, b"work plan".len() as u64);
    assert_eq!(
        fs::read(home.join("Documents/Private/note")).unwrap(),
        b"private note",
        "the excluded original stays untouched"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn plan_carries_selected_exclusions_and_refuses_escape_paths() {
    use crate::setup::migration_handoff::{Categories, Plan};
    use crate::setup::migration_store::NativePath;
    let mut plan = crate::setup::migration_handoff::fixture_plan();
    plan.categories = Categories {
        home: true,
        flatpak: false,
        accounts: false,
    };
    plan.home_exclusions = vec![NativePath::encode(std::path::Path::new(
        "Documents/Private",
    ))];
    plan.validate().unwrap();
    let bytes = plan.bytes().unwrap();
    let parsed = Plan::parse(&bytes).unwrap();
    assert_eq!(
        parsed.home_exclusion_paths().unwrap(),
        vec![std::path::PathBuf::from("Documents/Private")]
    );
    for escape in ["/absolute", "../up", "a/../b", ""] {
        plan.home_exclusions = vec![NativePath::encode(std::path::Path::new(escape))];
        assert!(plan.validate().is_err(), "{escape}");
    }
    plan.home_exclusions = Vec::new();
    plan.categories = Categories {
        home: false,
        flatpak: true,
        accounts: false,
    };
    plan.home_exclusions = vec![NativePath::encode(std::path::Path::new("Documents"))];
    assert!(
        plan.validate().is_err(),
        "exclusions require selected Home data"
    );
}

fn named<'a>(nodes: &'a [Node], name: &str) -> Option<&'a Node> {
    for node in nodes {
        if node.private_label() == Some(name) {
            return Some(node);
        }
        if let Some(found) = named(&node.children, name) {
            return Some(found);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn documents_tree(home: &std::path::Path) -> Tree {
    discover_home(home, &SteamCatalog::builtin()).unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn identities_follow_the_entry_not_its_position_across_sibling_changes() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/Alpha")).unwrap();
    fs::create_dir_all(home.join("Documents/Beta")).unwrap();
    fs::write(home.join("Documents/Alpha/one"), b"alpha").unwrap();
    fs::write(home.join("Documents/Beta/two"), b"beta").unwrap();

    let first = documents_tree(&home);
    let documents = named(&first.sources, "Documents").unwrap();
    let alpha = named(&documents.children, "Alpha").unwrap().id.clone();
    let beta = named(&documents.children, "Beta").unwrap().id.clone();

    // A sibling inserted before the old first child, and one appended after it,
    // do not move either identity.
    fs::create_dir_all(home.join("Documents/Aaa")).unwrap();
    fs::create_dir_all(home.join("Documents/Gamma")).unwrap();
    fs::write(home.join("Documents/Aaa/three"), b"aaa").unwrap();
    fs::write(home.join("Documents/Gamma/four"), b"gamma").unwrap();
    let second = documents_tree(&home);
    let documents = named(&second.sources, "Documents").unwrap();
    assert_eq!(named(&documents.children, "Alpha").unwrap().id, alpha);
    assert_eq!(named(&documents.children, "Beta").unwrap().id, beta);
    assert!(named(&documents.children, "Aaa").is_some());
    assert!(named(&documents.children, "Gamma").is_some());

    // Removing a sibling changes no surviving identity.
    fs::remove_dir_all(home.join("Documents/Beta")).unwrap();
    let third = documents_tree(&home);
    let documents = named(&third.sources, "Documents").unwrap();
    assert_eq!(named(&documents.children, "Alpha").unwrap().id, alpha);
    assert!(named(&documents.children, "Beta").is_none());

    // Renaming changes the entry identity, so no old decision can follow it.
    fs::rename(home.join("Documents/Alpha"), home.join("Documents/Renamed")).unwrap();
    let fourth = documents_tree(&home);
    let documents = named(&fourth.sources, "Documents").unwrap();
    let renamed = named(&documents.children, "Renamed").unwrap().id.clone();
    assert_ne!(renamed, alpha);
    assert!(named(&documents.children, "Alpha").is_none());

    // Replacing the directory at the same relative name is a different entry.
    fs::rename(
        home.join("Documents/Renamed"),
        fixture.path().join("old-renamed"),
    )
    .unwrap();
    fs::create_dir_all(home.join("Documents/Renamed")).unwrap();
    fs::write(home.join("Documents/Renamed/other"), b"replacement").unwrap();
    let fifth = documents_tree(&home);
    let documents = named(&fifth.sources, "Documents").unwrap();
    let replacement = named(&documents.children, "Renamed").unwrap().id.clone();
    assert_ne!(replacement, renamed);
}

#[cfg(target_os = "linux")]
#[test]
fn interleaved_exclusions_survive_refresh_without_retargeting_a_sibling() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    for name in ["Alpha", "Beta", "Gamma"] {
        fs::create_dir_all(home.join("Documents").join(name)).unwrap();
        fs::write(home.join("Documents").join(name).join("file"), name).unwrap();
    }
    let first = documents_tree(&home);
    let documents = named(&first.sources, "Documents").unwrap().id.clone();
    let alpha = named(&first.sources, "Alpha").unwrap().id.clone();
    let beta = named(&first.sources, "Beta").unwrap().id.clone();
    let gamma = named(&first.sources, "Gamma").unwrap().id.clone();
    assert_eq!(
        alpha,
        named(&first.sources, "Documents").unwrap().children[0].id
    );
    let mut previous = first.selection();
    previous.set(&alpha, Decision::Exclude).unwrap();
    previous.set(&gamma, Decision::Exclude).unwrap();

    fs::create_dir_all(home.join("Documents/Delta")).unwrap();
    fs::write(home.join("Documents/Delta/file"), b"delta").unwrap();
    let refreshed = documents_tree(&home);
    let delta = named(refreshed.sources.as_slice(), "Delta")
        .unwrap()
        .id
        .clone();
    let mut selection = refreshed.selection();
    selection.adopt(&previous);
    assert_eq!(selection.state(&documents), Some(SelectionState::Mixed));
    assert_eq!(selection.state(&alpha), Some(SelectionState::Excluded));
    assert_eq!(selection.state(&beta), Some(SelectionState::Selected));
    assert_eq!(selection.state(&gamma), Some(SelectionState::Excluded));
    assert_eq!(selection.state(&delta), Some(SelectionState::Selected));
    let excluded = selection.exclusions().home().to_vec();
    assert!(excluded.contains(&std::path::PathBuf::from("Documents/Alpha")));
    assert!(excluded.contains(&std::path::PathBuf::from("Documents/Gamma")));
    assert!(!excluded.contains(&std::path::PathBuf::from("Documents/Beta")));
    assert!(!excluded.contains(&std::path::PathBuf::from("Documents/Delta")));

    // Removing an excluded sibling drops that exact decision and cannot move
    // it to the next folder.
    fs::remove_dir_all(home.join("Documents/Alpha")).unwrap();
    let after_removal = documents_tree(&home);
    let mut selection = after_removal.selection();
    selection.adopt(&previous);
    assert_eq!(selection.state(&beta), Some(SelectionState::Selected));
    assert_eq!(selection.state(&gamma), Some(SelectionState::Excluded));
    assert_eq!(
        selection.exclusions().home(),
        &[std::path::PathBuf::from("Documents/Gamma")]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_new_top_level_source_follows_an_explicit_exclude_all_intent() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents")).unwrap();
    fs::create_dir_all(home.join("Music")).unwrap();
    fs::write(home.join("Documents/one"), b"one").unwrap();
    fs::write(home.join("Music/two"), b"two").unwrap();
    let first = documents_tree(&home);
    let source_count = first.sources.len();
    let mut previous = first.selection();
    previous.select_all(false);
    assert_eq!(
        previous
            .tree()
            .sources
            .iter()
            .filter(|source| source.selectable)
            .map(|source| previous.state(&source.id))
            .collect::<Vec<_>>(),
        vec![Some(SelectionState::Excluded); source_count]
    );

    fs::create_dir_all(home.join("Videos")).unwrap();
    fs::write(home.join("Videos/three"), b"three").unwrap();
    let refreshed = documents_tree(&home);
    let videos = named(refreshed.sources.as_slice(), "Videos")
        .unwrap()
        .id
        .clone();
    let mut selection = refreshed.selection();
    selection.adopt(&previous);
    assert_eq!(selection.state(&videos), Some(SelectionState::Excluded));
}

#[cfg(target_os = "linux")]
#[test]
fn root_replacement_never_replays_an_exclusion_onto_a_different_folder() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    fs::create_dir_all(home.join("Documents/Alpha")).unwrap();
    fs::write(home.join("Documents/Alpha/one"), b"alpha").unwrap();
    let first = documents_tree(&home);
    let old_alpha = named(&first.sources, "Alpha").unwrap().id.clone();
    let mut previous = first.clone().selection();
    previous.set(&old_alpha, Decision::Exclude).unwrap();

    let old_home = fixture.path().join("old-home");
    fs::rename(&home, &old_home).unwrap();
    fs::create_dir_all(home.join("Documents/Alpha")).unwrap();
    fs::create_dir_all(home.join("Documents/Beta")).unwrap();
    fs::write(home.join("Documents/Alpha/one"), b"new alpha").unwrap();
    fs::write(home.join("Documents/Beta/two"), b"beta").unwrap();
    let replaced = documents_tree(&home);
    assert!(named(replaced.sources.as_slice(), "Alpha").is_some());
    let documents = named(replaced.sources.as_slice(), "Documents")
        .unwrap()
        .id
        .clone();
    assert_ne!(
        named(replaced.sources.as_slice(), "Documents")
            .unwrap()
            .children[0]
            .id,
        old_alpha
    );
    let mut selection = replaced.clone().selection();
    selection.adopt(&previous);
    assert_eq!(selection.state(&documents), Some(SelectionState::Excluded));
    assert_eq!(
        selection.exclusions().home(),
        &[std::path::PathBuf::from("Documents")],
        "a replaced root starts conservatively excluded, never with the old child exclusion"
    );

    // A root replacement after an all-selected review keeps the visible default.
    let mut all_selected = first.selection();
    all_selected.select_all(true);
    let mut selection = replaced.selection();
    selection.adopt(&all_selected);
    assert!(selection.exclusions().is_empty());
    assert_eq!(selection.state(&documents), Some(SelectionState::Selected));
}

#[test]
fn same_named_children_in_distinct_roots_keep_distinct_decisions() {
    let fixture = tempfile::tempdir().unwrap();
    let first_root = fixture.path().join("First/Music");
    let second_root = fixture.path().join("Second/Music");
    for root in [&first_root, &second_root] {
        fs::create_dir_all(root.join("Shared")).unwrap();
        fs::write(root.join("Shared/one"), b"shared").unwrap();
    }
    let inventory = Inventory::discover(&[
        grant(&first_root, RootRole::KnownFolder),
        grant(&second_root, RootRole::KnownFolder),
    ])
    .unwrap();
    let tree = discover_windows(&inventory, &SteamCatalog::builtin());
    assert_eq!(tree.sources.len(), 2);
    assert_ne!(tree.sources[0].id, tree.sources[1].id);
    let first_shared = named(&tree.sources[0].children, "Shared")
        .unwrap()
        .id
        .clone();
    let second_shared = named(&tree.sources[1].children, "Shared")
        .unwrap()
        .id
        .clone();
    assert_ne!(first_shared, second_shared);
    let mut previous = tree.selection();
    previous.set(&first_shared, Decision::Exclude).unwrap();

    // Refresh both roots after content changes; the same-named child in the
    // other root never inherits the first root's decision.
    for root in [&first_root, &second_root] {
        fs::write(root.join("Shared/two"), b"more").unwrap();
    }
    let inventory = Inventory::discover(&[
        grant(&first_root, RootRole::KnownFolder),
        grant(&second_root, RootRole::KnownFolder),
    ])
    .unwrap();
    let refreshed = discover_windows(&inventory, &SteamCatalog::builtin());
    let first_shared = named(&refreshed.sources[0].children, "Shared")
        .unwrap()
        .id
        .clone();
    let second_shared = named(&refreshed.sources[1].children, "Shared")
        .unwrap()
        .id
        .clone();
    let mut selection = refreshed.selection();
    selection.adopt(&previous);
    assert_eq!(
        selection.state(&first_shared),
        Some(SelectionState::Excluded)
    );
    assert_eq!(
        selection.state(&second_shared),
        Some(SelectionState::Selected)
    );
    let exclusions = selection.exclusions();
    assert_eq!(
        exclusions.windows().get(&inventory.roots[0].source_id),
        Some(&vec![std::path::PathBuf::from("Shared")])
    );
    assert!(
        exclusions
            .windows()
            .get(&inventory.roots[1].source_id)
            .is_none()
    );
}
