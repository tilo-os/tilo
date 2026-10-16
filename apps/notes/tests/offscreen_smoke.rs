//! Headless render smoke test: the notebook surface composes through the
//! same `sync → build_layout → apply_layout → draw` cycle the live window
//! runs, with every block of a mixed note laid out.

use crate::framework_support::{surface, with_note};
use ui::widget::Widget;

const DEMO: &str = "\
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

#[test]
fn render_one_frame_of_a_mixed_note() {
    let (mut app, _) = with_note("Welcome", DEMO);
    // A flex chain that drops `flex_direction: Column` collapses every block
    // to width 0; it has regressed before.
    let rects = surface(&mut app, |notes| {
        notes
            .main_panel_mut()
            .editor()
            .blocks()
            .iter()
            .map(|block| block.line.get_rect())
            .collect::<Vec<_>>()
    });
    assert!(
        rects.len() >= 6,
        "the note parses into its blocks: {rects:?}"
    );
    for rect in &rects {
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "block rect must be non-degenerate (got {rect:?})"
        );
    }
    if let Ok(path) = std::env::var("NOTES_SNAPSHOT") {
        app.harness().render_png(std::path::Path::new(&path));
        eprintln!("notes: snapshot written to {path}");
    }
}
