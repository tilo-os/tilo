//! `word-processor` — the runnable word-processor app. Opens a document
//! (`.odt` / `.md`, or a blank doc) in a `WordProcessorWidget` and runs it as a
//! `ui` application, the same way the terminal and file-explorer apps do.
//!
//! Usage:
//!   word-processor [FILE]              run interactively (FILE optional)
//!   word-processor --shots OUT [FILE]  render one headless frame to OUT.png
//!   word-processor --shots OUT --history [FILE]
//!                                      ...with the version-history strip open
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

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use word_processor::{open_document, WordProcessorWidget};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceKey;
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
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--shots" => {
                shots = args.get(i + 1).cloned();
                i += 2;
            }
            "--history" => {
                history = true;
                i += 1;
            }
            // `--scenario NAME` seeds a specific editor state for a `--shots` frame:
            // `selection` (a live text selection), `caret-scroll` (viewport followed
            // the caret to the bottom of a long document), `spellcheck` (misspelled
            // words carry squiggles).
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
        render_shot(state, Box::new(wp), (1000, 700), Path::new(out))?;
        return Ok(());
    }

    // `--shots --scenario NAME`: seed a specific editor state so the single
    // headless frame proves a live behaviour (selection, caret-follow, squiggles).
    if let (Some(out), Some(name)) = (shots.as_deref(), scenario.as_deref()) {
        let (mut state, mut wp) = build_concrete(file.as_deref())?;
        match name {
            "selection" => seed_selection(&mut state, &mut wp),
            "caret-scroll" => seed_caret_scroll(&mut state, &mut wp),
            "spellcheck" => seed_spellcheck(&mut state, &mut wp),
            "word-drag-right" => seed_word_drag(&mut state, &mut wp, true),
            "word-drag-left" => seed_word_drag(&mut state, &mut wp, false),
            other => return Err(format!("unknown --scenario {other:?}").into()),
        }
        render_shot(state, Box::new(wp), (1000, 700), Path::new(out))?;
        return Ok(());
    }

    // Plain `--shots`: build the concrete widget so the transient status bubble
    // (which replaced the bottom strip) can be armed — pinned fully visible — for
    // the single headless frame, then render.
    if let Some(out) = shots.as_deref() {
        let (state, mut wp) = build_concrete(file.as_deref())?;
        wp.arm_status_bubble_for_shots();
        render_shot(state, Box::new(wp), (1000, 700), Path::new(out))?;
        return Ok(());
    }

    let (mut state, widget) = build(file.as_deref())?;
    {
        // Collaboration (best-effort, opt-in via env): give this instance a
        // distinct CRDT actor so two editors are distinguishable authors, then
        // hand the App a presence-broadcast scope. The App already owns a
        // `CollaborationManager` that connects to `QUICK_COLLAB_SOCK` (when set)
        // and, while the event loop runs, pumps remote ops into `State`,
        // broadcasts local doc deltas + caret/selection presence, and feeds the
        // resulting `PresenceCtx` into the widget's draw — so wiring here is
        // just the per-instance identity + scope, mirroring the notes app. With
        // no socket set this is a no-op and single-user editing is unchanged.
        // The same id drives the Automerge actor (so two instances are distinct
        // CRDT authors) and is the colour/label key peers attribute presence to.
        // A missing QUICK_APP_ID falls back to a per-process id so two
        // un-configured instances still get distinct actors.
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
        // Announce ourselves under a stable document scope: this drives the
        // late-join handshake (peers already in the room reply with full state)
        // and is the scope under which our pointer presence is broadcast. The
        // per-block caret/selection presence the widget reports rides on top.
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
            id: "word-processor".to_string(),
        }));
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

