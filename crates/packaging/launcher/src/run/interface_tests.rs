//! A build that speaks another version of an interface than the system serves is refused, with
//! what the system serves and how to remove the app.

use super::*;
use tilo_app_package::interfaces::ProvidedInterfaces;

#[test]
fn a_build_speaking_another_interface_version_is_refused() {
    let settings: ManifestView = crate::manifest::test_manifest(
        "id = \"so.tilo.settings\"\nversion = \"2\"\nkind = \"app\"",
        "[entrypoint]\nbin = \"bin/settings\"\n\
         [sandbox.services]\nsystem = \"required@2\"\ncollab = \"optional@3\"\n",
    );
    let os = |system: u32, runtime: u32| {
        ProvidedInterfaces::new([("system", system), ("runtime", runtime)], ["collab"])
    };
    let unmet = interfaces_unmet(&settings, Some(&os(1, 3)));
    assert_eq!(unmet.len(), 1);
    assert_eq!(unmet[0].requirement, "system@2");
    let optional = interfaces_unmet(&settings, Some(&os(2, 4)));
    assert_eq!(
        optional[0].requirement, "collab@3",
        "an optional service is spoken at its own version where it is there"
    );
    assert!(interfaces_unmet(&settings, Some(&os(2, 3))).is_empty());
    assert!(interfaces_unmet(&settings, None).is_empty());

    let message = RunError::InterfacesUnmet {
        package: "so.tilo.settings".into(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    }
    .to_string();
    assert!(message.contains("system@2 (this system provides system@1)"));
    assert!(message.contains("app remove so.tilo.settings"));
}
