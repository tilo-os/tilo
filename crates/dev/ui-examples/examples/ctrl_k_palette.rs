//! Offscreen renders of the unified Ctrl+K palette (`tilo_ui::ActionPalette`).
//!
//! One surface, driven by the resource/action model: the focused app's exported
//! `ActionSpec`s form the **Actions** group, and a folded-in `SearchProvider`
//! (here, contacts) forms a **Contacts** results group. Rendered by the shared
//! `SearchField` + `SearchResultsView` — the same widgets the "find things"
//! surface uses — so the pixels match the live Ctrl+K.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- ctrl_k_palette`
//! Out:  `$CTRLK_PALETTE_OUT` (default `/tmp/ctrl_k_palette`) —
//!       `ctrlk-{dark,light}-browse.png`, `ctrlk-dark-query.png`

use std::path::{Path, PathBuf};

use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::ActionPalette;
use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::search::{FieldWeight, ListIndex, SearchProvider, SearchResult};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tilo_actions::{ActionSpec, EncodedAction, ExecutionLocus, Resource};

const WIDTH: u32 = 560;
const MARGIN: f64 = 20.0;

/// A demo immediate action, built by hand (no dispatch happens in a shot).
fn action(id: &str, label: &str, desc: &str) -> ActionSpec {
    ActionSpec::immediate_encoded(
        id,
        label,
        ExecutionLocus::ClientLocal,
        EncodedAction {
            type_id: format!("demo.{id}"),
            payload: b"{}".to_vec(),
        },
    )
    .with_description(desc)
}

/// The focused app's open resource + its exported actions (the Actions group).
fn open_resource() -> Resource {
    Resource::new("doc:design-notes", "doc", "Design Notes")
        .with_action(action("rename", "Rename", "Give the document a new name"))
        .with_action(action(
            "share",
            "Share with a peer",
            "Invite someone to this workspace",
        ))
        .with_action(action(
            "export",
            "Export as PDF",
            "Write a PDF alongside the source",
        ))
}

/// A folded-in searchable corpus — stands in for the contacts provider.
#[derive(Default)]
struct Contacts(ListIndex);
impl SearchProvider for Contacts {
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "SearchProvider::name is `&str`, not `&'static str`; the impl can't widen it"
    )]
    fn name(&self) -> &str {
        "Contacts"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        const PEOPLE: &[(&str, &str)] = &[
            ("Ada Lovelace", "ada@analytical.engine"),
            ("Grace Hopper", "grace.hopper@navy.mil"),
            ("Alan Turing", "alan@bletchley.uk"),
        ];
        let ranks = self.0.ranks(
            query,
            PEOPLE.iter().map(|(name, email)| {
                [(*name, FieldWeight::TITLE), (*email, FieldWeight::SUBTITLE)]
            }),
        );
        PEOPLE
            .iter()
            .zip(ranks)
            .filter_map(|(person, rank)| rank.map(|_| person))
            .map(|(name, email)| {
                SearchResult::new("Contacts", *name, format!("contact:{name}"))
                    .with_subtitle(*email)
                    .with_icon(Icon::Mail)
            })
            .collect()
    }
}

fn palette(query: &str, cursor: usize) -> ActionPalette {
    let mut p = ActionPalette::new();
    p.load_open_resources(&[open_resource()]);
    p.set_providers(vec![Box::new(Contacts::default())]);
    p.open();
    if !query.is_empty() {
        p.set_query(query);
    }
    p.set_cursor(cursor);
    p
}

/// Rounds a rendered pixel measurement (always non-negative, far under
/// `u32::MAX`) to a `u32` for `OffscreenRenderer::new`'s `height` parameter.
fn px_u32(v: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "v is a rendered pixel measurement, always non-negative and far below u32::MAX"
    )]
    let out = v.max(0.0).round() as u32;
    out
}

fn shoot(theme: &Theme, out: &Path, query: &str, cursor: usize) {
    let mut p = palette(query, cursor);
    let width = f64::from(WIDTH) - MARGIN * 2.0;
    p.layout(theme, Point::new(MARGIN, MARGIN), width);
    let height = px_u32((p.rect().y1 + MARGIN).ceil());

    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    p.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, height);
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {} ({}x{})", out.display(), WIDTH, height);
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("CTRLK_PALETTE_OUT").unwrap_or_else(|_| "/tmp/ctrl_k_palette".into());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let dark = Theme::dark();
    let light = Theme::light();

    // Browse (empty query): the full Actions group + the full Contacts group,
    // highlight on the first action.
    shoot(&dark, &dir.join("ctrlk-dark-browse.png"), "", 0);
    shoot(&light, &dir.join("ctrlk-light-browse.png"), "", 0);
    // A live query hitting both groups, highlight on a contact row.
    shoot(&dark, &dir.join("ctrlk-dark-query.png"), "a", 3);
}
