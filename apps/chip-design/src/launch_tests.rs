//! Actual host-file launch reuses public import, inspection and history.
use crate::tests::Parsed as _;
use crate::{ChipDesignShell, VerificationRecord};
use ui::application::OpenFile;
use ui::framework::testing::TestApp;

#[test]
fn handed_json_and_collected_binary_open_the_exact_undoable_report() {
    let raw = include_str!("../fixtures/evidence-flow.json");
    let identity = VerificationRecord::import(raw).unwrap().0;
    for (name, mime) in [
        ("report.json", "application/json"),
        ("content-hash.bin", "application/octet-stream"),
    ] {
        let (file, _) = OpenFile::new(name, mime, raw.as_bytes().to_vec()).unwrap();
        let mut app = TestApp::<ChipDesignShell>::handed(file).unwrap();
        assert_eq!(app.parsed().verifications.len(), 1);
        assert!(app.parsed().verifications.contains_key(&identity));
        let read = app
            .call(
                "read",
                serde_json::json!({"item":crate::resource::verification_resource_id(&identity)}),
            )
            .unwrap()
            .unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(&VerificationRecord::import(raw).unwrap().1).unwrap();
        assert_eq!(read, expected);
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("Synthetic CPU comparison")
        );
        assert!(
            app.undo(),
            "host-file import must use ordinary attributed history"
        );
        assert!(app.parsed().verifications.is_empty());
    }
}

#[test]
fn invalid_host_files_show_refusal_without_mutating_the_project() {
    for bytes in [
        b"{}".to_vec(),
        b"not JSON".to_vec(),
        vec![255],
        vec![b' '; crate::model::MAX_REPORT_BYTES + 1],
    ] {
        let (file, _) = OpenFile::new("untrusted.bin", "application/octet-stream", bytes).unwrap();
        let mut app = TestApp::<ChipDesignShell>::handed(file).unwrap();
        assert!(app.parsed().verifications.is_empty());
        assert!(app.parsed().reports.is_empty());
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("This file is not a supported Chip Design report")
        );
        assert!(!app.undo(), "refused input must create no report history");
    }
}

#[test]
fn handed_analytical_report_uses_existing_storage_import() {
    let raw = include_bytes!("../fixtures/storage-report.json");
    let (file, _) = OpenFile::new("storage.json", "application/json", raw.to_vec()).unwrap();
    let mut app = TestApp::<ChipDesignShell>::handed(file).unwrap();
    assert_eq!(app.parsed().reports.len(), 1);
    assert!(app.parsed().verifications.is_empty());
    assert!(app.undo());
    assert!(app.parsed().reports.is_empty());
}

#[test]
fn launch_without_a_file_stays_empty_and_unsupported_media_is_refused() {
    let app = TestApp::<ChipDesignShell>::new().unwrap();
    assert!(app.parsed().reports.is_empty());
    assert!(app.parsed().verifications.is_empty());
    let (file, _) = OpenFile::new(
        "photo.png",
        "image/png",
        include_bytes!("../fixtures/storage-report.json").to_vec(),
    )
    .unwrap();
    assert!(TestApp::<ChipDesignShell>::handed(file).is_err());
}
