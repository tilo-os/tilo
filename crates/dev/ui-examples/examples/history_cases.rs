#![allow(
    clippy::single_match_else,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Renders the history surfaces over real documents, one PNG per case.
//!
//! ```bash
//! cargo run -p ui-examples --example ui-shots -- history_cases target/shots-history
//! TILO_SHOTS_THEME=light cargo run -p ui-examples --example ui-shots -- history_cases target/shots-history
//! ```
//!
//! `TILO_SHOTS_THEME` picks the palette (`dark` by default, `light`, or any
//! other [`Theme::named`] preset); the PNGs land in `<dir>/<theme>/`.
//!
//! Every case here builds an **automerge document**, edits it through `qdoc` (or
//! through plain automerge, for the room-shaped documents that carry no editor
//! schema), names runs of it through the real
//! [`tilo_ui::history_groups`] API, and then derives the rows and bands with
//! [`tilo_ui::HistorySource`] + [`tilo_ui::group_entries`] exactly as the word processor
//! and the compositor do. Nothing is a hand-written `HistoryEntry` — except the
//! one case that says so in its own name, where the point is layout under a
//! thousand bands rather than where the bands came from.
//!
//! The cases are the ones a history pane has to survive: a long name, a band of
//! one change, a thousand bands, no history at all, a name somebody else wrote, a
//! narrow viewport, and a band still being appended to while you look at it.

use std::path::{Path, PathBuf};
use tilo_automerge::ApplyIncoming;

use automerge::AutoCommit;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::widget::history_scrubber::ScrubberGroup;
use tilo_ui::{
    ActivityTimeline, AuthorDirectory, GroupingConfig, HistoryEntry, HistoryScrubber,
    HistorySource, Theme, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// A fixed "now" so relative timestamps do not move between runs.
const NOW_SECS: u64 = 1_800_000_000;

fn now() -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(NOW_SECS))
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// `NOW_SECS` as a signed offset, for `tilo_resource::history::commit_stamped_at`
/// (which timestamps in signed seconds-since-epoch). `NOW_SECS` is a small
/// fixed constant, always well inside `i64`'s range.
fn now_i64() -> i64 {
    i64::try_from(NOW_SECS).unwrap_or(i64::MAX)
}

// =====================================================================
// Documents
// =====================================================================

/// A `qdoc` document with three working sessions: two by the local author either
/// side of a long break, one by a peer in between. Returns the document and the
/// ids of every change, oldest first.
fn three_session_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    let root = automerge::ROOT;
    let c = tilo_doc::model::init(&mut doc, &root);
    let mut ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
    let base = now_i64().saturating_sub(6 * 3600);
    tilo_resource::history::commit_stamped_at(&mut doc, base);

    let para = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Quarterly report");
        id
    };
    tilo_resource::history::commit_stamped_at(&mut doc, base.saturating_add(20));

    // Session one: six edits about twenty seconds apart.
    for i in 0..6i64 {
        let at = text_len(&doc, &root);
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&para, at, 0, " Revenue held steady.");
        let stamp = base.saturating_add(40).saturating_add(i.saturating_mul(20));
        tilo_resource::history::commit_stamped_at(&mut doc, stamp);
    }

    // Session two, an hour later, by a peer: forked, edited under its own actor,
    // merged back — the shape a sync round has.
    let mut peer = doc.fork();
    peer.set_actor(automerge::ActorId::from("Ada Lovelace".as_bytes()));
    for i in 0..4i64 {
        let at = text_len(&peer, &root);
        let Some(cc) = tilo_doc::model::open(&peer, &root).ok() else {
            continue;
        };
        let mut pids = tilo_doc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut peer,
            c: &cc,
            ids: &mut pids,
        };
        dm.splice_text(&para, at, 0, " Costs fell by four percent.");
        let stamp = base
            .saturating_add(3600)
            .saturating_add(i.saturating_mul(30));
        tilo_resource::history::commit_stamped_at(&mut peer, stamp);
    }
    #[allow(
        clippy::expect_used,
        reason = "peer forked from this same doc moments ago with no divergent schema; a merge \
                  failure here means the example's own setup is broken, not a recoverable state"
    )]
    doc.merge_incoming(&mut peer).expect("peers merge");

    // Session three, three hours in, back to the local author.
    for i in 0..7i64 {
        let at = text_len(&doc, &root);
        let Some(cc) = tilo_doc::model::open(&doc, &root).ok() else {
            continue;
        };
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &cc,
            ids: &mut ids,
        };
        dm.splice_text(&para, at, 0, " Headcount is unchanged.");
        let stamp = base
            .saturating_add(4 * 3600)
            .saturating_add(i.saturating_mul(25));
        tilo_resource::history::commit_stamped_at(&mut doc, stamp);
    }
    doc
}

