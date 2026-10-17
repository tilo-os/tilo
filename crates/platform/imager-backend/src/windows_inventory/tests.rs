use super::*;
use std::fs;
use std::sync::atomic::AtomicBool;

fn source(path: &Path, role: RootRole) -> SelectedRoot {
    SelectedRoot {
        path: path.into(),
        role,
    }
}

#[test]
fn review_preserves_unknown_mail_drafts_and_cloud_data_until_explicit_copy() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("chosen");
    fs::create_dir_all(root.join("Drafts")).unwrap();
    fs::write(root.join("archive.pst"), b"archive bytes").unwrap();
    fs::write(root.join("Drafts/local.eml"), b"unsent draft").unwrap();
    fs::write(root.join("mystery.bin"), b"unique unknown bytes").unwrap();
    let mut inventory = Inventory::discover(&[source(&root, RootRole::OneDrive)]).unwrap();
    let id = inventory.roots[0].source_id.clone();
    let total = b"archive bytes".len() + b"unsent draft".len() + b"unique unknown bytes".len();
    let budget = inventory.budget();
    assert_eq!(budget.copy_bytes, 0);
    assert_eq!(budget.retained_bytes, total as u64);
    assert_eq!(budget.explicitly_excluded_bytes, 0);
    assert!(budget.coverage_complete);
    assert!(inventory.roots[0].entries.iter().all(|e| !e.sync_verified));
    let destination = fixture.path().join("copied");
    assert!(
        inventory
            .copy_selected(&id, &destination, &AtomicBool::new(false))
            .is_err()
    );
    assert!(!destination.exists());
    assert!(inventory.select(&id, Decision::Exclude, None).is_err());
    inventory.select(&id, Decision::Copy, None).unwrap();
    assert_eq!(inventory.budget().copy_bytes, total as u64);
    let report = inventory
        .copy_selected(&id, &destination, &AtomicBool::new(false))
        .unwrap();
    assert!(report.complete(), "{report:?}");
    assert_eq!(report.verified_bytes, total as u64);
    for path in ["archive.pst", "Drafts/local.eml", "mystery.bin"] {
        assert_eq!(
            fs::read(root.join(path)).unwrap(),
            fs::read(destination.join(path)).unwrap()
        );
    }
    inventory
        .select(
            &id,
            Decision::Exclude,
            Some("I saved an independent copy".into()),
        )
        .unwrap();
    assert_eq!(inventory.budget().explicitly_excluded_bytes, total as u64);
    assert_eq!(inventory.budget().copy_bytes, 0);
    assert!(
        root.join("mystery.bin").exists(),
        "exclusion never deletes originals"
    );
    let accounts = crate::account_transfer::discover_windows_sources(&inventory);
    assert_eq!(accounts.candidates.len(), 1);
    assert!(!accounts.candidates[0].can_import_credentials);
    assert!(accounts.candidates[0].needs_sign_in);
}

#[test]
fn steam_external_library_is_only_a_suggestion_and_requires_its_own_grant() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("Steam");
    let external = fixture.path().join("another-library");
    fs::create_dir_all(root.join("steamapps")).unwrap();
    fs::create_dir_all(root.join("userdata/123/456/remote")).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("private-save"), b"external unique sentinel").unwrap();
    fs::write(root.join("userdata/123/456/remote/save.dat"), b"local save").unwrap();
    let config = format!(
        "\"libraryfolders\" {{ \"0\" {{ \"path\" \"{}\" }} }}",
        external.display()
    );
    fs::write(root.join("steamapps/libraryfolders.vdf"), config).unwrap();
    let inventory = Inventory::discover(&[source(&root, RootRole::Steam)]).unwrap();
    assert_eq!(inventory.roots.len(), 1);
    assert_eq!(inventory.roots[0].additional_grants, vec![external.clone()]);
    assert!(!inventory.budget().coverage_complete);
    assert!(inventory.roots[0].gaps.iter().any(|gap| {
        gap.reason
            .contains("requires an independent exact path grant")
    }));
    assert!(
        inventory.roots[0]
            .entries
            .iter()
            .all(|e| !e.relative.starts_with(&external))
    );
    assert!(
        inventory.roots[0]
            .entries
            .iter()
            .any(|e| e.category == Category::SteamUserdata)
    );
    assert_eq!(
        fs::read(external.join("private-save")).unwrap(),
        b"external unique sentinel"
    );
    let granted = Inventory::discover(&[
        source(&root, RootRole::Steam),
        source(&external, RootRole::SteamLibrary),
    ])
    .unwrap();
    assert_eq!(granted.roots.len(), 2);
    assert_eq!(
        granted.roots[1].entries[0].category,
        Category::PotentiallyUnique
    );
    assert_eq!(granted.roots[1].decision, Decision::RetainOriginal);
    assert!(
        Inventory::discover(&[
            source(&root, RootRole::Steam),
            source(&root.join("userdata"), RootRole::Other),
        ])
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn links_and_ungranted_sources_are_truthful_coverage_gaps_not_zero_cost_copies() {
    use std::os::unix::fs::symlink;
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("selected");
    let external = fixture.path().join("not-selected");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&external).unwrap();
    fs::write(external.join("private"), b"never inventory outside grant").unwrap();
    symlink(&external, root.join("redirect")).unwrap();
    fs::write(root.join("local.ost"), b"might include unsent drafts").unwrap();
    let mut inventory = Inventory::discover(&[source(&root, RootRole::Outlook)]).unwrap();
    assert!(!inventory.budget().coverage_complete);
    assert_eq!(inventory.roots[0].gaps.len(), 1);
    assert_eq!(inventory.roots[0].entries.len(), 2);
    let link = inventory.roots[0]
        .entries
        .iter()
        .find(|e| e.relative == Path::new("redirect"))
        .unwrap();
    assert!(!link.copy_eligible);
    let id = inventory.roots[0].source_id.clone();
    inventory.select(&id, Decision::Copy, None).unwrap();
    let destination = fixture.path().join("output");
    let report = inventory
        .copy_selected(&id, &destination, &AtomicBool::new(false))
        .unwrap();
    assert!(!report.complete());
    assert_eq!(report.skipped.len(), 1);
    assert!(!destination.join("redirect").exists());
    assert_eq!(
        fs::read(destination.join("local.ost")).unwrap(),
        b"might include unsent drafts"
    );
    assert_eq!(
        fs::read(external.join("private")).unwrap(),
        b"never inventory outside grant"
    );
}

