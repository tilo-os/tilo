//! App compatibility surface for native original-caller facts.
pub use crate::capture::original_caller::OriginalCallerFacts;

#[cfg(any(test, feature = "test-support"))]
pub fn fixture_source_owner(home: &std::path::Path) -> OriginalCallerFacts {
    use std::os::unix::ffi::OsStrExt;
    // Controlled fixture only: make the Home's actual owner nonroot on root
    // test runners. These facts do not claim authenticated broker provenance.
    // SAFETY: these calls only read the test process's current credentials.
    let actual_uid = rustix::process::geteuid().as_raw();
    let uid = actual_uid.max(1);
    // SAFETY: reads the test process's current effective GID.
    let gid = rustix::process::getegid().as_raw();
    if actual_uid == 0 {
        let path = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-terminated fixture-owned path; no user source is touched.
        assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
    }
    serde_json::from_value(serde_json::json!({
        "schema": 1,
        "uid": uid,
        "primary_gid": gid,
        "supplementary_gids": [],
    }))
    .unwrap()
}