fn text_len(doc: &AutoCommit, root: &automerge::ObjId) -> usize {
    let Some(c) = tilo_doc::model::open(doc, root).ok() else {
        return 0;
    };
    tilo_doc::model::materialize(doc, &c)
        .blocks
        .first()
        .map_or(0, |b| b.text.chars().count())
}

/// A document three people took turns editing, so most runs are one change long.
///
/// This is the band-of-one case. A group of one unnamed change is rendered as a
/// plain row, so the alternating changes here are rows rather than bands —
/// which is what makes a history with no structure read exactly as the old flat
/// list did. The two consecutive changes at the start are by the same author, so
/// they are one band: the case is "mostly rows", not "no bands at all".
fn singleton_bands_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    let root = automerge::ROOT;
    let c = tilo_doc::model::init(&mut doc, &root);
    let mut ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
    tilo_resource::history::commit_stamped_at(&mut doc, now_i64().saturating_sub(900));
    let para = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Alice starts.");
        id
    };
    tilo_resource::history::commit_stamped_at(&mut doc, now_i64().saturating_sub(800));

    for (who, text, ago) in [
        ("Ada Lovelace", " Ada adds a line.", 700i64),
        ("alice", " Alice replies.", 600),
        ("Ada Lovelace", " And Ada again.", 500),
    ] {
        let mut peer = doc.fork();
        peer.set_actor(automerge::ActorId::from(who.as_bytes()));
        let at = text_len(&peer, &root);
        let Some(cc) = tilo_doc::model::open(&peer, &root).ok() else {
            continue;
        };
        let mut pids = tilo_doc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut peer,
            c: &cc,
            ids: &mut pids,
        };
        dm.splice_text(&para, at, 0, text);
        tilo_resource::history::commit_stamped_at(&mut peer, now_i64().saturating_sub(ago));
        #[allow(
            clippy::expect_used,
            reason = "peer forked from this same doc moments ago with no divergent schema; a \
                      merge failure here means the example's own setup is broken, not a \
                      recoverable state"
        )]
        doc.merge_incoming(&mut peer).expect("peers merge");
    }
    doc
}

/// The rows a document yields, through the production producer.
fn rows(doc: &mut AutoCommit) -> Vec<HistoryEntry> {
    let mut authors = AuthorDirectory::new();
    // Two known people; anyone else falls through to the honest
    // "Unknown participant" path.
    authors.register(
        &hex_actor("alice"),
        tilo_ui::HistoryAuthor::new("Alice", 210.0, short(&hex_actor("alice"))),
    );
    authors.register(
        &hex_actor("Ada Lovelace"),
        tilo_ui::HistoryAuthor::new("Ada Lovelace", 320.0, short(&hex_actor("Ada Lovelace"))),
    );
    HistorySource::new()
        .with_authors(authors)
        .with_max_entries(4000)
        .entries(doc)
}

fn hex_actor(name: &str) -> String {
    automerge::ActorId::from(name.as_bytes()).to_hex_string()
}

fn short(hex: &str) -> String {
    hex.chars().take(8).collect()
}

// =====================================================================
// Rendering
// =====================================================================

struct Shot {
    dir: PathBuf,
    n: usize,
    theme: Theme,
}

/// The palette the run paints with: `TILO_SHOTS_THEME`, or the dark default.
fn shot_theme() -> (String, Theme) {
    let name = std::env::var("TILO_SHOTS_THEME").unwrap_or_else(|_| "dark".to_string());
    match Theme::named(&name) {
        Some(theme) => (name, theme),
        None => {
            eprintln!("history_cases: unknown TILO_SHOTS_THEME {name:?}, using dark");
            ("dark".to_string(), Theme::dark())
        }
    }
}

