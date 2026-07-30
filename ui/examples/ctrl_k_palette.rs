//! Offscreen renders of the unified Ctrl+K palette (`ui::ActionPalette`).
//!
//! One surface, driven by the resource/action model: the focused app's exported
//! `ActionSpec`s form the **Actions** group, and a folded-in `SearchProvider`
//! (here, contacts) forms a **Contacts** results group. Rendered by the shared
//! `SearchField` + `SearchResultsView` — the same widgets the "find things"
//! surface uses — so the pixels match the live Ctrl+K.
//!
//! Run:  cargo run -p ui --example ctrl_k_palette
//! Out:  $CTRLK_PALETTE_OUT (default /tmp/ctrl_k_palette) —
//!       ctrlk-{dark,light}-browse.png, ctrlk-dark-query.png

use std::path::PathBuf;

use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::icons::Icon;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::qos_actions::{ActionInput, ActionSpec, EncodedAction, ExecutionLocus, Resource};
use ui::search::{fuzzy_score, SearchProvider, SearchResult};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::ActionPalette;

const WIDTH: u32 = 560;
const MARGIN: f64 = 20.0;

/// A demo immediate action, built by hand (no dispatch happens in a shot).
fn action(id: &str, label: &str, desc: &str) -> ActionSpec {
    ActionSpec {
        id: id.into(),
        label: label.into(),
        description: Some(desc.into()),
        locus: ExecutionLocus::ClientLocal,
        input: ActionInput::Immediate(EncodedAction {
            type_id: format!("demo.{id}"),
            payload: b"{}".to_vec(),
        }),
    }
}

/// The focused app's open resource + its exported actions (the Actions group).
fn open_resource() -> Resource {
    Resource::new("doc:design-notes", "doc", "Design Notes")
        .with_action(action("rename", "Rename", "Give the document a new name"))
        .with_action(action("share", "Share with a peer", "Invite someone to this workspace"))
        .with_action(action("export", "Export as PDF", "Write a PDF alongside the source"))
}

/// A folded-in searchable corpus — stands in for the contacts provider.
struct Contacts;
impl SearchProvider for Contacts {
    fn name(&self) -> &str {
        "Contacts"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        const PEOPLE: &[(&str, &str)] = &[
            ("Ada Lovelace", "ada@analytical.engine"),
            ("Grace Hopper", "grace.hopper@navy.mil"),
            ("Alan Turing", "alan@bletchley.uk"),
        ];
        PEOPLE
            .iter()
            .filter(|(name, email)| {
                fuzzy_score(query, name).is_some() || fuzzy_score(query, email).is_some()
            })
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
    p.set_providers(vec![Box::new(Contacts)]);
    p.open();
    if !query.is_empty() {
        p.set_query(query);
    }
    p.set_cursor(cursor);
    p
}

fn shoot(theme: &Theme, out: PathBuf, query: &str, cursor: usize) {
    let mut p = palette(query, cursor);
    let width = WIDTH as f64 - MARGIN * 2.0;
    p.layout(theme, Point::new(MARGIN, MARGIN), width);
    let height = (p.rect().y1 + MARGIN).ceil() as u32;

    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    p.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, height);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {} ({}x{})", out.display(), WIDTH, height);
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let dir = std::env::var("CTRLK_PALETTE_OUT").unwrap_or_else(|_| "/tmp/ctrl_k_palette".into());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    let dark = Theme::dark();
    let light = Theme::light();

    // Browse (empty query): the full Actions group + the full Contacts group,
    // highlight on the first action.
    shoot(&dark, dir.join("ctrlk-dark-browse.png"), "", 0);
    shoot(&light, dir.join("ctrlk-light-browse.png"), "", 0);
    // A live query hitting both groups, highlight on a contact row.
    shoot(&dark, dir.join("ctrlk-dark-query.png"), "a", 3);
}
