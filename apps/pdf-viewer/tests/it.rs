#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
#[path = "capture.rs"]
pub mod capture;
#[path = "headless_launch.rs"]
pub mod headless_launch;
#[path = "window_documents.rs"]
pub mod window_documents;
