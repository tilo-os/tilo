use super::*;
use std::sync::mpsc::channel;

/// Poll until the listing read on its own thread arrives.
fn await_listing(widget: &mut FileExplorerWidget) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !widget.poll_listing() {
        assert!(
            std::time::Instant::now() < deadline,
            "the listing never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn names(widget: &FileExplorerWidget) -> Vec<String> {
    let mut names: Vec<String> = widget.entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names
}

// USER REQUIREMENT (2026-10-21): "Native tilo-app apps: a toolkit API for
// "busy" and "progress" (one call, e.g. from a long load). Use it in at least
// Files (opening a large folder)."
#[test]
fn a_folder_that_takes_long_to_read_opens_loading_and_says_so_in_the_window() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    let mut widget = FileExplorerWidget::new();
    let (release, hold) = channel();
    widget.listing_budget = Some(Duration::ZERO);
    widget.listing_hold = Some(hold);

    widget.load(dir.path().to_path_buf());
    assert!(
        widget.is_loading(),
        "the folder shows loading while it is read"
    );
    assert!(widget.entries.is_empty(), "nothing of another folder shows");
    assert_eq!(widget.current_path, dir.path());
    assert_eq!(
        ui::window_loading::current(),
        ui::window_loading::WindowLoading::Busy,
        "the window's loading line says it is busy"
    );
    assert!(!widget.poll_listing(), "nothing arrived yet");

    release.send(()).unwrap();
    await_listing(&mut widget);
    assert!(!widget.is_loading());
    assert_eq!(names(&widget), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(
        ui::window_loading::current(),
        ui::window_loading::WindowLoading::Idle,
        "and done once the listing shows"
    );
}

#[test]
fn a_quick_folder_opens_in_place_without_a_loading_state() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("only.txt"), b"ok").unwrap();
    let mut widget = FileExplorerWidget::new();
    // A budget no read here outlasts: the folder is read in place.
    widget.listing_budget = Some(Duration::from_secs(30));
    widget.load(dir.path().to_path_buf());
    assert!(!widget.is_loading());
    assert!(widget.pending_listing.is_none());
    assert_eq!(names(&widget), ["only.txt"]);
}

#[test]
fn a_folder_left_while_it_is_read_never_replaces_the_next_one() {
    let slow = tempfile::tempdir().unwrap();
    std::fs::write(slow.path().join("slow.txt"), b"slow").unwrap();
    let next = tempfile::tempdir().unwrap();
    std::fs::write(next.path().join("next.txt"), b"next").unwrap();
    let mut widget = FileExplorerWidget::new();
    let (release, hold) = channel();
    widget.listing_budget = Some(Duration::ZERO);
    widget.listing_hold = Some(hold);
    widget.load(slow.path().to_path_buf());
    assert!(widget.is_loading());

    // The person moves on before it arrives; the next folder reads in place.
    widget.listing_budget = None;
    widget.load(next.path().to_path_buf());
    let _ = release.send(());
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(
        !widget.poll_listing(),
        "the left folder's listing is dropped"
    );
    assert_eq!(widget.current_path, next.path());
    assert_eq!(names(&widget), ["next.txt"]);
}
