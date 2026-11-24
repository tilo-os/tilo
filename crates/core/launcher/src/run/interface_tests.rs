//! A build that speaks another version of an interface than the system serves is
//! refused, with what the system serves and how to remove the app.

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

    let message = LaunchError::InterfacesUnmet {
        package: "so.tilo.settings".into(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    }
    .to_string();
    assert!(message.contains("system@2 (this system provides system@1)"));
    assert!(message.contains("app remove so.tilo.settings"));
}

#[test]
fn a_build_speaking_the_previous_runtime_version_runs_and_one_before_it_is_refused() {
    let current = runtime_portal::RUNTIME_INTERFACE_VERSION;
    let oldest = runtime_portal::OLDEST_RUNTIME_INTERFACE_VERSION;
    let pinned = |version: u32| {
        let tail = format!(
            "[entrypoint]\nbin = \"bin/notes\"\n[sandbox.services]\n\
             collab = \"optional@{version}\"\n"
        );
        crate::manifest::test_manifest(
            "id = \"so.tilo.notes\"\nversion = \"2\"\nkind = \"app\"",
            &tail,
        )
    };
    let os = |oldest: u32| {
        ProvidedInterfaces::new([("runtime", current)], ["collab"]).with_runtime_oldest(oldest)
    };

    assert!(
        interfaces_unmet(&pinned(oldest), Some(&os(oldest))).is_empty(),
        "an app built against the oldest served runtime version runs"
    );
    assert!(
        !interfaces_unmet(&pinned(oldest), Some(&os(current))).is_empty(),
        "an image that serves only its current version refuses it"
    );

    let too_old = oldest.saturating_sub(1);
    let unmet = interfaces_unmet(&pinned(too_old), Some(&os(oldest)));
    assert_eq!(unmet.len(), 1);
    let message = LaunchError::InterfacesUnmet {
        package: "so.tilo.notes".into(),
        unmet: tilo_app_package::interfaces::describe_unmet(&unmet),
    }
    .to_string();
    assert!(
        message.contains(&format!(
            "collab@{too_old} (this system provides collab@{oldest} to collab@{current})"
        )),
        "{message}"
    );
    assert!(message.contains("app remove so.tilo.notes"), "{message}");
}
