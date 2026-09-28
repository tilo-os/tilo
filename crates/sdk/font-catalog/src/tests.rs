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
