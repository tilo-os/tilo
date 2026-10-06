//! Offscreen renders of the platform search surfaces, from real widgets:
//! the results overlay fed by a real [`SearchRegistry`]; the palette narrowed
//! to a scope with facet chips, message rows and a slow page still loading;
//! media and file-hit rows; the in-content filter field in its states; and
//! the find bar over a document with its match highlights.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- search_showcase [OUT_DIR]`
//! Out:  `OUT_DIR`, else `$SEARCH_SHOWCASE_OUT`, else `/tmp/search_showcase` —
//!       search-{dark,light}-populated.png, search-dark-selected.png,
//!       search-dark-empty.png, palette-scoped-{dark,light}.png,
//!       palette-rows-{dark,light}.png, filter-{dark,light}.png,
//!       find-bar-{dark,light}.png
//!
//! Each shot drives the real five-pass path for the field and the real
//! self-layout of the overlay, so the pixels match the live widgets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use tilo_ui::ActionPalette;
use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::search::{
    Facet, FieldWeight, ListIndex, PagePoll, PendingToken, ScopeDecl, SearchPage, SearchProvider,
    SearchQuery, SearchRegistry, SearchResult, SearchRow,
};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui::widget::{
    FilterCount, Filterable, FindBar, FindMatch, FindOptions, Findable, SearchField,
    SearchResultsView, Widget, paint_find_match,
};

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

pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let dir = args.first().cloned().unwrap_or_else(|| {
        std::env::var("SEARCH_SHOWCASE_OUT").unwrap_or_else(|_| "/tmp/search_showcase".into())
    });
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

    for (theme, name) in [(&dark, "dark"), (&light, "light")] {
        shoot_palette(theme, &dir.join(format!("palette-scoped-{name}.png")), true);
        shoot_palette(theme, &dir.join(format!("palette-rows-{name}.png")), false);
        shoot_filters(theme, &dir.join(format!("filter-{name}.png")));
        shoot_find(theme, &dir.join(format!("find-bar-{name}.png")));
    }
}

/// A mailbox whose server answers late: the cached hits now, the rest pending.
struct Mailbox;

impl SearchProvider for Mailbox {
    fn name(&self) -> &'static str {
        "Messages"
    }
    fn search(&self, _: &str) -> Vec<SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &SearchQuery,
        cursor: Option<&tilo_ui::search::SearchCursor>,
        _: usize,
        _: usize,
    ) -> SearchPage {
        if cursor.is_some() {
            return SearchPage::empty();
        }
        let hits = [
            (
                "Ada Lovelace",
                "March invoice",
                "The invoice for March is attached, with the hours by week.",
                "Tue",
                true,
                "ada",
            ),
            (
                "Grace Hopper",
                "Invoice correction",
                "One line was billed twice; here is the corrected total.",
                "12 Mar",
                false,
                "grace",
            ),
            (
                "Alan Turing",
                "Re: invoice numbering",
                "Numbering by quarter works for us. I updated the template.",
                "9 Mar",
                false,
                "alan",
            ),
        ]
        .into_iter()
        .filter(|(.., from)| query.admits("from", &[from]))
        .map(|(sender, subject, snippet, date, unread, from)| {
            SearchResult::new("Messages", subject, format!("folder:inbox/msg:{from}")).with_row(
                SearchRow::message(sender, subject)
                    .snippet(snippet)
                    .date(date)
                    .unread(unread),
            )
        })
        .collect();
        SearchPage::pending(hits, PendingToken::new(1))
    }
    fn poll_page(&self, _: &PendingToken) -> PagePoll {
        PagePoll::Pending
    }
    fn facets(&self) -> Vec<Facet> {
        vec![
            Facet::new("from", "From")
                .value("ada", "Ada Lovelace")
                .value("grace", "Grace Hopper")
                .value("alan", "Alan Turing"),
        ]
    }
}

/// Media and code hits, drawn as what they are.
struct Library;

impl SearchProvider for Library {
    fn name(&self) -> &'static str {
        "Library"
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        let videos = [
            ("Parsing with combinators", "Rust Belt Rust", "41:12"),
            ("Writing a parser by hand", "Systems Week", "18:05"),
        ]
        .into_iter()
        .map(|(title, channel, duration)| {
            SearchResult::new("Videos", title, format!("video:{title}"))
                .with_row(SearchRow::media(title).byline(channel).duration(duration))
        });
        let code = [
            (
                "crates/text/src/parse.rs",
                42,
                "    pub fn parse(input: &str) -> Result<Doc, Error> {",
            ),
            (
                "crates/text/src/lexer.rs",
                7,
                "/// Splits input into tokens before parse runs.",
            ),
        ]
        .into_iter()
        .map(|(path, line, text)| {
            let start = text.find("parse").unwrap_or(0);
            SearchResult::new("Code", path, format!("code:{path}:{line}")).with_row(
                SearchRow::file_hit(path, text)
                    .line(line)
                    .matched(start..start.saturating_add("parse".len())),
            )
        });
        let apps = std::iter::once(
            SearchResult::new("Apps", "Text editor", "app:text").with_icon(Icon::FileText),
        );
        videos
            .chain(code)
            .chain(apps)
            .filter(|hit| {
                query.is_empty() || hit.title.to_lowercase().contains("pars") || hit.kind == "Apps"
            })
            .collect()
    }
}

