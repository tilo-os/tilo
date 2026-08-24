//! `word-processor` — the runnable word-processor app. Opens a document
//! (`.odt` / `.md`, or a blank doc) in a `WordProcessorWidget` and runs it as a
//! `ui` application, the same way the terminal and file-explorer apps do.
//!
//! Usage:
//!   word-processor [FILE]              run interactively (FILE optional)
//!   word-processor --shots OUT [FILE]  render one headless frame to OUT.png
//!   word-processor --shots OUT --size WxH [FILE]
//!   word-processor --shots OUT --history [FILE]
//!                                      ...with the version-history strip open
//!   word-processor --shots OUT --scenario remark
//!                                      ...mid-gesture: a phrase selected, the
//!                                      context menu open, and the compose
//!                                      surface beside it carrying that
//!                                      paragraph's address
//!
//! Collaboration: launched from the shell, the compositor's broker is
//! reached through the descriptor the launcher injects — nothing to
//! configure. Run outside a session, point two instances at one shell:
//!
//! ```bash
//! QUICK_COLLAB_SOCK="$XDG_RUNTIME_DIR/qos/services/collab-json.sock"
//! QUICK_APP_ID=alice word-processor doc.md &
//! QUICK_APP_ID=bob   word-processor doc.md
//! ```

// Thin build only: pull the shared toolkit dylib
// (`libruntime_dylib.so`) into the link graph. It statically contains
// the `ui` toolkit this app links -- and with it wgpu, vello,
// cosmic-text, automerge, taffy and fluent -- so with `-C
// prefer-dynamic` rustc serves `ui` and libstd from the dylib instead
// of baking ~18 MB of `.text` into this binary. Naming the crate here
// is what keeps rustc from eliminating it as unused. Compiled out in
// normal (static) builds.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use word_processor::{open_document, WordProcessorWidget};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::text_render::TextRenderer;
use ui::{App, PresenceCtx, State, Theme, Widget};
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use vello::Scene;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut shots: Option<String> = None;
    let mut file: Option<PathBuf> = None;
    let mut history = false;
    let mut scenario: Option<String> = None;
    // Viewport for `--shots`. A shot is only worth as much as the window it is
    // taken in, and the narrow tiled windows where the page has to be fitted to
    // the canvas are exactly the ones the default does not cover.
    let mut size = (1000_u32, 700_u32);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--shots" => {
                shots = args.get(i + 1).cloned();
                i += 2;
            }
            "--size" => {
                if let Some((w, h)) = args.get(i + 1).and_then(|s| s.split_once(['x', 'X'])) {
                    match (w.trim().parse(), h.trim().parse()) {
                        (Ok(w), Ok(h)) => size = (w, h),
                        _ => return Err(format!("--size wants WIDTHxHEIGHT, got {:?}", args[i + 1]).into()),
                    }
                } else {
                    return Err("--size wants WIDTHxHEIGHT, e.g. --size 800x1014".into());
                }
                i += 2;
            }
            "--history" => {
                history = true;
                i += 1;
            }
            "--scenario" => {
                scenario = args.get(i + 1).cloned();
                i += 2;
            }
            a if a.starts_with("--") => i += 1,
            a => {
                file = Some(PathBuf::from(a));
                i += 1;
            }
        }
    }

    if let (Some(out), true) = (shots.as_deref(), history) {
        // The strip renders the document's own change graph, so a shot of it is
        // only worth anything over a document that has one. Give the document a
        // few authored revisions first, through the same editing path a user
        // takes, then open the strip.
        let (mut state, mut wp) = build_concrete(file.as_deref())?;
        seed_revisions(&mut state, &mut wp);
        // Pin the floating status readout visible so the shot captures it.
        wp.arm_status_bubble_for_shots();
        render_shot(state, wp, size, Path::new(out), None)?;
        return Ok(());
    }

    // Plain `--shots`: build the concrete widget so the transient status bubble
    // (which replaced the bottom strip) can be armed — pinned fully visible — for
    // the single headless frame, then render.
    if let Some(out) = shots.as_deref() {
        // "Say something about this": a phrase selected in a real document, the
        // ordinary right-click menu, and the compose surface beside it carrying
        // that paragraph's address. The destinations are demo values, seeded
        // here and only here — the editor itself never invents a roster.
        let demo = (scenario.as_deref() == Some("remark")).then(seed_remark_demo);
        let (mut state, mut wp) = build_concrete(demo.as_deref().or(file.as_deref()))?;
        if scenario.as_deref() == Some("remark") {
            select_demo_phrase(&mut state, &mut wp);
            wp.set_remark_destinations(vec![
                ui::widget::DestinationOption::localised(qos_reference::Destination::channel(
                    "c_run", "run-of-show",
                )),
                ui::widget::DestinationOption::localised(qos_reference::Destination::participant(
                    "u_xaver", "Xaver", false,
                )),
                ui::widget::DestinationOption::localised(qos_reference::Destination::participant(
                    "agent_claude", "Claude", true,
                )),
            ]);
        } else {
            wp.arm_status_bubble_for_shots();
        }
        render_shot(state, wp, size, Path::new(out), scenario.as_deref())?;
        return Ok(());
    }

    let (mut state, widget) = build(file.as_deref())?;
    {
        // Give this instance a distinct CRDT actor so two editors are
        // distinguishable authors. Nothing else is wired here: the `App`
        // owns a `CollaborationManager`, announces itself in the workspace
        // it was launched into, pumps remote ops into `State`, broadcasts
        // local deltas and caret/selection presence, and feeds the
        // resulting `PresenceCtx` into the widget's draw — all of it
        // without an app saying so. A missing QUICK_APP_ID falls back to a
        // per-process id so two un-configured instances still get distinct
        // actors.
        let app_id = std::env::var("QUICK_APP_ID")
            .unwrap_or_else(|_| format!("wp-{}", std::process::id()));
        state
            .doc
            .set_actor(automerge::ActorId::from(app_id.as_bytes()));

        // The document being edited is a record identified by its path, and
        // the widget names it. Installing the record set is what puts its
        // deltas on that record's own channel instead of one shared by every
        // word-processor window in the room — which is what used to merge two
        // people's unrelated documents into each other.
        //
        // The set is in-memory: this app's durable form is the file on disk,
        // not a per-record automerge store, and adding a second copy of the
        // truth is not what records are for. What the set gives it is
        // identity, routing and the room's index of which documents it holds.
        let (room, room_autosave) = ui::persist::open("dev.qos.word-processor");
        let records = ui::RecordSet::in_memory().with_home(
            std::env::var("QOS_WORKSPACE_ID")
                .or_else(|_| std::env::var("QUICK_CLUSTER_ID"))
                .unwrap_or_default(),
        );
        let mut app = App::new(state, widget)
            .with_autosave(room_autosave)
            .with_records(records, room.doc);
        // Register the word processor's action decoder so the Ctrl+K palette and
        // remote replay can reconstruct every document-mutating operation.
        word_processor::actions::register(app.action_registry_mut());
        app.run();
    }
    Ok(())
}

