//! Exercise unsupported behavior on the native test host as well.
use super::*;
use std::sync::atomic::AtomicBool;

fn authorized_fixture(identity: &DestinationIdentity) -> ImportConsent {
    let mut issuer = ImportAuthority::default();
    issuer
        .approve("job", "account", &[], "missing", identity, "device")
        .unwrap();
    issuer
        .consume("job", "account", &[], "missing", identity, "device")
        .unwrap()
}

#[test]
fn unsupported_imports_never_touch_paths_or_invoke_callbacks() {
    let identity: DestinationIdentity = serde_json::from_str(r#"{"device":1,"inode":2}"#).unwrap();
    let cancel = AtomicBool::new(false);
    let progress = |_: TransferProgress| panic!("unsupported backend must not start work");
    let mover: MoveInto = |_, _| panic!("unsupported compensation must not move files");
    assert!(unsupported::pin_destination("not-a-directory", std::iter::empty()).is_err());
    assert!(unsupported::compensate("missing", &identity, &identity, mover).is_err());
    assert!(unsupported::restore("missing", "missing-trash", &identity, &identity, mover).is_err());
    for result in [
        unsupported::execute(authorized_fixture(&identity), &cancel, progress, &[]),
        unsupported::execute_at(
            Path::new("missing-socket"),
            authorized_fixture(&identity),
            &cancel,
            progress,
            &[],
        ),
    ] {
        let TransferOutcome::Failed { partial, reason } = result else {
            panic!("unsupported backend must fail");
        };
        assert!(partial.is_empty());
        assert!(!reason.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn unsupported_cloud_client_fails_without_a_native_socket() {
    use super::unsupported_cloud as portable;
    assert!(portable::default_files_socket().is_none());
    assert_eq!(
        portable::CloudClient::connect("missing-socket")
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Unsupported,
    );
    let mut client = portable::CloudClient;
    assert_eq!(
        client
            .import(
                "account",
                portable::ImportRequest::Browse {
                    folder: None,
                    offset: 0
                }
            )
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported,
    );
    assert_eq!(
        client
            .file(
                "account",
                portable::FileRequest::Describe {
                    path: "file".into()
                }
            )
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported,
    );
    // The portable view keeps the native service's persisted entry encoding.
    let native = cloud::ImportEntry {
        id: "opaque".into(),
        name: "document".into(),
        kind: cloud::ImportKind::NativeExport,
        revision: Some("revision".into()),
        size: Some(17),
        content_writable: false,
    };
    let value = serde_json::to_value(&native).unwrap();
    let portable: portable::ImportEntry = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(portable).unwrap(), value);
}

#[test]
fn consent_requires_local_approval_and_cannot_be_replayed() {
    let identity = DestinationIdentity {
        device: 7,
        inode: 11,
        modified: None,
    };
    let mut issuer = ImportAuthority::default();
    assert!(
        issuer
            .consume("job", "account", &[], "folder", &identity, "device")
            .is_err()
    );
    issuer
        .approve("job", "account", &[], "folder", &identity, "device")
        .unwrap();
    assert!(
        issuer
            .approve("job", "account", &[], "folder", &identity, "device")
            .is_err()
    );
    assert!(
        issuer
            .approve(
                "job",
                "different-account",
                &[],
                "folder",
                &identity,
                "device"
            )
            .is_err()
    );
    let consent = issuer
        .consume("job", "account", &[], "folder", &identity, "device")
        .unwrap();
    assert!(
        issuer
            .consume("job", "account", &[], "folder", &identity, "device")
            .is_err()
    );
    assert!(
        issuer
            .approve("job", "account", &[], "folder", &identity, "device")
            .is_err()
    );
    let mut other_issuer = ImportAuthority::default();
    assert!(
        other_issuer
            .consume("job", "account", &[], "folder", &identity, "device")
            .is_err()
    );
    let payload = consent.into_payload();
    assert_eq!(payload.account, "account");
    assert_eq!(payload.folder, "folder");
    assert_eq!(payload.destination, identity);
}

#[test]
fn consent_binds_every_payload_field_and_burns_mismatched_approvals() {
    let identity = DestinationIdentity {
        device: 7,
        inode: 11,
        modified: None,
    };
    let entry = cloud::ImportEntry {
        id: "source".into(),
        name: "document".into(),
        kind: cloud::ImportKind::File,
        revision: Some("revision".into()),
        size: Some(17),
        content_writable: false,
    };
    let original = vec![entry];
    for field in 0..14 {
        let mut issuer = ImportAuthority::default();
        issuer
            .approve("job", "account", &original, "folder", &identity, "device")
            .unwrap();
        let mut job = "job";
        let mut account = "account";
        let mut entries = original.clone();
        let mut folder = "folder";
        let mut destination = identity.clone();
        let mut device = "device";
        match field {
            0 => job = "other-job",
            1 => account = "other-account",
            2 => entries[0].id = "other-source".into(),
            3 => entries[0].name = "other-name".into(),
            4 => entries[0].kind = cloud::ImportKind::Folder,
            5 => entries[0].revision = None,
            6 => entries[0].size = Some(18),
            7 => entries[0].content_writable = true,
            8 => entries.clear(),
            9 => folder = "other-folder",
            10 => destination.inode += 1,
            11 => destination.device += 1,
            12 => destination.modified = Some((1, 2, 3)),
            _ => device = "other-device",
        }
        assert!(
            issuer
                .consume(job, account, &entries, folder, &destination, device)
                .is_err(),
            "field {field}"
        );
        if field != 0 {
            assert!(
                issuer
                    .consume("job", "account", &original, "folder", &identity, "device")
                    .is_err(),
                "mismatch must burn approval for field {field}"
            );
        }
    }
}

#[test]
fn destination_receipts_keep_the_legacy_default_and_encoding() {
    let identity: DestinationIdentity = serde_json::from_str(r#"{"device":7,"inode":11}"#).unwrap();
    assert_eq!(
        serde_json::to_value(identity).unwrap(),
        serde_json::json!({
            "device": 7, "inode": 11, "modified": null,
        })
    );
}
