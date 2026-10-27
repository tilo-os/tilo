//! The contract every [`FilePicker`] and [`UriOpener`] keeps, as checks a
//! backend's tests (and a host's own backend's tests) run.
//!
//! Every check sends a request the contract says must fail before any chooser
//! or handler appears, so they run headless, on CI, against the real system
//! backends. They panic with the violated rule.
#![allow(
    clippy::missing_panics_doc,
    reason = "a conformance check fails by panicking with the violated rule"
)]

use std::time::Duration;

use crate::portal::{
    ContentsOutcome, LaunchOutcome, LaunchRequest, OpenFileOutcome, PickRequest, SaveOutcome,
    open_uri_with,
};
use crate::{FilePicker, UriOpener};

/// How long a backend may take to answer a request it must refuse before
/// the check calls it hung.
fn answer() -> Duration {
    tilo_test_env::hang_bound()
}

/// Every request answers exactly once, and requests that cannot succeed fail
/// cleanly instead of opening a chooser or hanging.
pub fn check_picker_contract(picker: &dyn FilePicker) {
    assert!(
        matches!(
            picker
                .open_file(PickRequest::save("Save", "Save", "x.txt"))
                .recv_timeout(answer()),
            Ok(OpenFileOutcome::Failed(_))
        ),
        "an open-descriptor request must be a single-file Open chooser"
    );
    assert!(
        matches!(
            picker
                .open_file(PickRequest::open_multiple("Open", "Open"))
                .recv_timeout(answer()),
            Ok(OpenFileOutcome::Failed(_))
        ),
        "an open-descriptor request must not be multi-select"
    );
    assert!(
        matches!(
            picker
                .launch(LaunchRequest::open("relative/file.txt"))
                .recv_timeout(answer()),
            Ok(LaunchOutcome::Failed(_))
        ),
        "a relative path must never reach the desktop's opener"
    );
    assert!(
        matches!(
            picker
                .save_contents(PickRequest::open("Open", "Open"), b"x".to_vec())
                .recv_timeout(answer()),
            Ok(SaveOutcome::Failed(_))
        ),
        "only a Save chooser saves contents"
    );
    assert!(
        matches!(
            picker
                .pick_contents(PickRequest::directory("Folder", "Choose"))
                .recv_timeout(answer()),
            Ok(ContentsOutcome::Failed(_))
        ),
        "only an Open chooser delivers contents"
    );
}

/// An opener refuses what is not an absolute URI, and a URI carrying NUL
/// never reaches it (native APIs would silently truncate it).
pub fn check_opener_contract(opener: &dyn UriOpener) {
    assert_eq!(
        open_uri_with(opener, "https://example.invalid/\0evil").map_err(|error| error.kind()),
        Err(std::io::ErrorKind::InvalidInput),
        "a NUL byte must be refused before the opener"
    );
    assert!(
        opener.open("not a uri").is_err(),
        "a relative string is not a URI"
    );
}
