//! Offscreen renders of the shared global-search surface — a real
//! [`SearchField`] input over a real [`SearchResultsView`] overlay, fed by a
//! real [`SearchRegistry`] with a few demo providers. Proves the whole seam
//! renders end-to-end (declare providers → query → merge/rank → grouped,
//! selectable overlay) without opening a window.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- search_showcase`
//! Out:  `$SEARCH_SHOWCASE_OUT` (default `/tmp/search_showcase`) —
//!       search-{dark,light}-populated.png, search-dark-selected.png,
//!       search-dark-empty.png
//!
//! Each shot drives the real five-pass path for the field and the real
//! self-layout of the overlay, so the pixels match the live widgets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::search::{FieldWeight, ListIndex, SearchRegistry, SearchResult};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{SearchField, SearchResultsView, Widget};

const WIDTH: u32 = 480;
const PAD: f64 = 16.0;
const FIELD_H: f64 = 38.0;
const GAP: f64 = 10.0;

/// Narrows a screen-space `f64` (rect extent or coordinate) to the `f32`
/// taffy layout math expects.
#[allow(
    clippy::cast_possible_truncation,
    reason = "on-screen layout coordinates never approach f32::MAX; this narrowing is exact for every practical UI extent"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Rounds a canvas dimension up to the nearest whole pixel. Every caller
/// passes a positive, finite layout extent, so `v` is always in
/// `[0.0, u32::MAX]` — the `.clamp` here makes the truncating/sign-losing
/// cast provably safe regardless.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "v is clamped into [0.0, u32::MAX] on the line above, so the cast can never truncate or flip sign"
)]
fn px_dim(v: f64) -> u32 {
    v.ceil().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// One demo record: (title, subtitle, action-id).
type Rec = (&'static str, Option<&'static str>, &'static str);

fn provider(reg: &mut SearchRegistry, kind: &'static str, icon: Icon, recs: &'static [Rec]) {
    let index = ListIndex::default();
    reg.register_fn(kind, move |q: &str| {
        let ranks = index.ranks(
            q,
            recs.iter().map(|(title, sub, _)| {
                [
                    (*title, FieldWeight::TITLE),
                    (sub.unwrap_or_default(), FieldWeight::SUBTITLE),
                ]
            }),
        );
        recs.iter()
            .zip(ranks)
            .filter_map(|(rec, rank)| rank.map(|_| rec))
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
    let layout_result = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(to_f32(width)),
            height: AvailableSpace::Definite(to_f32(FIELD_H)),
        },
        |known, available, id, _, _| {
            node_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, &mut tr)
            })
        },
    );
    if let Err(e) = layout_result {
        eprintln!("search_showcase: layout failed: {e}");
        return;
    }
    field.apply_layout(&taffy, node, origin);
}

fn shoot(theme: &Theme, out: &Path, query: &str, cursor: usize) {
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let field_w = f64::from(WIDTH) - PAD * 2.0;

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
        f64::from(WIDTH) - PAD,
        overlay_top + view.desired_height(),
    ));

    let height = px_dim(overlay_top + view.desired_height() + PAD);

    let mut scene = Scene::new();
    field.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    view.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);

    let mut r = OffscreenRenderer::new(WIDTH, height);
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {} ({}x{})", out.display(), WIDTH, height);
}

pub fn run(_args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir =
        std::env::var("SEARCH_SHOWCASE_OUT").unwrap_or_else(|_| "/tmp/search_showcase".into());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("failed to create output dir {dir}: {e}");
        return;
    }
    let dir = PathBuf::from(dir);

    let dark = Theme::dark();
    let light = Theme::light();

    // Populated: a mixed "al" query hitting all four kinds, first row selected.
    shoot(&dark, &dir.join("search-dark-populated.png"), "al", 0);
    shoot(&light, &dir.join("search-light-populated.png"), "al", 0);
    // Selection mid-list: the highlight sits on a lower row (not the top hit),
    // to show the selected-row form is distinct from idle rows and headers.
    shoot(&dark, &dir.join("search-dark-selected.png"), "al", 3);
    // Empty state.
    shoot(&dark, &dir.join("search-dark-empty.png"), "zzqq", 0);
}