impl Shot {
    fn paint<W: Widget>(&mut self, name: &str, w: u32, h: u32, widget: &mut W) {
        let theme = &self.theme;
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        // The pane is placed by hand, the way both hosts place it: the compositor
        // reserves a sheet out of its chrome, the editor a band under its canvas.
        widget.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
        self.n = self.n.saturating_add(1);
        let path = self.dir.join(format!("{:02}-{name}.png", self.n));
        std::fs::create_dir_all(&self.dir).ok();
        OffscreenRenderer::new(w, h).render_png(&scene, theme.tokens.colors.surfaces.base, &path);
        println!("  {}", path.display());
    }
}

fn timeline(
    w: f64,
    h: f64,
    entries: Vec<HistoryEntry>,
    named: Vec<tilo_ui::NamedRange>,
) -> ActivityTimeline {
    let mut t = ActivityTimeline::with_entries(entries);
    t.set_named_ranges(named);
    t.set_rect(Rect::new(0.0, 0.0, w, h));
    t.set_now_for_test(now());
    t
}

fn scrubber(w: f64, entries: Vec<HistoryEntry>, groups: Vec<ScrubberGroup>) -> HistoryScrubber {
    let mut s = HistoryScrubber::new(entries);
    s.set_groups(groups);
    s.set_rect(Rect::new(0.0, 0.0, w, s.natural_height()));
    s.set_now_for_test(now());
    s
}

/// Turn the model's groups into the strip's segments, exactly as the word
/// processor's panel does.
fn segments(entries: &[HistoryEntry], named: &[tilo_ui::NamedRange]) -> Vec<ScrubberGroup> {
    tilo_ui::group_entries(entries, named, &GroupingConfig::default())
        .iter()
        .map(|g| ScrubberGroup {
            start: g.start,
            end: g.end.saturating_sub(1),
            label: g.name.clone(),
            derived: g.title(entries),
            record_id: g.record_id.clone(),
            open: g.open,
        })
        .collect()
}

/// Rounds a rendered pixel measurement (always non-negative, far under
/// `u32::MAX`) to a `u32` for `Shot::paint`'s `w`/`h` parameters.
fn px_u32(v: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "v is a rendered pixel measurement, always non-negative and far below u32::MAX"
    )]
    let out = v.max(0.0).round() as u32;
    out
}

pub fn run(args: &[String]) {
    let (theme_name, theme) = shot_theme();
    let dir = PathBuf::from(
        args.first()
            .cloned()
            .unwrap_or_else(|| "target/shots-history".to_string()),
    )
    .join(&theme_name);
    let mut shot = Shot {
        dir: dir.clone(),
        n: 0,
        theme,
    };
    println!("history cases ({theme_name}) -> {}", dir.display());

    // ---------------------------------------------------------------
    // The main surface, over a real three-session document.
    // ---------------------------------------------------------------
    let mut doc = three_session_doc();
    let (entries, named) = shoot_main_bands(&mut shot, &mut doc);
    shoot_long_and_nested(&mut shot, &doc, &entries);
    let (open_entries, open_named) = shoot_open_band(&mut shot, &doc, &entries);
    let single_entries = shoot_singleton_and_empty(&mut shot);
    shoot_narrow_and_thousand(&mut shot, &entries, &named);
    shoot_version_strip(
        &mut shot,
        &entries,
        &named,
        &open_entries,
        &open_named,
        &single_entries,
    );

    println!("done: {} shots", shot.n);
    let _ = Point::ZERO;
    let _: &Path = shot.dir.as_path();
}