#[test]
fn bounded_native_metadata_and_steam_parser_preserve_redirected_paths() {
    let suggestions = native::parse(
        br#"[
        {"kind":"known","path":"D:\\OneDrive - Work\\Documents","label":"Documents"},
        {"kind":"onedrive","path":"D:\\OneDrive - Work","label":"Business1"}
    ]"#,
    )
    .unwrap();
    assert_eq!(
        suggestions[0].path,
        PathBuf::from(r"D:\OneDrive - Work\Documents")
    );
    assert_eq!(suggestions[1].role, RootRole::OneDrive);
    assert!(native::parse(b"{bad json}").is_err());
    assert!(native::parse(&vec![b' '; 256 * 1024 + 1]).is_err());
    let modern = r#""libraryfolders" { "0" { "path" "D:\\SteamLibrary" "apps" { "123" "55" } } }"#;
    assert_eq!(
        metadata::libraries(modern).unwrap(),
        vec![PathBuf::from(r"D:\SteamLibrary")]
    );
    let old = r#""LibraryFolders" { "TimeNextStatsReport" "5" "1" "E:\\Games" }"#;
    assert_eq!(
        metadata::libraries(old).unwrap(),
        vec![PathBuf::from(r"E:\Games")]
    );
    for broken in [
        r#""libraryfolders" { "0" { "path" "C:\\Games" }"#,
        r#""path" "relative""#,
        "{",
    ] {
        assert!(metadata::libraries(broken).is_err(), "{broken}");
    }
    assert_eq!(
        category(
            RootRole::Steam,
            Path::new("steamapps/common/Game/mods/local")
        ),
        Category::GameMod
    );
    assert_eq!(
        category(RootRole::Other, Path::new("unknown.dat")),
        Category::PotentiallyUnique
    );
    assert_eq!(
        category(RootRole::Outlook, Path::new("local.ost")),
        Category::MailCacheOrDrafts
    );
}

#[test]
fn selected_grants_and_category_exclusions_require_explicit_review() {
    assert!(Inventory::discover(&[source(Path::new("relative-root"), RootRole::Other)]).is_err());
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    fs::write(root.join("unique.bin"), b"unique").unwrap();
    fs::write(root.join("archive.pst"), b"mail").unwrap();
    let mut inventory = Inventory::discover(&[source(&root, RootRole::Other)]).unwrap();
    let id = inventory.roots[0].source_id.clone();
    assert!(inventory.source_folder_uri("not-selected").is_err());
    assert!(
        inventory
            .source_folder_uri(&id)
            .unwrap()
            .starts_with("file:")
    );
    for reason in [None, Some("  ".to_string())] {
        assert!(
            inventory
                .select_category(&id, Category::PotentiallyUnique, Decision::Exclude, reason,)
                .is_err()
        );
    }
    assert!(
        inventory
            .select_category(
                &id,
                Category::GameSave,
                Decision::Exclude,
                Some("not present".into()),
            )
            .is_err()
    );
    assert_eq!(inventory.budget().retained_bytes, 10);
    assert_eq!(inventory.budget().explicitly_excluded_bytes, 0);
    inventory
        .select_category(
            &id,
            Category::MailArchive,
            Decision::Exclude,
            Some("Independent verified backup".into()),
        )
        .unwrap();
    assert_eq!(inventory.budget().retained_bytes, 6);
    assert_eq!(inventory.budget().explicitly_excluded_bytes, 4);
    assert_eq!(fs::read(root.join("archive.pst")).unwrap(), b"mail");
    inventory
        .select(&id, Decision::RetainOriginal, None)
        .unwrap();
    assert_eq!(inventory.budget().retained_bytes, 10);
    assert_eq!(inventory.budget().explicitly_excluded_bytes, 0);
    assert!(inventory.roots[0].category_decisions.is_empty());
}

#[cfg(unix)]
#[test]
fn changed_selected_root_refuses_copy_and_open_without_touching_replacement() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("selected");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("local.bin"), b"selected bytes").unwrap();
    let mut inventory = Inventory::discover(&[source(&root, RootRole::Other)]).unwrap();
    let id = inventory.roots[0].source_id.clone();
    inventory.select(&id, Decision::Copy, None).unwrap();
    fs::rename(&root, fixture.path().join("moved")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("replacement.bin"), b"ungranted replacement").unwrap();
    assert!(inventory.source_folder_uri(&id).is_err());
    let destination = fixture.path().join("output");
    assert!(
        inventory
            .copy_selected(&id, &destination, &AtomicBool::new(false))
            .is_err()
    );
    assert!(!destination.exists());
    assert_eq!(
        fs::read(root.join("replacement.bin")).unwrap(),
        b"ungranted replacement"
    );
    assert_eq!(
        fs::read(fixture.path().join("moved/local.bin")).unwrap(),
        b"selected bytes"
    );
}

#[cfg(not(windows))]
#[test]
fn native_suggestions_are_explicitly_unavailable_off_windows() {
    assert_eq!(
        suggestions().unwrap_err(),
        "native Windows root metadata requires Windows"
    );
}
