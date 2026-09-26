//! The Contacts app manifest (`tilo-app.toml`) declares Contacts as the
//! handler for vCard files. These tests validate that declaration against the
//! real Tilo packaging schema — so the association is checked, not eyeballed.
//!
//! Note on scope: `[providers]` is the manifest's "what file kinds this
//! package opens" field. The declaration is correct and schema-valid today;
//! the runtime piece that would *route* a `.vcf` open to this app (a
//! MIME→app resolver in the launcher) is not yet built — only the WASM
//! format-host reads `providers` today. See the crate's report for detail.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_app_package::validate::validate_manifest;
use tilo_app_package::{Kind, Manifest};

fn manifest() -> Manifest {
    let src = include_str!("../tilo-app.toml");
    Manifest::from_toml_str(src).expect("contacts tilo-app.toml parses as a manifest")
}

/// The manifest is a valid app declaring `vCard` file types and the file-chooser portal.
#[test]
fn manifest_declares_a_valid_vcard_app_with_the_file_chooser() {
    // --- phase: manifest_is_a_valid_tilo_app ---
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

    // --- phase: manifest_declares_the_vcard_file_types ---
    {
        let m = manifest();
        // The vCard MIME types both spellings of the type carry.
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

    // --- phase: manifest_declares_the_file_chooser_portal ---
    {
        // Import/Export use the host XDG file-chooser portal; the launcher only
        // plumbs the D-Bus session bus in when the manifest declares the intent.
        use tilo_app_package::manifest::PortalPolicy;
        let m = manifest();
        assert_eq!(
            m.sandbox.portals.get("file-chooser"),
            Some(&PortalPolicy::ViaHost),
            "the file-chooser portal must be declared via-host for Import/Export",
        );
    }
}
