//! What the settings provider publishes, as `tilolang-tilo` reads it for a view: written beside
//! this crate (`published.toml`).

/// The committed surface file is what this crate describes. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_surface_file_is_what_this_crate_describes() {
    let text = super::surface_file().expect("the surface writes");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("published.toml");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        std::fs::read_to_string(&path).ok().as_deref(),
        Some(text.as_str()),
        "crates/desktop/settings-sections/published.toml is not what this crate describes; \
         regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

/// Every section is one resource where it is kept, set field by field: no generic edit, a
/// setter of each field that is not read only, and the person's sections apart from the
/// device's.
#[test]
fn every_section_is_one_resource_set_field_by_field() {
    let (resources, roots) = super::surface();
    assert_eq!(resources.len(), roots.len());
    let mut published = std::collections::BTreeSet::new();
    for (resource, root) in resources.iter().zip(&roots) {
        assert_eq!(resource.kind, root.kind);
        assert!(published.insert(root.published.clone()), "{root:?}");
        for generic in ["field-edit", "apply", "group"] {
            assert!(
                !resource.verbs.iter().any(|verb| verb.id == generic),
                "{}: {generic}",
                resource.kind
            );
        }
    }
    let at = |name: &str| {
        roots
            .iter()
            .position(|root| root.name == name)
            .unwrap_or_else(|| panic!("{name} is a root"))
    };
    assert_eq!(roots[at("appearance")].published, "settings/appearance");
    assert_eq!(
        roots[at("power")].published,
        "devices/{device}/settings/power"
    );
    assert_eq!(
        roots[at("continuity")].published,
        "devices/{device}/settings/continuity",
        "continuity and whether displays' input is taken on its own are one place"
    );
    assert_eq!(roots[at("agent_models")].published, "settings/agent-models");
    let power = resources[at("power")].clone().with_setters();
    let setter = |id: &str| {
        power
            .verbs
            .iter()
            .find(|verb| verb.id == id)
            .unwrap_or_else(|| panic!("{id} sets a power setting"))
    };
    let params = |id: &str| -> Vec<&str> {
        setter(id)
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect()
    };
    assert_eq!(
        params("set-lock-minutes"),
        ["value"],
        "a longer lock is set whole"
    );
    assert_eq!(params("set-suspend-minutes"), ["value", "edit"]);
}
