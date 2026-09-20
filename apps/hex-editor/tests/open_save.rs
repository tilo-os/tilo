//! End-to-end proof of the real file lifecycle: open a file, edit a byte,
//! and save it back to disk — all through the production widget code paths
//! (`view_for_path`, the portal drain, and `handle_event`), with real disk
//! I/O and no mocked buffer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::{AutoCommit, ReadDoc};
use hex_editor::HexView;
use taffy::{NodeId, Style, TaffyTree};
use ui::Widget;
use ui::portal::PortalOutcome;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

/// Build a synthetic key-press. `handle_event` only inspects `logical_key` +
/// `state`; the rest tolerate the zero pattern (mirrors the toolkit's own
/// widget tests).
fn key_press(logical: Key) -> WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical;
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
    }
}

fn dummy_layout() -> (TaffyTree<()>, NodeId) {
    let mut t = TaffyTree::<()>::new();
    let n = t.new_leaf(Style::default()).unwrap();
    (t, n)
}

fn tmp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("hex_{name}_{}.bin", std::process::id()))
}

#[test]
fn open_via_portal_loads_real_bytes_through_the_drain_path() {
    let path = tmp("portal_open");
    std::fs::write(&path, b"HELLO-HEX").unwrap();

    let mut view = HexView::empty();
    assert!(!view.has_file(), "starts in the honest empty state");
    assert!(view.model().is_empty(), "no fabricated buffer");

    // Feed the chosen path through the exact drain path the portal worker feeds.
    view.inject_open_outcome_for_test(PortalOutcome::Opened(path.clone()));

    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    view.sync(&mut doc, &root); // drains the pick -> open_path()

    assert!(view.has_file(), "opening a file leaves the empty state");
    assert_eq!(
        view.model().bytes(),
        b"HELLO-HEX",
        "the real file bytes are loaded"
    );
    assert_eq!(view.path(), Some(path.as_path()));
    assert!(
        doc.get(&automerge::ROOT, "bytes").unwrap().is_none(),
        "raw local bytes are never persisted into Automerge"
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn edit_then_save_writes_back_to_disk() {
    let path = tmp("edit_save");
    std::fs::write(&path, [0x00u8, 0x11, 0x22, 0x33]).unwrap();

    // Real open via the CLI/path production path.
    let mut view = hex_editor::app::view_for_path(&path);
    assert!(view.has_file());
    assert_eq!(view.model().bytes(), &[0x00, 0x11, 0x22, 0x33]);
    assert_eq!(view.path(), Some(path.as_path()));

    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let (taffy, node) = dummy_layout();
    let no_mods = Modifiers::default();
    let ctrl = Modifiers::from(ModifiersState::CONTROL);

    view.sync(&mut doc, &root); // synchronize local portal/view state

    // Type "a" then "b" over byte 0 (hex column, overwrite) -> 0xAB.
    for ch in ["a", "b"] {
        let mut handled = false;
        let mut focus = None;
        view.handle_event(
            &key_press(Key::Character(ch.into())),
            1.0,
            &no_mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }
    assert_eq!(
        view.model().bytes()[0],
        0xAB,
        "hex input edited the first byte"
    );
    assert!(view.model().dirty_count() > 0, "the edit is marked dirty");

    // Ctrl+S saves straight back to the known path.
    let mut handled = false;
    let mut focus = None;
    view.handle_event(
        &key_press(Key::Character("s".into())),
        1.0,
        &ctrl,
        &mut doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );

    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(
        on_disk,
        vec![0xAB, 0x11, 0x22, 0x33],
        "Ctrl+S wrote the edit to disk"
    );
    assert_eq!(
        view.model().dirty_count(),
        0,
        "a successful save clears the dirty markers"
    );

    std::fs::remove_file(&path).ok();
}

/// Raw typing remains in the local model: it must neither seed nor mutate the
/// collaboration document, and it creates a device-local undo boundary.
#[test]
fn keystroke_keeps_raw_bytes_local() {
    let mut view = HexView::new(vec![0x00, 0x11, 0x22, 0x33], "buf.bin");
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let (taffy, node) = dummy_layout();
    let no_mods = Modifiers::default();

    view.sync(&mut doc, &root); // local model only

    // Type "a" (hex 0xA) over byte 0 (0x00) — the high nibble → 0xA0.
    let mut handled = false;
    let mut focus = None;
    view.handle_event(
        &key_press(Key::Character("a".into())),
        1.0,
        &no_mods,
        &mut doc,
        &root,
        &mut handled,
        &mut focus,
        node,
        &taffy,
    );
    assert!(handled, "the hex-digit keystroke was consumed");

    assert!(
        doc.get(&automerge::ROOT, "bytes").unwrap().is_none(),
        "the byte stays local"
    );
}
