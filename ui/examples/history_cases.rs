//! Renders the history surfaces over real documents, one PNG per case.
//!
//! ```bash
//! cargo run -p ui --example ui-shots -- history_cases target/shots-history
//! ```
//!
//! Every case here builds an **automerge document**, edits it through `qdoc` (or
//! through plain automerge, for the room-shaped documents that carry no editor
//! schema), names runs of it through the real
//! [`ui::history_groups`] API, and then derives the rows and bands with
//! [`ui::HistorySource`] + [`ui::group_entries`] exactly as the word processor
//! and the compositor do. Nothing is a hand-written `HistoryEntry` — except the
//! one case that says so in its own name, where the point is layout under a
//! thousand bands rather than where the bands came from.
//!
//! The cases are the ones a history pane has to survive: a long name, a band of
//! one change, a thousand bands, no history at all, a name somebody else wrote, a
//! narrow viewport, and a band still being appended to while you look at it.

use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::history_scrubber::ScrubberGroup;
use ui::{
    ActivityTimeline, AuthorDirectory, GroupingConfig, HistoryEntry, HistoryScrubber,
    HistorySource, Theme, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// A fixed "now" so relative timestamps do not move between runs.
const NOW_SECS: u64 = 1_800_000_000;

fn now() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW_SECS)
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
    let c = qdoc::model::init(&mut doc, &root);
    let mut ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
    let base = NOW_SECS as i64 - 6 * 3600;
    qdoc::history::commit_stamped_at(&mut doc, base);

    let para = {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Quarterly report");
        id
    };
    qdoc::history::commit_stamped_at(&mut doc, base + 20);

    // Session one: six edits about twenty seconds apart.
    for i in 0..6 {
        let at = text_len(&doc, &root);
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&para, at, 0, " Revenue held steady.");
        qdoc::history::commit_stamped_at(&mut doc, base + 40 + i * 20);
    }

    // Session two, an hour later, by a peer: forked, edited under its own actor,
    // merged back — the shape a sync round has.
    let mut peer = doc.fork();
    peer.set_actor(automerge::ActorId::from("Ada Lovelace".as_bytes()));
    for i in 0..4 {
        let at = text_len(&peer, &root);
        let cc = qdoc::model::open(&peer, &root).unwrap();
        let mut pids = qdoc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut peer,
            c: &cc,
            ids: &mut pids,
        };
        dm.splice_text(&para, at, 0, " Costs fell by four percent.");
        qdoc::history::commit_stamped_at(&mut peer, base + 3600 + i * 30);
    }
    doc.merge(&mut peer).expect("peers merge");

    // Session three, three hours in, back to the local author.
    for i in 0..7 {
        let at = text_len(&doc, &root);
        let cc = qdoc::model::open(&doc, &root).unwrap();
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &cc,
            ids: &mut ids,
        };
        dm.splice_text(&para, at, 0, " Headcount is unchanged.");
        qdoc::history::commit_stamped_at(&mut doc, base + 4 * 3600 + i * 25);
    }
    doc
}

fn text_len(doc: &AutoCommit, root: &automerge::ObjId) -> usize {
    let c = qdoc::model::open(doc, root).expect("qdoc schema");
    qdoc::model::materialize(doc, &c)
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
    let c = qdoc::model::init(&mut doc, &root);
    let mut ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
    qdoc::history::commit_stamped_at(&mut doc, NOW_SECS as i64 - 900);
    let para = {
        let mut dm = qdoc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let id = dm.insert_block(None, "paragraph");
        dm.splice_text(&id, 0, 0, "Alice starts.");
        id
    };
    qdoc::history::commit_stamped_at(&mut doc, NOW_SECS as i64 - 800);

    for (who, text, ago) in [
        ("Ada Lovelace", " Ada adds a line.", 700i64),
        ("alice", " Alice replies.", 600),
        ("Ada Lovelace", " And Ada again.", 500),
    ] {
        let mut peer = doc.fork();
        peer.set_actor(automerge::ActorId::from(who.as_bytes()));
        let at = text_len(&peer, &root);
        let cc = qdoc::model::open(&peer, &root).unwrap();
        let mut pids = qdoc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut peer,
            c: &cc,
            ids: &mut pids,
        };
        dm.splice_text(&para, at, 0, text);
        qdoc::history::commit_stamped_at(&mut peer, NOW_SECS as i64 - ago);
        doc.merge(&mut peer).expect("peers merge");
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
        ui::HistoryAuthor::new("Alice", 210.0, short(&hex_actor("alice"))),
    );
    authors.register(
        &hex_actor("Ada Lovelace"),
        ui::HistoryAuthor::new("Ada Lovelace", 320.0, short(&hex_actor("Ada Lovelace"))),
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
}