/// `--scenario selection`: a short paragraph with the first few words selected, so
/// the frame shows the selection highlight. The selection is built with the shared
/// word-motion path (the same one Ctrl+Shift+Right drives).
fn seed_selection(state: &mut State, wp: &mut WordProcessorWidget) {
    state.doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut state.doc,
        "The quick brown fox jumps over the lazy dog. Drag across any words to select them.",
    );
    wp.set_focus(true);
    {
        let m = wp.model_mut();
        m.move_to_block(&state.doc, 0);
        m.move_home();
        m.set_anchor();
        m.move_word_right(&state.doc);
        m.move_word_right(&state.doc);
        m.move_word_right(&state.doc);
    }
    wp.sync(&mut state.doc, &state.root);
}

/// `--scenario caret-scroll`: a long multi-paragraph document with the caret at the
/// end. Focus + an armed caret-reveal, then a settle frame, so the viewport has
/// already followed the caret to the bottom when the shot renders.
fn seed_caret_scroll(state: &mut State, wp: &mut WordProcessorWidget) {
    state.doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    wp.sync(&mut state.doc, &state.root);
    for i in 0..40 {
        wp.model_mut().insert_str(
            &mut state.doc,
            &format!("Paragraph {}. This line carries a little body text so the page fills up.", i + 1),
        );
        wp.model_mut().enter(&mut state.doc);
    }
    wp.set_focus(true);
    wp.arm_caret_reveal();
    settle_frame(state, wp);
}

/// `--scenario spellcheck`: a sentence with several misspellings, with the spell
/// cache warmed synchronously so the single frame paints the red squiggles (the
/// live editor computes these on a background worker instead).
fn seed_spellcheck(state: &mut State, wp: &mut WordProcessorWidget) {
    state.doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut state.doc,
        "This sentance conatins a few mispelled wrods to show the squiggle underlines.",
    );
    wp.set_focus(true);
    // Repopulate the materialized blocks the warm pass reads, then warm the cache.
    wp.sync(&mut state.doc, &state.root);
    wp.warm_spellcheck_for_shots();
}

/// `--scenario word-drag-{right,left}`: double-click a word, then drag by a whole
/// word in that direction — driven through the real pointer path — so the frame
/// proves the double-clicked word stays anchored and the selection grows by
/// whole words (dragging left used to drop the word and collapse to one char).
///
/// The sentence "The quick brown fox jumps over the lazy dog." double-clicks
/// "jumps" (chars 20..25). Right drags into "over" (→ "jumps over"); left drags
/// into "fox" (→ "fox jumps").
fn seed_word_drag(state: &mut State, wp: &mut WordProcessorWidget, rightward: bool) {
    state.doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut().insert_str(
        &mut state.doc,
        "The quick brown fox jumps over the lazy dog.",
    );
    wp.set_focus(true);
    // Lay the page out first so pointer hit-testing resolves to real glyphs.
    settle_frame(state, wp);
    // Double-click char 22 (inside "jumps"), then drag: right to char 28 (inside
    // "over"), or left to char 17 (inside "fox").
    let to = if rightward { 28 } else { 17 };
    wp.shots_word_drag(&mut state.doc, &state.root, 22, to);
    wp.sync(&mut state.doc, &state.root);
}

/// Run one headless five-pass frame (no PNG) so viewport-follow / layout settles
/// before the real shot renders. Mirrors [`render_shot`]'s pipeline on borrowed
/// state; the scroll math is theme-independent, so any theme suffices here.
fn settle_frame(state: &mut State, wp: &mut WordProcessorWidget) {
    let (w, h) = (1000u32, 700u32);
    let theme = Theme::dark();
    let permission = PermissionContext::permissive();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    wp.sync(&mut state.doc, &state.root);
    let child = wp.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
            &[child],
        )
        .expect("settle layout root");
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .expect("settle layout compute");
    }
    wp.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    wp.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
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

/// Render a single headless frame to `out` (mirrors the other apps' `--shots`).
fn render_shot(mut state: State, mut widget: Box<dyn Widget>, viewport: (u32, u32), out: &Path) -> Result<(), Box<dyn Error>> {
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

    let mut scene = Scene::new();
    widget.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, Color::TRANSPARENT, out);
    println!("word-processor: wrote shot to {}", out.display());
    Ok(())
}
