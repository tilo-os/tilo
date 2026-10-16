use super::*;

#[test]
fn the_catalog_parses_and_names_its_default() {
    let catalog: Catalog = toml::from_str(include_str!("../catalog.toml")).unwrap();
    assert_eq!(catalog.schema, SCHEMA);
    assert_eq!(all().len(), catalog.wallpaper.len());
    assert!(all().len() >= 6, "a picker needs a real choice");
    assert_eq!(
        default_wallpaper().map(|w| w.id.as_str()),
        Some(catalog.default.as_str())
    );
}

/// Every entry is complete: a stable id the setting can hold, a free licence
/// stated with its source, and installed files pinned by size and digest at
/// the shapes the desktop and the picker draw.
#[test]
fn every_wallpaper_is_pinned_licensed_and_credited() {
    let mut ids = std::collections::BTreeSet::new();
    let mut files = std::collections::BTreeSet::new();
    let hex = |digest: &str| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    for wallpaper in all() {
        assert!(
            !wallpaper.id.is_empty()
                && wallpaper.id != CUSTOM
                && wallpaper
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{}: ids are lowercase words joined by hyphens",
            wallpaper.id
        );
        assert!(
            ids.insert(wallpaper.id.clone()),
            "{} is listed twice",
            wallpaper.id
        );
        assert!(
            ["CC0-1.0", "public-domain"].contains(&wallpaper.license.as_str()),
            "{}: only public-domain and CC0 pictures ship",
            wallpaper.id
        );
        for url in [
            &wallpaper.license_url,
            &wallpaper.source_url,
            &wallpaper.original_url,
        ] {
            assert!(url.starts_with("https://"), "{}: {url}", wallpaper.id);
        }
        assert!(!wallpaper.author.is_empty() && !wallpaper.title.is_empty());
        assert!(hex(&wallpaper.original_sha256), "{}", wallpaper.id);
        for (asset, size) in [
            (&wallpaper.picture, (2944, 1840)),
            (&wallpaper.thumbnail, (480, 300)),
        ] {
            assert_eq!((asset.width, asset.height), size, "{}", asset.file);
            assert!(
                hex(&asset.sha256) && asset.bytes > 0,
                "{} is not pinned",
                asset.file
            );
            assert!(asset.bytes <= MAX_ASSET_BYTES, "{}", asset.file);
            assert!(
                files.insert(asset.file.clone()),
                "{} is listed twice",
                asset.file
            );
        }
    }
}

#[test]
fn a_setting_value_chooses_a_wallpaper() {
    let default = default_wallpaper().unwrap();
    let other = all().iter().find(|w| w.id != default.id).unwrap();
    assert_eq!(choice(None), Some(Choice::Bundled(default)));
    assert_eq!(choice(Some(&other.id)), Some(Choice::Bundled(other)));
    assert_eq!(choice(Some(CUSTOM)), Some(Choice::Custom));
    // A newer device's picture this build does not ship.
    assert_eq!(
        choice(Some("not-shipped-here")),
        Some(Choice::Bundled(default))
    );
    assert_eq!(value_of(Choice::Bundled(other)), other.id);
    assert_eq!(value_of(Choice::Custom), CUSTOM);
    let values = values();
    assert_eq!(values.len(), all().len() + 1);
    assert!(values.contains(&CUSTOM));
}

/// An installed file is found in the XDG data directories and decodes at
/// the size the catalog gives; one that is missing is `None`, not a panic.
#[test]
fn decoding_reads_only_what_is_installed() {
    let missing = Asset {
        file: "surely-not-installed-anywhere.webp".into(),
        width: 4,
        height: 2,
        bytes: 1,
        sha256: String::new(),
    };
    assert_eq!(installed(&missing), None);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.png");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([90, 90, 90, 255]))
        .save(&path)
        .unwrap();
    let asset = Asset {
        file: "tiny.png".into(),
        width: 4,
        height: 2,
        bytes: std::fs::metadata(&path).unwrap().len(),
        sha256: String::new(),
    };
    let decoded = decode(&path, &asset).unwrap();
    assert_eq!((decoded.width, decoded.height), (4, 2));
    assert_eq!(decoded.rgba.len(), 4 * 2 * 4);
}
