//! Regression: a remote peer adding a page grows the sidebar's row list
//! via `sync`, and a draw can land before the next relayout. The sidebar
//! must not index `row_rects` out of bounds (panicked at sidebar.rs:310
//! in the live two-instance demo). Drives the real
//! `sync → build_layout → compute → apply_layout → draw` cycle headless
//! (no GPU — `draw` just fills a vello Scene).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::NotesApp;
use notes::model::PageMeta;
use ui::headless::Framed;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::Affine;

/// Run a full frame through the headless host (update, sync, build,
/// compute, apply, draw) — the part of the frame that sizes every widget's
/// rects, including the sidebar's per-row `row_rects`.
fn relayout(app: &mut Framed<NotesApp>) -> Result<(), String> {
    Framed::harness_mut(app)
        .relayout_at((1024, 768))
        .map_err(|e| e.to_string())
}

fn draw(app: &mut NotesApp, theme: &Theme, tr: &mut TextRenderer) {
    let presence = PresenceCtx::empty();
    let mut scene = Scene::new();
    app.draw(&mut scene, Affine::IDENTITY, tr, theme, &presence);
}

#[test]
fn sidebar_survives_remote_page_added_between_layout_and_draw() {
    let mut state = ui::State::new();
    notes::read::initialize(&mut state.doc);
    crate::component_support::upsert_page(
        &mut state.doc,
        &PageMeta {
            id: uuid::Uuid::new_v4(),
            title: "Local Page".into(),
            parent: None,
        },
    );
    let app = NotesApp::from_document(&state.doc, &ui::permissions::UserId("me".into())).unwrap();
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();

    // Frame 1: full cycle so the sidebar lays out its welcome row(s).
    let mut app = Framed::new(state, app, (1024, 768), theme.clone());
    draw(&mut app, &theme, &mut tr);

    // A remote peer adds a second page to the SHARED doc.
    crate::component_support::upsert_page(
        &mut Framed::split(&mut app).1.doc,
        &PageMeta {
            id: uuid::Uuid::new_v4(),
            title: "Remote Page".into(),
            parent: None,
        },
    );

    // `sync` pulls the new page in (sidebar flat_rows grows) but we DRAW
    // before the next relayout — the exact ordering that panicked in the
    // live demo. Must not panic.
    let (notes, state) = Framed::split(&mut app);
    notes.sync(&state.doc, &state.root);
    draw(&mut app, &theme, &mut tr);

    // And after a proper relayout it renders both rows without panic.
    relayout(&mut app).unwrap();
    draw(&mut app, &theme, &mut tr);
}