/// A peer names its own session, the local author names their own afternoon,
/// then the default/collapsed/expanded/naming/selection shots. Split out of
/// `run` to keep that under the line cap. Returns the entries and named
/// ranges later sections reuse.
fn shoot_main_bands(
    shot: &mut Shot,
    doc: &mut AutoCommit,
) -> (Vec<HistoryEntry>, Vec<tilo_ui::NamedRange>) {
    let entries = rows(doc);
    println!("  ({} changes in the document)", entries.len());

    // A peer names its own session, in the document, under its own actor — so the
    // band reads "named by Ada Lovelace" for everyone else.
    let mut peer = doc.fork();
    peer.set_actor(automerge::ActorId::from("Ada Lovelace".as_bytes()));
    if let Some(start) = entries.get(8)
        && let Some(end) = entries.get(11)
    {
        tilo_ui::history_groups::name_range(&mut peer, &start.id, Some(&end.id), "Costs section");
    }
    tilo_resource::history::commit_stamped_at(&mut peer, now_i64().saturating_sub(2 * 3600));
    #[allow(
        clippy::expect_used,
        reason = "peer forked from this same doc moments ago with no divergent schema; a merge \
                  failure here means the example's own setup is broken, not a recoverable state"
    )]
    doc.merge_incoming(&mut peer).expect("peers merge");

    // And the local author names their own afternoon.
    if let Some(start) = entries.get(12)
        && let Some(end) = entries.last()
    {
        tilo_ui::history_groups::name_range(doc, &start.id, Some(&end.id), "Headcount pass");
    }
    tilo_resource::history::commit_stamped_at(doc, now_i64().saturating_sub(600));

    let entries = rows(doc);
    let named = tilo_ui::history_groups::read_named_ranges(doc, &author_dir());
    println!("  ({} named runs)", named.len());

    let mut t = timeline(520.0, 640.0, entries.clone(), named.clone());
    shot.paint("bands-default", 520, 640, &mut t);

    // Every band shut. This is what the pane looks like once a history is long
    // enough that nothing auto-expands, so it is the shape most readers see.
    t.set_all_expanded(false);
    shot.paint("bands-collapsed", 520, 640, &mut t);

    t.set_all_expanded(true);
    shot.paint("bands-expanded", 520, 640, &mut t);

    // Mid-interaction: naming a band in place.
    let mut t = timeline(520.0, 640.0, entries.clone(), named.clone());
    #[allow(
        clippy::expect_used,
        reason = "this shot's whole point is a band being named; if the fixture stopped \
                  producing one, the demo case itself is broken"
    )]
    let band = (0..t.row_count())
        .find(|i| t.group_at_row(*i).is_some())
        .expect("a band to name");
    t.begin_naming_group_at_row(band);
    shot.paint("naming-a-band", 520, 640, &mut t);

    // Mid-interaction: a run selected, the bar offering to name it.
    let mut t = timeline(520.0, 640.0, entries.clone(), named.clone());
    t.set_all_expanded(true);
    t.set_selection(Some((14, 18)));
    shot.paint("selection", 520, 640, &mut t);

    (entries, named)
}

/// A band still open: named with no end anchor. Split out of `run` to keep
/// that under the line cap. Returns the entries and named ranges the
/// version-strip section reuses for its own "still open" case.
fn shoot_open_band(
    shot: &mut Shot,
    doc: &AutoCommit,
    entries: &[HistoryEntry],
) -> (Vec<HistoryEntry>, Vec<tilo_ui::NamedRange>) {
    let mut open_doc = doc.clone();
    if let Some(start) = entries.get(12) {
        tilo_ui::history_groups::name_range(&mut open_doc, &start.id, None, "Rework the intro");
    }
    tilo_resource::history::commit_stamped_at(&mut open_doc, now_i64().saturating_sub(300));
    let open_entries = rows(&mut open_doc);
    let open_named = tilo_ui::history_groups::read_named_ranges(&open_doc, &author_dir());
    let mut t = timeline(520.0, 640.0, open_entries.clone(), open_named.clone());
    shot.paint("open-band", 520, 640, &mut t);

    (open_entries, open_named)
}

/// A very long name, on a wide and then a narrow pane, and a name nested
/// inside a name. Split out of `run` to keep that under the line cap.
fn shoot_long_and_nested(shot: &mut Shot, doc: &AutoCommit, entries: &[HistoryEntry]) {
    // A very long name, in the document.
    let long = "Rewrote the introduction, then the second section, then reconsidered \
                the whole framing and put most of it back where it started";
    let mut long_doc = doc.clone();
    if let Some(start) = entries.get(12)
        && let Some(end) = entries.last()
    {
        tilo_ui::history_groups::name_range(&mut long_doc, &start.id, Some(&end.id), long);
    }
    tilo_resource::history::commit_stamped_at(&mut long_doc, now_i64().saturating_sub(500));
    let long_entries = rows(&mut long_doc);
    let long_named = tilo_ui::history_groups::read_named_ranges(&long_doc, &author_dir());
    let mut t = timeline(520.0, 640.0, long_entries.clone(), long_named.clone());
    shot.paint("long-name", 520, 640, &mut t);

    // The same long name on a narrow pane, expanded, so the rows are under
    // pressure too.
    let mut t = timeline(320.0, 640.0, long_entries, long_named);
    t.set_all_expanded(true);
    shot.paint("long-name-narrow-320", 320, 640, &mut t);

    // Nesting: a name inside a name, expanded.
    let mut nested_doc = doc.clone();
    if let Some(start) = entries.first()
        && let Some(end) = entries.last()
    {
        tilo_ui::history_groups::name_range(
            &mut nested_doc,
            &start.id,
            Some(&end.id),
            "Q3 release",
        );
    }
    tilo_resource::history::commit_stamped_at(&mut nested_doc, now_i64().saturating_sub(400));
    let nested_entries = rows(&mut nested_doc);
    let nested_named = tilo_ui::history_groups::read_named_ranges(&nested_doc, &author_dir());
    let mut t = timeline(520.0, 700.0, nested_entries, nested_named);
    t.set_all_expanded(true);
    shot.paint("nested", 520, 700, &mut t);
}

