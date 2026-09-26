//! Markdown body IO: round-trip a page body through disk, then
//! delete it and assert the file is gone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::storage::{
    delete_page_file, ensure_layout, page_file_path, read_page_body, write_page_body,
};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn page_files_write_read_and_delete_including_missing_ones() {
    eprintln!("phase write_then_read_preserves_body");
    {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        let body = "# Hello\n\nMarkdown body with **bold** content.\n";
        write_page_body(tmp.path(), id, body).unwrap();
        let back = read_page_body(tmp.path(), id).unwrap();
        assert_eq!(back, body);
    }

    eprintln!("phase read_missing_page_returns_empty_string");
    {
        let tmp = TempDir::new().unwrap();
        ensure_layout(tmp.path()).unwrap();
        let id = Uuid::new_v4();
        let body = read_page_body(tmp.path(), id).unwrap();
        assert!(body.is_empty(), "missing page should read as empty body");
    }

    eprintln!("phase delete_removes_the_file");
    {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        write_page_body(tmp.path(), id, "body").unwrap();
        assert!(page_file_path(tmp.path(), id).exists());
        delete_page_file(tmp.path(), id).unwrap();
        assert!(!page_file_path(tmp.path(), id).exists());
    }

    eprintln!("phase delete_missing_file_is_ok");
    {
        let tmp = TempDir::new().unwrap();
        let id = Uuid::new_v4();
        delete_page_file(tmp.path(), id).expect("deleting a non-existent file is success");
    }
}