fn shoot_palette(theme: &Theme, out: &Path, scoped: bool) {
    let presence = PresenceCtx::default();
    let width = 640.0;
    let mut palette = ActionPalette::with_text_renderer(TextRenderer::new());
    if scoped {
        palette.set_providers(vec![Box::new(Mailbox)]);
        palette.open();
        palette.set_scope(Some(ScopeDecl::new("folder:inbox", "Inbox")));
        palette.set_query("invoice");
        palette.select_query();
    } else {
        palette.set_providers(vec![Box::new(Library)]);
        palette.open();
        palette.set_query("parse");
    }
    for _ in 0..8 {
        if !palette.advance_search_page().pending {
            break;
        }
    }
    palette.layout(theme, Point::new(PAD, PAD), width);
    let height = px_dim(palette.rect().y1 + PAD);
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    palette.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    let mut r = OffscreenRenderer::new(px_dim(width + PAD * 2.0), height);
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

/// A list the filter narrows: people, by name.
struct People {
    query: String,
}

impl Filterable for People {
    fn set_filter(&mut self, query: &str) {
        query.clone_into(&mut self.query);
    }
    fn filter_count(&self) -> Option<FilterCount> {
        let names = ["Ada Lovelace", "Alan Turing", "Grace Hopper"];
        let shown = names
            .iter()
            .filter(|name| name.to_lowercase().contains(&self.query.to_lowercase()))
            .count();
        Some(FilterCount::new(shown, 120))
    }
}

fn shoot_filters(theme: &Theme, out: &Path) {
    let presence = PresenceCtx::default();
    let width = 420.0;
    let row = 32.0 + GAP;
    let states: [(&str, bool, f64); 4] = [
        ("", true, width),
        ("a", false, width),
        ("zebra", false, width),
        ("", false, 90.0),
    ];
    let mut scene = Scene::new();
    let mut tr = TextRenderer::new();
    for (index, (query, focused, w)) in states.into_iter().enumerate() {
        let mut field = SearchField::filter().with_placeholder("Filter people by name or team");
        field.set_query(query);
        if !query.is_empty() {
            let mut people = People {
                query: String::new(),
            };
            people.set_filter(query);
            field.set_count(people.filter_count());
        }
        field.set_focused(focused);
        let top = PAD + row * f64::from(u8::try_from(index).unwrap_or(0));
        lay_out_field(&mut field, theme, w, Point::new(PAD, top));
        field.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }
    let height = px_dim(PAD * 2.0 + row * 4.0);
    let mut r = OffscreenRenderer::new(px_dim(width + PAD * 2.0), height);
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

const PROSE: [&str; 4] = [
    "The parser reads the whole input before it builds a tree,",
    "so a parse error names the line it found. Each parse step",
    "keeps the span it covered; the tree keeps every span.",
    "Errors list in reading order.",
];

/// The prose above as a findable document, laid out one line per row.
struct Prose {
    found: Vec<FindMatch>,
    current: usize,
}

impl Findable for Prose {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.found.clear();
        if query.is_empty() {
            return Vec::new();
        }
        for (line, text) in PROSE.iter().enumerate() {
            let (hay, needle) = if options.match_case {
                ((*text).to_owned(), query.to_owned())
            } else {
                (text.to_lowercase(), query.to_lowercase())
            };
            let mut at = 0;
            while let Some(offset) = hay.get(at..).and_then(|rest| rest.find(&needle)) {
                let start = line
                    .saturating_mul(1000)
                    .saturating_add(at)
                    .saturating_add(offset);
                self.found
                    .push(FindMatch::new(start, start.saturating_add(needle.len())));
                at = at.saturating_add(offset).saturating_add(needle.len());
            }
        }
        self.found.clone()
    }
    fn reveal(&mut self, index: usize) {
        self.current = index;
    }
    fn supports_replace(&self) -> bool {
        true
    }
}

fn shoot_find(theme: &Theme, out: &Path) {
    let presence = PresenceCtx::default();
    let width = 640.0;
    let mut tr = TextRenderer::new();
    let mut bar = FindBar::new();
    let mut prose = Prose {
        found: Vec::new(),
        current: 0,
    };
    bar.set_query("parse");
    bar.drive(&mut prose);
    bar.next();
    bar.drive(&mut prose);
    bar.focus_query();
    let band = Rect::new(0.0, 0.0, width, bar.height(theme));
    bar.layout(band, theme, &mut tr);

    let mut scene = Scene::new();
    let reading = theme.text(TextRole::Reading);
    let line_h = f64::from(tr.measure_style("Ag", None, &reading).1);
    let left = 24.0;
    let mut y = band.y1 + 24.0;
    for (line, text) in PROSE.iter().enumerate() {
        for (index, found) in prose.found.iter().enumerate() {
            if found.start / 1000 != line {
                continue;
            }
            let start = found.start % 1000;
            let end = found.end % 1000;
            let before = tr
                .measure_style(text.get(..start).unwrap_or(""), None, &reading)
                .0;
            let hit = tr
                .measure_style(text.get(start..end).unwrap_or(""), None, &reading)
                .0;
            let x0 = left + f64::from(before);
            paint_find_match(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(x0, y, x0 + f64::from(hit), y + line_h),
                index == prose.current,
                theme,
            );
        }
        tr.draw_style(
            &mut scene,
            text,
            theme.text_primary,
            Affine::translate((left, y)),
            None,
            &reading,
        );
        y += line_h;
    }
    bar.paint(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    let mut r = OffscreenRenderer::new(px_dim(width), px_dim(y + 24.0));
    r.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}