/// Give `widget`'s document a real authored history, so `--shots --history` has
/// something worth drawing.
///
/// Three working sessions: two by the local author either side of a long break,
/// one by a collaborator in between — forked, edited under its own actor, merged
/// back, the shape a sync round has. That is what makes the strip's version band
/// mean something: the boundaries come from a change of author and a gap in the
/// clock, not from anything seeded here.
///
/// The sessions are committed with explicit wall clocks (the same primitive the
/// FS layer uses when it replays a durable chain) because the gaps are the point;
/// the first and last revisions go through the editor's own commit path.
///
/// Two of the runs are then **named**, one of them by the collaborator under its
/// own actor, so the strip shows a name somebody else wrote. Nothing here seeds
/// the history *pane*: it seeds the document, and the pane derives its band from
/// the resulting change graph exactly as it does for a document a user typed.
fn seed_revisions(state: &mut State, wp: &mut WordProcessorWidget) {
    state
        .doc
        .set_actor(automerge::ActorId::from("alice".as_bytes()));
    wp.sync(&mut state.doc, &state.root);
    state.commit_action(None, None);

    wp.model_mut()
        .insert_str(&mut state.doc, "Quarterly report");
    wp.model_mut()
        .set_paragraph_style(&mut state.doc, "heading", Some(1), None);
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut()
        .set_paragraph_style(&mut state.doc, "paragraph", None, None);
    wp.model_mut()
        .insert_str(&mut state.doc, "Revenue held steady through the quarter.");
    state.commit_action(None, None);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let base = now - 5 * 3600;

    // Session one: five more sentences, half a minute apart.
    for i in 0..5 {
        wp.model_mut()
            .insert_str(&mut state.doc, " Margins improved slightly.");
        qdoc::history::commit_stamped_at(&mut state.doc, base + i * 30);
    }

    // Session two, an hour later, by a collaborator.
    let mut peer = state.doc.fork();
    peer.set_actor(automerge::ActorId::from("Bob Chen".as_bytes()));
    let mut peer_first = None;
    for i in 0..4 {
        let c = qdoc::model::open(&peer, &state.root).expect("qdoc schema");
        let blocks = qdoc::model::materialize(&peer, &c).blocks;
        let last = blocks.last().expect("a paragraph to append to");
        let at = last.text.chars().count();
        let mut ids = qdoc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut { doc: &mut peer, c: &c, ids: &mut ids };
        dm.splice_text(&last.id, at, 0, " Costs fell by four percent.");
        drop(dm);
        let hash = qdoc::history::commit_stamped_at(&mut peer, base + 3600 + i * 30);
        if i == 0 {
            peer_first = hash;
        }
    }
    // The collaborator names its own session, in the document, under its own
    // actor — so everyone else's strip says who wrote the name.
    if let (Some(first), Some(last)) = (peer_first, peer.get_heads().first().copied()) {
        let start = ui::history_source::entry_id_from_hash(&first.to_string());
        let end = ui::history_source::entry_id_from_hash(&last.to_string());
        ui::history_groups::name_range(&mut peer, &start, Some(&end), "Costs section");
        qdoc::history::commit_stamped_at(&mut peer, base + 3600 + 150);
    }
    state.doc.merge(&mut peer).expect("peers merge");

    // Session three, three hours after that, back to the local author.
    wp.sync(&mut state.doc, &state.root);
    let mut session_three_start = None;
    for i in 0..6 {
        wp.model_mut()
            .insert_str(&mut state.doc, " Headcount is unchanged.");
        let hash = qdoc::history::commit_stamped_at(&mut state.doc, base + 4 * 3600 + i * 25);
        if i == 0 {
            session_three_start = hash;
        }
    }
    // And the local author names theirs.
    if let (Some(first), Some(last)) = (
        session_three_start,
        state.doc.get_heads().first().copied(),
    ) {
        let start = ui::history_source::entry_id_from_hash(&first.to_string());
        let end = ui::history_source::entry_id_from_hash(&last.to_string());
        ui::history_groups::name_range(&mut state.doc, &start, Some(&end), "Headcount pass");
        qdoc::history::commit_stamped_at(&mut state.doc, now - 120);
    }

    wp.toggle_history(&mut state.doc);
    wp.sync(&mut state.doc, &state.root);
}

