//! The Contacts manifest (`tilo-app.toml`) declares Contacts as the handler for
//! vCard files. These tests validate that declaration against the real Tilo
//! packaging schema.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_app_package::validate::validate_manifest;
use tilo_app_package::{Kind, Manifest};

fn manifest() -> Manifest {
    let src = include_str!("../tilo-app.toml");
    Manifest::from_toml_str(src).expect("contacts tilo-app.toml parses as a manifest")
}

#[test]
fn manifest_declares_a_valid_vcard_app_with_the_file_chooser() {
    {
        let m = manifest();
        assert_eq!(m.package.id, "so.tilo.contacts");
        assert_eq!(m.package.kind, Kind::App, "contacts ships as an app");
        // Passes the real schema validation (identity, entrypoint, runtime form…).
        validate_manifest(&m).expect("the contacts manifest is schema-valid");
        assert!(
            m.entrypoint
                .as_ref()
                .is_some_and(|e| e.bin.ends_with("contacts")),
            "the app entrypoint launches the contacts binary",
        );
    }

    {
        let m = manifest();
        for mime in ["text/vcard", "text/x-vcard"] {
            assert!(
                m.providers.mime.iter().any(|t| t == mime),
                "contacts must claim the {mime} MIME type, got {:?}",
                m.providers.mime,
            );
        }
        // …and the file extensions.
        for ext in ["vcf", "vcard"] {
            assert!(
                m.providers.ext.iter().any(|t| t == ext),
                "contacts must claim the .{ext} extension, got {:?}",
                m.providers.ext,
            );
        }
    }

    {
        // Import and export use the host's XDG file chooser, which the launcher only
        // connects when the manifest declares the intent.
        use tilo_app_package::manifest::PortalPolicy;
        let m = manifest();
        assert_eq!(
            m.sandbox.portals.get("file-chooser"),
            Some(&PortalPolicy::ViaHost),
            "the file-chooser portal must be declared via-host for Import/Export",
        );
    }
}
