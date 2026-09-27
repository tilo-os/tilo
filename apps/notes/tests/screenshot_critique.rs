//! Generate genuine desktop and narrow interaction states through the same
//! scenario driver used by the executable's --shots path. Always render:
//! absence of an output override must never make this visual check pass empty.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

#[test]
fn critique_state_dump() {
    let directory = std::env::var_os("NOTES_CRITIQUE_DIR").map_or_else(
        || std::path::PathBuf::from("target/notes-critique"),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&directory).expect("create screenshot evidence directory");
    let only = std::env::var("NOTES_CRITIQUE_SCENARIO").ok();
    let captures = [
        ("01-overview", None, (1280, 800)),
        ("25-typed-lists", Some("typed_lists"), (1280, 800)),
        ("26-code-keyboard", Some("code_keyboard"), (1280, 800)),
        ("27-code-active", Some("code_keyboard_active"), (390, 844)),
        ("28-rich-outline", Some("rich_outline"), (1280, 800)),
        ("29-narrow-rich-outline", Some("rich_outline"), (390, 844)),
        ("30-multiline-typing", Some("multiline_typing"), (1280, 800)),
        ("31-clicked-caret", Some("paragraph_click"), (1280, 800)),
        ("32-find-in-note", Some("find"), (1280, 800)),
        ("33-block-type-menu", Some("block_type_menu"), (1280, 800)),
        ("02-empty-workspace", Some("first_note"), (1280, 800)),
        ("03-title-edit", Some("title_edit"), (1280, 800)),
        ("04-block-drag", Some("block_drag"), (1280, 800)),
        ("05-menu", Some("menu"), (1280, 800)),
        ("06-narrow-menu", Some("menu"), (390, 844)),
        ("07-narrow-heading", Some("heading"), (390, 844)),
        ("08-narrow-slash-applied", Some("slash_applied"), (390, 844)),
        ("09-narrow-shortcuts", Some("shortcuts"), (390, 844)),
        ("10-table", Some("table"), (1280, 800)),
        ("11-image", Some("image"), (1280, 800)),
        ("12-note-menu", Some("note_menu"), (1280, 800)),
        ("13-narrow-note-menu", Some("note_menu"), (390, 844)),
        ("14-large-table", Some("table_large"), (1280, 800)),
        ("15-narrow-large-table", Some("table_large"), (390, 844)),
        (
            "16-large-table-bottom",
            Some("table_large_scrolled"),
            (1280, 800),
        ),
        (
            "17-narrow-large-table-bottom",
            Some("table_large_scrolled"),
            (390, 844),
        ),
        ("18-table-cell-edit", Some("table_cell_edit"), (1280, 800)),
        ("19-image-edit", Some("image_edit"), (1280, 800)),
        ("22-fence-opening", Some("fenced_code_opening"), (1280, 800)),
        ("23-typed-python", Some("fenced_code"), (1280, 800)),
        ("24-narrow-typed-python", Some("fenced_code"), (390, 844)),
    ]
    .into_iter()
    .filter(|(_, scenario, _)| only.is_none() || *scenario == only.as_deref())
    .map(|(name, scenario, viewport)| crate::captures::Capture {
        label: name.to_owned(),
        scenario,
        viewport,
        path: directory.join(format!("{name}.png")),
    })
    .collect::<Vec<_>>();
    crate::captures::render_all(&captures);
}