/// Like [`build`] but keeps the concrete widget type.
fn build_concrete(file: Option<&Path>) -> Result<(State, WordProcessorWidget), Box<dyn Error>> {
    match file {
        Some(p) => {
            let (state, widget, _fmt) = open_document(p)?;
            Ok((state, widget))
        }
        None => Ok((State::new(), WordProcessorWidget::new())),
    }
}

/// Build the document state + widget: open `file` if given, else a blank doc.
fn build(file: Option<&Path>) -> Result<(State, Box<dyn Widget>), Box<dyn Error>> {
    match file {
        Some(p) => {
            let (state, widget, _fmt) = open_document(p)?;
            Ok((state, Box::new(widget)))
        }
        None => Ok((State::new(), Box::new(WordProcessorWidget::new()))),
    }
}

/// The demo document the `remark` scenario says something about. Its second
/// paragraph is the one selected; the phrase is located in it by search rather
/// than by a hard-coded offset, so editing the prose cannot silently move the
/// selection off the words it is meant to be about.
const DEMO_PARAGRAPH: &str = "Sound check runs from four to five. The desk is locked after that, so anything the band wants changed has to be said before then.";
const DEMO_PHRASE: &str = "The desk is locked after that";

/// Write the demo document to a temp file and return its path, so the shot
/// opens it through the same codec path a `.md` on disk takes.
fn seed_remark_demo() -> PathBuf {
    let path = std::env::temp_dir().join("qos-wp-remark-demo.md");
    let doc = format!(
        "# Run of show\n\nDoors at seven, the support act at half past, and the hall is ours until eleven.\n\n{DEMO_PARAGRAPH}\n\nTwo hundred and forty seats are sold; the balcony stays shut.\n"
    );
    let _ = std::fs::write(&path, doc);
    path
}

/// Select [`DEMO_PHRASE`] inside the demo document's second paragraph, through
/// the same caret movements a person's keyboard produces.
fn select_demo_phrase(state: &mut State, wp: &mut WordProcessorWidget) {
    let byte = DEMO_PARAGRAPH.find(DEMO_PHRASE).unwrap_or(0);
    let start = DEMO_PARAGRAPH[..byte].chars().count();
    let len = DEMO_PHRASE.chars().count();
    let doc = &state.doc;
    let m = wp.model_mut();
    // Block 0 is the heading, 1 the opening paragraph, 2 the one selected.
    m.move_to_block(doc, 2);
    m.move_home();
    m.clear_anchor();
    for _ in 0..start {
        m.move_right(doc);
    }
    m.set_anchor();
    for _ in 0..len {
        m.move_right(doc);
    }
}

/// Render a single headless frame to `out` (mirrors the other apps' `--shots`).
fn render_shot(mut state: State, mut widget: WordProcessorWidget, viewport: (u32, u32), out: &Path, scenario: Option<&str>) -> Result<(), Box<dyn Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) },
            ..Default::default()
        },
        &[child],
    )?;
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, trr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    // A gesture aimed at the page needs the page shaped first: the laid-out
    // geometry a click resolves a caret against is produced by drawing. Take a
    // warm-up frame, the way a real session's first frame precedes its first
    // click, then perform the gesture and photograph the result.
    if scenario == Some("remark") {
        let mut warm = Scene::new();
        widget.draw_with_presence(&mut warm, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
        let at = widget.debug_selection_point();
        widget.debug_right_click(&state.doc, at);
    }

    let mut scene = Scene::new();
    widget.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, Color::TRANSPARENT, out);
    println!("word-processor: wrote shot to {}", out.display());
    Ok(())
}
