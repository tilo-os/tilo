//! Offscreen renders of the shared global-search surface — a real
//! [`SearchField`] input over a real [`SearchResultsView`] overlay, fed by a
//! real [`SearchRegistry`] with a few demo providers. Proves the whole seam
//! renders end-to-end (declare providers → query → merge/rank → grouped,
//! selectable overlay) without opening a window.
//!
//! Run:  cargo run -p ui --example ui-shots -- search_showcase
//! Out:  $SEARCH_SHOWCASE_OUT (default /tmp/search_showcase) —
//!       search-{dark,light}-populated.png, search-dark-selected.png,
//!       search-dark-empty.png
//!
//! Each shot drives the real five-pass path for the field and the real
//! self-layout of the overlay, so the pixels match the live widgets.

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::search::{SearchRegistry, SearchResult, fuzzy_score};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{SearchField, SearchResultsView, Widget};

const WIDTH: u32 = 480;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;
const GAP: f64 = 10.0;

/// One demo record: (title, subtitle, action-id).
type Rec = (&'static str, Option<&'static str>, &'static str);

fn provider(reg: &mut SearchRegistry, kind: &'static str, icon: Icon, recs: &'static [Rec]) {
    reg.register_fn(kind, move |q: &str| {
        recs.iter()
            .filter(|(title, sub, _)| {
                fuzzy_score(q, title).is_some()
                    || sub.map(|s| fuzzy_score(q, s).is_some()).unwrap_or(false)
            })
            .map(|(title, sub, id)| {
                let mut r = SearchResult::new(kind, *title, *id).with_icon(icon);
                if let Some(s) = sub {
                    r = r.with_subtitle(*s);
                }
                r
            })
            .collect()
    });
}

fn registry() -> SearchRegistry {
    let mut reg = SearchRegistry::new();
    provider(
        &mut reg,
        "Contacts",
        Icon::Mail,
        &[
            ("Alice Ng", Some("alice@example.com"), "contact:alice"),
            ("Alan Turing", Some("Computing"), "contact:alan"),
            (
                "Priya Natarajan",
                Some("priya@example.com"),
                "contact:priya",
            ),
        ],
    );
    provider(
        &mut reg,
        "Files",
        Icon::FileText,
        &[
            ("alpha-notes.md", Some("~/documents"), "file:alpha"),
            ("release-plan.md", Some("~/work"), "file:release"),
        ],
    );
    provider(
        &mut reg,
        "Apps",
        Icon::LayoutGrid,
        &[
            ("Calculator", None, "app:calculator"),
            ("Calendar", None, "app:calendar"),
        ],
    );
    provider(
        &mut reg,
        "Commands",
        Icon::Settings2,
        &[
            ("Align left", Some("Format the selection"), "cmd:align-left"),
            ("Toggle sidebar", Some("View"), "cmd:toggle-sidebar"),
        ],
    );
    reg
}

fn lay_out_field(field: &mut SearchField, theme: &Theme, width: f64, origin: Point) {
    let mut tr = TextRenderer::new();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = field.build_layout(&mut taffy, theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width as f32),
                height: AvailableSpace::Definite(FIELD_H as f32),
            },
            |known, available, id, _, _| {
                node_map
                    .get(&id)
                    .map(|&p| unsafe { (*p).measure(known, available, &mut tr) })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
    field.apply_layout(&taffy, node, origin);
}

fn shoot(theme: &Theme, out: PathBuf, query: &str, cursor: usize) {
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let field_w = WIDTH as f64 - PAD * 2.0;

    // The shared input, seeded with the query and focused (so the focus ring
    // shows). Real widget, real shared text core.
    let mut field = SearchField::new().with_placeholder("Search");
    field.set_query(query);
    field.set_focused(true);
    lay_out_field(&mut field, theme, field_w, Point::new(PAD, PAD));

    // The results overlay, fed by the registry's ranked, grouped query.
    let mut view = SearchResultsView::new().with_empty_text("No results for your search");
    view.set_groups(registry().query_grouped_capped(query, 4));
    if !view.is_empty() {
        view.set_cursor(cursor);
    }
    let overlay_top = PAD + FIELD_H + GAP;
    view.set_rect(Rect::new(
        PAD,
        overlay_top,
        WIDTH as f64 - PAD,
        overlay_top + view.desired_height(),
    ));

    let height = (overlay_top + view.desired_height() + PAD).ceil() as u32;

    let mut scene = Scene::new();
    field.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, height);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {} ({}x{})", out.display(), WIDTH, height);
}

pub fn run(_args: &[String]) {
    ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("SEARCH_SHOWCASE_OUT").unwrap_or_else(|_| "/tmp/search_showcase".into());
    std::fs::create_dir_all(&dir).unwrap();
    let dir = PathBuf::from(dir);

    let dark = Theme::dark();
    let light = Theme::light();

    // Populated: a mixed "al" query hitting all four kinds, first row selected.
    shoot(&dark, dir.join("search-dark-populated.png"), "al", 0);
    shoot(&light, dir.join("search-light-populated.png"), "al", 0);
    // Selection mid-list: the highlight sits on a lower row (not the top hit),
    // to show the selected-row form is distinct from idle rows and headers.
    shoot(&dark, dir.join("search-dark-selected.png"), "al", 3);
    // Empty state.
    shoot(&dark, dir.join("search-dark-empty.png"), "zzqq", 0);
}
