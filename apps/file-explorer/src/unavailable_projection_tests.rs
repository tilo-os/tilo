//! A host without the Tilo filesystem projection never acknowledges a
//! control command or reports a projected state.

use host_fs::attributes::{get, remove, set};
use std::io;

#[test]
fn unavailable_projection_never_reports_a_successful_control_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("document.txt");
    std::fs::write(&path, b"local document").unwrap();
    assert_eq!(
        get(&path, crate::model::SYNC_XATTR).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        set(&path, crate::model::AVAILABILITY_CONTROL_XATTR, b"evict")
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        remove(&path, crate::model::PIN_XATTR).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"local document");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    assert_eq!(crate::model::read_sync_state(&path), None);
    let history = crate::framework_history::MountedFileHistory::open(&path);
    assert!(matches!(
        history,
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported
    ));
    let runtime = ui::rt::app();
    assert!(
        runtime
            .block_on(crate::received_shares::admit_link(
                "unavailable-service".into()
            ))
            .unwrap()
            .is_err()
    );
    assert!(
        runtime
            .block_on(crate::sharing::create_one_use_view_link(
                &path,
                fs_protocol::FileRecordId::new()
            ))
            .unwrap()
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"local document");
}
