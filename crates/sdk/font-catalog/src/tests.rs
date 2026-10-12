use super::*;

#[test]
fn the_compiled_in_manifest_parses() {
    let manifest = manifest().unwrap();
    assert!(manifest.font.iter().any(|a| a.scope == Scope::Product));
    assert!(manifest.font.iter().any(|a| a.scope == Scope::Fixture));
}

#[test]
fn the_checkout_holds_the_pinned_catalog() {
    let root = checkout_root();
    check_manifest(&root).unwrap();
    for asset in &manifest().unwrap().font {
        asset.verify(&root).unwrap();
    }
}

#[test]
fn a_different_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.toml"), "schema = 1\n").unwrap();
    let error = check_manifest(dir.path()).unwrap_err();
    assert!(error.problem.contains("differs"), "{error}");
}

#[test]
fn a_file_that_differs_from_its_pin_is_refused() {
    let asset = manifest()
        .unwrap()
        .font
        .iter()
        .min_by_key(|a| a.bytes)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = asset.path_in(dir.path()).unwrap();
    let mut bytes = std::fs::read(asset.path_in(&checkout_root()).unwrap()).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    assert!(
        asset
            .verify(dir.path())
            .unwrap_err()
            .problem
            .contains("SHA-256")
    );
    assert!(asset.verify_bytes(&path, &bytes).is_err());
    bytes.pop();
    std::fs::write(&path, &bytes).unwrap();
    assert!(
        asset
            .verify(dir.path())
            .unwrap_err()
            .problem
            .contains("bytes")
    );
}

#[test]
fn manifest_paths_stay_inside_the_catalog() {
    let asset = |file: &str| Asset {
        scope: Scope::Product,
        browser_startup: false,
        file: file.to_string(),
        bytes: 0,
        sha256: String::new(),
        reserved_aliases: Vec::new(),
        faces: Vec::new(),
    };
    let root = Path::new("/catalog");
    assert!(asset("../escape.ttf").path_in(root).is_err());
    assert!(asset("/etc/passwd").path_in(root).is_err());
    assert_eq!(
        asset("a/b.ttf").path_in(root).unwrap(),
        root.join("a/b.ttf")
    );
}

#[test]
fn the_standalone_catalog_carries_only_new_computer_modern_s_gust_cuts() {
    let manifest = manifest().unwrap();
    let scope = |file: &str| {
        manifest
            .font
            .iter()
            .find(|asset| asset.file == file)
            .unwrap_or_else(|| panic!("{file} is in the catalog"))
            .scope
    };
    // NewCM10-Regular is GPL-3.0-or-later with the font exception: the Tilo
    // OS ships it, a third-party app's bundle does not.
    assert_eq!(scope("NewCM10-Regular.otf"), Scope::Os);
    assert!(Scope::Os.os() && !Scope::Os.standalone());
    for file in [
        "NewCM10-Book.otf",
        "NewCM10-BookItalic.otf",
        "NewCM10-Italic.otf",
        "NewCM10-Bold.otf",
        "NewCM10-BoldItalic.otf",
        "NewCM08-Book.otf",
    ] {
        assert!(scope(file).standalone(), "{file} is GUST-licensed");
    }
    assert!(!Scope::Fixture.os() && !Scope::Fixture.standalone());
}

/// The smallest pinned asset, and its pinned bytes from the checkout.
fn smallest_asset() -> (&'static Asset, Vec<u8>) {
    let asset = manifest()
        .unwrap()
        .font
        .iter()
        .min_by_key(|a| a.bytes)
        .unwrap();
    let bytes = std::fs::read(asset.path_in(&checkout_root()).unwrap()).unwrap();
    (asset, bytes)
}

#[test]
fn verify_trust_still_rejects_a_modified_font() {
    let (asset, mut bytes) = smallest_asset();
    let dir = tempfile::tempdir().unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(asset.path_in(dir.path()).unwrap(), &bytes).unwrap();
    let error = asset.verify_with(dir.path(), Trust::Verify).unwrap_err();
    assert!(error.problem.contains("SHA-256"), "{error}");
}

#[test]
fn immutable_trust_still_hashes_a_file_that_is_not_immutable() {
    // A caller that claims `Immutable` for a writable root gains nothing:
    // the production probe refuses the file, so it is hashed and refused.
    let (asset, mut bytes) = smallest_asset();
    let dir = tempfile::tempdir().unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    let path = asset.path_in(dir.path()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(!file_is_immutable(&path));
    let error = asset.verify_with(dir.path(), Trust::Immutable).unwrap_err();
    assert!(error.problem.contains("SHA-256"), "{error}");
}

#[test]
fn immutable_trust_reads_no_font_bytes() {
    // Same size, none of the pinned bytes: only a reader that never hashes
    // the file accepts it, which is what an immutable file earns.
    let (asset, bytes) = smallest_asset();
    let dir = tempfile::tempdir().unwrap();
    let path = asset.path_in(dir.path()).unwrap();
    let zeros = vec![0_u8; bytes.len()];
    std::fs::write(&path, &zeros).unwrap();
    assert_eq!(
        asset
            .verify_probed(dir.path(), Trust::Immutable, |_| true)
            .unwrap(),
        path
    );
    // The size is still checked without reading.
    std::fs::write(&path, &zeros[1..]).unwrap();
    let error = asset
        .verify_probed(dir.path(), Trust::Immutable, |_| true)
        .unwrap_err();
    assert!(error.problem.contains("bytes"), "{error}");
    // The probe decides per file: one it refuses is hashed after all.
    std::fs::write(&path, &zeros).unwrap();
    assert!(
        asset
            .verify_probed(dir.path(), Trust::Immutable, |_| false)
            .unwrap_err()
            .problem
            .contains("SHA-256")
    );
    assert!(
        asset
            .verify_probed(dir.path(), Trust::Verify, |_| true)
            .unwrap_err()
            .problem
            .contains("SHA-256")
    );
}

#[test]
fn writable_and_checkout_roots_earn_no_trust() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.toml"), MANIFEST).unwrap();
    assert_eq!(Trust::of_root(dir.path()), Trust::Verify);
    assert!(!file_is_immutable(dir.path()));
    assert_eq!(Trust::of_root(&checkout_root()), Trust::Verify);
    assert_eq!(Trust::of_root(&dir.path().join("absent")), Trust::Verify);
}

#[cfg(unix)]
#[test]
fn a_symlinked_file_is_not_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("/", &link).unwrap();
    assert!(!file_is_immutable(&link));
}