/// Bands of one, exactly one change, and no history at all. Split out of
/// `run` to keep that under the line cap. Returns the bands-of-one entries,
/// which the version-strip section reuses for its own "one band" case.
fn shoot_singleton_and_empty(shot: &mut Shot) -> Vec<HistoryEntry> {
    // Bands of one: rendered as plain rows, so a history with no structure reads
    // exactly as a flat list.
    let mut single = singleton_bands_doc();
    let single_entries = rows(&mut single);
    let mut t = timeline(520.0, 300.0, single_entries.clone(), Vec::new());
    println!(
        "  (bands-of-one case: {} changes, {} lines, {} bands)",
        t.visible_len(),
        t.row_count(),
        (0..t.row_count())
            .filter(|i| t.group_at_row(*i).is_some())
            .count()
    );
    shot.paint("bands-of-one", 520, 300, &mut t);

    // Exactly one change. Not a band, not an empty state — one row, and every
    // affordance that hangs off a band has to be absent rather than stubbed.
    let mut one = single_entries.clone();
    one.truncate(1);
    let mut t = timeline(520.0, 300.0, one.clone(), Vec::new());
    println!(
        "  (one-change case: {} lines, {} bands)",
        t.row_count(),
        (0..t.row_count())
            .filter(|i| t.group_at_row(*i).is_some())
            .count()
    );
    shot.paint("one-change", 520, 300, &mut t);

    // No history at all.
    let mut blank = AutoCommit::new();
    let mut t = timeline(520.0, 300.0, rows(&mut blank), Vec::new());
    shot.paint("no-history", 520, 300, &mut t);

    single_entries
}

/// One id per synthetic history entry: cheap, deterministic, and — unlike a
/// counter — stable if a future case reorders the thousand-bands list.
fn synthetic_id(i: i32) -> [u8; 16] {
    let mut id = [0u8; 16];
    id[0] = u8::try_from(i % 251).unwrap_or(0);
    id[1] = u8::try_from(i / 251).unwrap_or(0);
    id
}

/// One synthetic `HistoryEntry` for the thousand-bands case. `i` ranges
/// `0..2000`, so every conversion below is lossless or provably in range.
fn synthetic_entry(i: i32) -> HistoryEntry {
    let hue = f32::from(i16::try_from(i).unwrap_or(i16::MAX)) * 7.3 % 360.0;
    let elapsed = 2000u64.saturating_sub(u64::try_from(i).unwrap_or(0));
    let delta_chars = usize::try_from((i % 40).saturating_add(1)).unwrap_or(1);
    HistoryEntry {
        id: tilo_ui::HistoryEntryId(synthetic_id(i)),
        author: tilo_ui::HistoryAuthor::new(format!("Person {}", i / 2), hue, "0badf00d"),
        op_class: collab_types::OpClass::Splice,
        artifact_kind: collab_types::ArtifactKind::Note,
        timestamp: now()
            .checked_sub(std::time::Duration::from_secs(elapsed))
            .unwrap_or_else(now),
        summary: format!("+{delta_chars} -0 chars"),
        text_delta: Some((delta_chars, 0)),
        diff_payload: None,
        redaction: None,
        context: None,
    }
}