impl Shot {
    fn paint<W: Widget>(&mut self, name: &str, w: u32, h: u32, widget: &mut W) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        // The pane is placed by hand, the way both hosts place it: the compositor
        // reserves a sheet out of its chrome, the editor a band under its canvas.
        widget.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        self.n += 1;
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
    named: Vec<ui::NamedRange>,
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
fn segments(entries: &[HistoryEntry], named: &[ui::NamedRange]) -> Vec<ScrubberGroup> {
    ui::group_entries(entries, named, &GroupingConfig::default())
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

pub fn run(args: &[String]) {
    let dir = args
        .first()
        .cloned()
        .unwrap_or_else(|| "target/shots-history".to_string());
    let mut shot = Shot {
        dir: PathBuf::from(&dir),
        n: 0,
    };
    println!("history cases -> {dir}");

    // ---------------------------------------------------------------
    // The main surface, over a real three-session document.
    // ---------------------------------------------------------------
    let mut doc = three_session_doc();
    let entries = rows(&mut doc);
    println!("  ({} changes in the document)", entries.len());

    // A peer names its own session, in the document, under its own actor — so the
    // band reads "named by Ada Lovelace" for everyone else.
    let mut peer = doc.fork();
    peer.set_actor(automerge::ActorId::from("Ada Lovelace".as_bytes()));
    ui::history_groups::name_range(
        &mut peer,
        &entries[8].id,
        Some(&entries[11].id),
        "Costs section",
    );
    qdoc::history::commit_stamped_at(&mut peer, NOW_SECS as i64 - 2 * 3600);
    doc.merge(&mut peer).expect("peers merge");

    // And the local author names their own afternoon.
    ui::history_groups::name_range(
        &mut doc,
        &entries[12].id,
        Some(&entries[entries.len() - 1].id),
        "Headcount pass",
    );
    qdoc::history::commit_stamped_at(&mut doc, NOW_SECS as i64 - 600);

    let entries = rows(&mut doc);
    let named = ui::history_groups::read_named_ranges(&doc, &author_dir());
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

    // A very long name, in the document.
    let long = "Rewrote the introduction, then the second section, then reconsidered \
                the whole framing and put most of it back where it started";
    let mut long_doc = doc.clone();
    ui::history_groups::name_range(
        &mut long_doc,
        &entries[12].id,
        Some(&entries[entries.len() - 1].id),
        long,
    );
    qdoc::history::commit_stamped_at(&mut long_doc, NOW_SECS as i64 - 500);
    let long_entries = rows(&mut long_doc);
    let long_named = ui::history_groups::read_named_ranges(&long_doc, &author_dir());
    let mut t = timeline(520.0, 640.0, long_entries.clone(), long_named.clone());
    shot.paint("long-name", 520, 640, &mut t);

    // The same long name on a narrow pane, expanded, so the rows are under
    // pressure too.
    let mut t = timeline(320.0, 640.0, long_entries, long_named);
    t.set_all_expanded(true);
    shot.paint("long-name-narrow-320", 320, 640, &mut t);

    // Nesting: a name inside a name, expanded.
    let mut nested_doc = doc.clone();
    ui::history_groups::name_range(
        &mut nested_doc,
        &entries[0].id,
        Some(&entries[entries.len() - 1].id),
        "Q3 release",
    );
    qdoc::history::commit_stamped_at(&mut nested_doc, NOW_SECS as i64 - 400);
    let nested_entries = rows(&mut nested_doc);
    let nested_named = ui::history_groups::read_named_ranges(&nested_doc, &author_dir());
    let mut t = timeline(520.0, 700.0, nested_entries, nested_named);
    t.set_all_expanded(true);
    shot.paint("nested", 520, 700, &mut t);

    // A band still open: named with no end anchor.
    let mut open_doc = doc.clone();
    ui::history_groups::name_range(&mut open_doc, &entries[12].id, None, "Rework the intro");
    qdoc::history::commit_stamped_at(&mut open_doc, NOW_SECS as i64 - 300);
    let open_entries = rows(&mut open_doc);
    let open_named = ui::history_groups::read_named_ranges(&open_doc, &author_dir());
    let mut t = timeline(520.0, 640.0, open_entries.clone(), open_named.clone());
    shot.paint("open-band", 520, 640, &mut t);

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

    // Narrow, with real bands.
    let mut t = timeline(300.0, 640.0, entries.clone(), named.clone());
    shot.paint("narrow-300", 300, 640, &mut t);
    let mut t = timeline(300.0, 640.0, entries.clone(), named.clone());
    t.set_all_expanded(true);
    shot.paint("narrow-300-expanded", 300, 640, &mut t);

    // A thousand bands. This one is the exception: the entries are synthesised
    // rather than produced from a document, because the producer caps a timeline
    // at 200 rows and the point of the case is what the layout does with a
    // thousand bands, not where they came from.
    let many: Vec<HistoryEntry> = (0..2000)
        .map(|i| {
            let mut id = [0u8; 16];
            id[0] = (i % 251) as u8;
            id[1] = (i / 251) as u8;
            HistoryEntry {
                id: ui::HistoryEntryId(id),
                author: ui::HistoryAuthor::new(
                    format!("Person {}", i / 2),
                    (i as f32 * 7.3) % 360.0,
                    "0badf00d",
                ),
                op_class: collab_types::OpClass::Splice,
                artifact_kind: collab_types::ArtifactKind::Note,
                timestamp: now() - std::time::Duration::from_secs(2000 - i as u64),
                summary: format!("+{} -0 chars", i % 40 + 1),
                text_delta: Some((i % 40 + 1, 0)),
                diff_payload: None,
                redaction: None,
            }
        })
        .collect();
    let mut t = timeline(520.0, 640.0, many, Vec::new());
    println!(
        "  (thousand-bands case: {} changes rendered as {} lines)",
        t.visible_len(),
        t.row_count()
    );
    shot.paint("thousand-bands-synthetic", 520, 640, &mut t);

    // ---------------------------------------------------------------
    // The version strip.
    // ---------------------------------------------------------------
    let segs = segments(&entries, &named);
    let mut s = scrubber(1000.0, entries.clone(), segs.clone());
    let h = s.natural_height() as u32;
    s.set_current_tick(entries.len() - 1);
    shot.paint("strip-versions", 1000, h, &mut s);

    // Parked inside a named version.
    let mut s = scrubber(1000.0, entries.clone(), segs.clone());
    if let Some(g) = segs.iter().find(|g| g.label.is_some()) {
        s.set_current_tick(g.end);
    }
    shot.paint("strip-inside-a-version", 1000, h, &mut s);

    // Naming from the strip.
    let mut s = scrubber(1000.0, entries.clone(), segs.clone());
    s.set_current_tick(entries.len() - 1);
    s.begin_naming_current_group();
    shot.paint("strip-naming", 1000, h, &mut s);

    // A version still open.
    let open_segs = segments(&open_entries, &open_named);
    let mut s = scrubber(1000.0, open_entries.clone(), open_segs);
    s.set_current_tick(open_entries.len() - 1);
    shot.paint("strip-open-version", 1000, h, &mut s);

    // Narrow strips: the button row has to shed buttons rather than overlap.
    for w in [620u32, 460, 360] {
        let mut s = scrubber(f64::from(w), entries.clone(), segs.clone());
        s.set_current_tick(entries.len() - 1);
        shot.paint(&format!("strip-narrow-{w}"), w, h, &mut s);
    }

    // The degenerate case: a strip that was never handed versions. One tick per
    // change, nothing snapping — exactly what it was before.
    let mut s = scrubber(1000.0, entries.clone(), Vec::new());
    s.set_current_tick(entries.len() - 1);
    let plain_h = s.natural_height() as u32;
    shot.paint("strip-no-versions", 1000, plain_h, &mut s);

    // One change, and none at all.
    let single_segs = segments(&single_entries, &[]);
    let mut s = scrubber(1000.0, single_entries, single_segs);
    shot.paint(
        "strip-bands-of-one",
        1000,
        s.natural_height() as u32,
        &mut s,
    );
    let mut s = scrubber(1000.0, Vec::new(), Vec::new());
    shot.paint("strip-no-history", 1000, s.natural_height() as u32, &mut s);

    println!("done: {} shots", shot.n);
    let _ = Point::ZERO;
    let _: &Path = shot.dir.as_path();
}

fn author_dir() -> AuthorDirectory {
    let mut authors = AuthorDirectory::new();
    authors.register(
        &hex_actor("alice"),
        ui::HistoryAuthor::new("Alice", 210.0, short(&hex_actor("alice"))),
    );
    authors.register(
        &hex_actor("Ada Lovelace"),
        ui::HistoryAuthor::new("Ada Lovelace", 320.0, short(&hex_actor("Ada Lovelace"))),
    );
    authors
}
