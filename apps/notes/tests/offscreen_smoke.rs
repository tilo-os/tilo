//! Headless render smoke test: spawn a `NotesApp`, build its layout,
//! and draw one frame into an offscreen scene. Confirms the whole
//! widget tree composes through the same `sync → build_layout →
//! apply_layout → draw` cycle the live `ui::App` uses, with no panic
//! / no missing layout slot.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::{NotesApp, WELCOME_BODY};
use tempfile::TempDir;
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Widget;

/// Sanity-assert the editor laid out multiple non-degenerate blocks for
/// the kitchen-sink Welcome body. Pinned so a future regression in
/// `MainPanel`'s flex chain (where an outside `set_style` accidentally
/// drops `flex_direction: Column`, collapsing every block to width 0 —
/// this has regressed before) gets caught here instead of in a quiet
/// snapshot.
fn assert_blocks_non_degenerate(app: &mut NotesApp) {
    let panel = app.main_panel_mut();
    let n = panel.editor().blocks().len();
    assert!(
        n >= 6,
        "Welcome seed should parse into at least 6 blocks; got {n}",
    );
    for b in panel.editor().blocks() {
        let r = b.line.get_rect();
        // Dividers have zero text but still get a non-degenerate
        // row (the horizontal rule paints across the whole row).
        assert!(
            r.width() > 0.0 && r.height() > 0.0,
            "block rect must be non-degenerate (got {r:?})"
        );
    }
}

#[test]
fn render_one_frame_against_seeded_workspace() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn ok");

    // v0.1: reload a short demo so the snapshot stays small enough
    // for the status bar to be visible at the bottom. The full
    // WELCOME_BODY is intentionally long — pin its round-trip in
    // welcome_page_renders_all_block_kinds, not here.
    {
        let panel = app.main_panel_mut();
        if let Some(id) = panel.page_id() {
            let demo = "\
# Welcome to notes

A collaborative notes app for Tilo.

## Block types

Bullet lists look like this:

- Coffee

- Tea

Checklists track open work:

- [ ] Open todo

- [x] Completed item

Code blocks:

```rust
fn hello() {}
```

---

## Keyboard

- `Cmd-K` jumps to any page

- `?` opens the shortcuts overlay
";
            panel.load_page(id, "Welcome", demo);
        }
    }
    // The full welcome body still has to round-trip for the
    // dedicated welcome test — pin its presence here just so the
    // `WELCOME_BODY` constant isn't dropped as unused.
    let _ = WELCOME_BODY;

    // Refresh the status bar so it picks up the new (smaller)
    // demo's word count. In the real app every winit event refreshes
    // the bar; in tests we drive it explicitly.
    app.tick_for_test();

    let (w, h) = (1024u16, 768u16);
    let mut app = Framed::new(
        ui::State::new(),
        app,
        (u32::from(w), u32::from(h)),
        Theme::dark(),
    );

    assert_blocks_non_degenerate(&mut app);

    // We don't compare pixels here — just confirm a non-degenerate
    // rect and that the frame, draw included, returned. If anything
    // panics upstream we'd never reach this line.
    assert!(app.get_rect().width() > 0.0);
    assert!(app.get_rect().height() > 0.0);

    // Setting NOTES_SNAPSHOT writes a PNG so a human can eyeball the
    // result. Skipped in CI to avoid the wgpu device init cost on
    // every run.
    if let Ok(path) = std::env::var("NOTES_SNAPSHOT") {
        Framed::harness(&app).render_png(std::path::Path::new(&path));
        eprintln!("notes: snapshot written to {path}");
    }
}