/// Narrow panes over real bands, and a thousand synthesised bands. Split out
/// of `run` to keep that under the line cap.
fn shoot_narrow_and_thousand(
    shot: &mut Shot,
    entries: &[HistoryEntry],
    named: &[tilo_ui::NamedRange],
) {
    // Narrow, with real bands.
    let mut t = timeline(300.0, 640.0, entries.to_vec(), named.to_vec());
    shot.paint("narrow-300", 300, 640, &mut t);
    let mut t = timeline(300.0, 640.0, entries.to_vec(), named.to_vec());
    t.set_all_expanded(true);
    shot.paint("narrow-300-expanded", 300, 640, &mut t);

    // A thousand bands. This one is the exception: the entries are synthesised
    // rather than produced from a document, because the producer caps a timeline
    // at 200 rows and the point of the case is what the layout does with a
    // thousand bands, not where they came from.
    let many: Vec<HistoryEntry> = (0..2000).map(synthetic_entry).collect();
    let mut t = timeline(520.0, 640.0, many, Vec::new());
    println!(
        "  (thousand-bands case: {} changes rendered as {} lines)",
        t.visible_len(),
        t.row_count()
    );
    shot.paint("thousand-bands-synthetic", 520, 640, &mut t);
}

/// The version strip: default, parked-inside-a-version, naming, a still-open
/// version, narrow widths, no versions at all, and no history at all. Split
/// out of `run` to keep that under the line cap.
fn shoot_version_strip(
    shot: &mut Shot,
    entries: &[HistoryEntry],
    named: &[tilo_ui::NamedRange],
    open_entries: &[HistoryEntry],
    open_named: &[tilo_ui::NamedRange],
    single_entries: &[HistoryEntry],
) {
    let last_tick = entries.len().saturating_sub(1);
    let segs = segments(entries, named);
    let mut s = scrubber(1000.0, entries.to_vec(), segs.clone());
    let h = px_u32(s.natural_height());
    s.set_current_tick(last_tick);
    shot.paint("strip-versions", 1000, h, &mut s);

    // Parked inside a named version.
    let mut s = scrubber(1000.0, entries.to_vec(), segs.clone());
    if let Some(g) = segs.iter().find(|g| g.label.is_some()) {
        s.set_current_tick(g.end);
    }
    shot.paint("strip-inside-a-version", 1000, h, &mut s);

    // Naming from the strip.
    let mut s = scrubber(1000.0, entries.to_vec(), segs.clone());
    s.set_current_tick(last_tick);
    s.begin_naming_current_group();
    shot.paint("strip-naming", 1000, h, &mut s);

    // A version still open.
    let open_segs = segments(open_entries, open_named);
    let mut s = scrubber(1000.0, open_entries.to_vec(), open_segs);
    s.set_current_tick(open_entries.len().saturating_sub(1));
    shot.paint("strip-open-version", 1000, h, &mut s);

    // Narrow strips: the button row has to shed buttons rather than overlap.
    for w in [620u32, 460, 360] {
        let mut s = scrubber(f64::from(w), entries.to_vec(), segs.clone());
        s.set_current_tick(last_tick);
        shot.paint(&format!("strip-narrow-{w}"), w, h, &mut s);
    }

    // The degenerate case: a strip that was never handed versions. One tick per
    // change, nothing snapping — exactly what it was before.
    let mut s = scrubber(1000.0, entries.to_vec(), Vec::new());
    s.set_current_tick(last_tick);
    let plain_h = px_u32(s.natural_height());
    shot.paint("strip-no-versions", 1000, plain_h, &mut s);

    // One change, and none at all.
    let single_segs = segments(single_entries, &[]);
    let mut s = scrubber(1000.0, single_entries.to_vec(), single_segs);
    let single_h = px_u32(s.natural_height());
    shot.paint("strip-bands-of-one", 1000, single_h, &mut s);
    let mut s = scrubber(1000.0, Vec::new(), Vec::new());
    let empty_h = px_u32(s.natural_height());
    shot.paint("strip-no-history", 1000, empty_h, &mut s);
}

fn author_dir() -> AuthorDirectory {
    let mut authors = AuthorDirectory::new();
    authors.register(
        &hex_actor("alice"),
        tilo_ui::HistoryAuthor::new("Alice", 210.0, short(&hex_actor("alice"))),
    );
    authors.register(
        &hex_actor("Ada Lovelace"),
        tilo_ui::HistoryAuthor::new("Ada Lovelace", 320.0, short(&hex_actor("Ada Lovelace"))),
    );
    authors
}
