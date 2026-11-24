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

/// The checkout's font sources: what this crate's copies are kept identical to.
fn checkout_sources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/ui-core/assets/fonts")
}

/// Every file below `root`, relative to it.
fn files_below(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else {
                files.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_crate_carries_the_checkout_s_artifact_list_and_notices() {
    // The manifest, the artifact list and the notices a published crate prepares its catalog
    // from are the checkout's, byte for byte: nothing else is in either.
    let carried = Path::new(env!("CARGO_MANIFEST_DIR")).join("catalog");
    let sources = checkout_sources();
    assert_eq!(
        std::fs::read_to_string(sources.join("manifest.toml")).unwrap(),
        MANIFEST
    );
    let mut expected: Vec<PathBuf> = files_below(&sources.join("LICENSES"))
        .into_iter()
        .map(|file| Path::new("LICENSES").join(file))
        .collect();
    expected.extend(["PROVENANCE.md", "artifacts.json"].map(PathBuf::from));
    expected.sort();
    assert_eq!(files_below(&carried), expected);
    for file in expected {
        assert!(
            std::fs::read(carried.join(&file)).unwrap()
                == std::fs::read(sources.join(&file)).unwrap(),
            "catalog/{} differs from the checkout's; copy it from {}",
            file.display(),
            sources.display()
        );
    }
}

#[cfg(feature = "prepare")]
mod prepare {
    use super::*;
    use crate::prepare::{self, Selection};

    #[test]
    fn the_artifact_list_pins_every_font_of_the_manifest() {
        let artifacts = prepare::artifacts().unwrap();
        assert_eq!(artifacts.len(), manifest().unwrap().font.len());
        assert!(
            artifacts
                .iter()
                .all(|artifact| artifact.url.starts_with("https://code.tilo.so/")
                    && artifact.url.contains(&artifact.sha256))
        );
    }

    #[test]
    fn a_catalog_is_named_as_the_checkout_s_tool_names_it() {
        // The build prepared the checkout's fixtures catalog with tools/font-catalog.py; the
        // same catalog prepared here has its name, so either finds the other's in a shared cache.
        assert_eq!(
            checkout_root().file_name().unwrap().to_str().unwrap(),
            Selection::Fixtures.catalog_name()
        );
        prepare::verify(&checkout_root(), Selection::Fixtures).unwrap();
    }

    /// An artifact cache holding `selection`'s fonts, as a fetch leaves them.
    fn seeded_cache(selection: Selection) -> tempfile::TempDir {
        let cache = tempfile::tempdir().unwrap();
        for asset in selection.fonts().unwrap() {
            let path = cache.path().join(&asset.sha256).join(&asset.file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::copy(asset.path_in(&checkout_root()).unwrap(), path).unwrap();
        }
        cache
    }

    #[test]
    fn a_catalog_prepared_from_the_cache_is_found_where_readers_look() {
        let cache = seeded_cache(Selection::Product);
        let root = prepare::prepared(cache.path(), Selection::Product, true, |_| {
            panic!("every font is cached")
        })
        .unwrap();
        assert_eq!(
            root,
            cache
                .path()
                .join("catalogs")
                .join(Selection::Product.catalog_name())
        );
        assert_eq!(find_root(prepared_catalogs(cache.path())), Some(root.clone()));
        prepare::verify(&root, Selection::Product).unwrap();
        // Only the catalog itself is left in `catalogs`: its staging directory is gone.
        assert_eq!(std::fs::read_dir(cache.path().join("catalogs")).unwrap().count(), 1);
        // Asked again, the published catalog is the answer.
        let again = prepare::prepared(cache.path(), Selection::Product, true, |_| {
            panic!("nothing is fetched twice")
        })
        .unwrap();
        assert_eq!(again, root);
    }

    #[test]
    fn a_cached_font_that_differs_from_its_pin_fails_and_is_kept() {
        let cache = seeded_cache(Selection::Product);
        let asset = Selection::Product
            .fonts()
            .unwrap()
            .into_iter()
            .min_by_key(|asset| asset.bytes)
            .unwrap();
        let path = cache.path().join(&asset.sha256).join(&asset.file);
        let mut bytes = std::fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let error = prepare::prepared(cache.path(), Selection::Product, false, |_| {
            panic!("a corrupt font is not fetched again")
        })
        .unwrap_err();
        assert!(error.problem.contains("SHA-256"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(find_root(prepared_catalogs(cache.path())).is_none());
    }

    #[test]
    fn offline_without_the_fonts_says_what_to_do() {
        let cache = tempfile::tempdir().unwrap();
        let error = prepare::prepared(cache.path(), Selection::Product, true, |_| {
            panic!("offline fetches nothing")
        })
        .unwrap_err();
        assert!(error.problem.contains(prepare::OFFLINE_ENV), "{error}");
        assert!(error.problem.contains("network"), "{error}");
        assert!(find_root(prepared_catalogs(cache.path())).is_none());
    }

    #[test]
    fn a_bundle_carries_the_product_catalog_alone() {
        // From the fixtures catalog, which holds the OS's GPL cut too, a bundle takes only what
        // any app may ship, and a second bundle into the same place keeps it.
        let app = tempfile::tempdir().unwrap();
        let fonts = app.path().join("fonts");
        prepare::bundle(&checkout_root(), &fonts, Selection::Product).unwrap();
        prepare::verify(&fonts, Selection::Product).unwrap();
        assert!(!fonts.join("NewCM10-Regular.otf").exists());
        assert_eq!(find_root([fonts.clone()]), Some(fonts.clone()));
        prepare::bundle(&checkout_root(), &fonts, Selection::Product).unwrap();
        assert_eq!(
            std::fs::read_dir(app.path()).unwrap().count(),
            1,
            "nothing but fonts/ is left beside the program"
        );
    }
}
