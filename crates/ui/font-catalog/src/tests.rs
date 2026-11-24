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
    for asset in manifest()
        .unwrap()
        .font
        .iter()
        .filter(|a| a.scope.fixtures())
    {
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

#[test]
fn a_reader_checks_a_font_s_size_without_reading_it() {
    // Same size, none of the pinned bytes: the size check accepts it, as a
    // process that sets text must without reading the file; the hash refuses it.
    let asset = manifest()
        .unwrap()
        .font
        .iter()
        .min_by_key(|a| a.bytes)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = asset.path_in(dir.path()).unwrap();
    let zeros = vec![0_u8; usize::try_from(asset.bytes).unwrap()];
    std::fs::write(&path, &zeros).unwrap();
    assert_eq!(asset.sized_in(dir.path()).unwrap(), path);
    assert!(
        asset
            .verify(dir.path())
            .unwrap_err()
            .problem
            .contains("SHA-256")
    );
    std::fs::write(&path, &zeros[1..]).unwrap();
    assert!(
        asset
            .sized_in(dir.path())
            .unwrap_err()
            .problem
            .contains("bytes")
    );
}
