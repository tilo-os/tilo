use tilo_install_artifacts::{bundled_catalog_json, bundled_selections, catalog_data};

#[test]
fn published_unreleased_catalog_has_no_authenticated_selections() {
    let catalog = catalog_data::parse_catalog(bundled_catalog_json()).unwrap();
    if catalog.tag == "unreleased" {
        assert!(catalog.releases.is_empty());
        assert!(bundled_selections().unwrap().is_empty());
    } else {
        assert!(catalog.tag.starts_with("imager-v"));
    }
}

#[test]
fn duplicate_and_positional_catalogs_are_rejected() {
    let duplicate = r#"{"schema":1,"schema":1,"tag":"unreleased","releases":[]}"#;
    assert!(catalog_data::parse_catalog(duplicate).is_err());
    assert!(catalog_data::parse_catalog(r#"[1,"unreleased",[]]"#).is_err());
    let valid =
        catalog_data::parse_catalog(r#"{"schema":1,"tag":"unreleased","releases":[]}"#).unwrap();
    assert_eq!(valid.schema, 1);
    assert_eq!(valid.tag, "unreleased");
    assert!(valid.releases.is_empty());
}
