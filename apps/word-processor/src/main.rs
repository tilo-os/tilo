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
//! reached through the `QOS_SVC_COLLAB_FD` descriptor the launcher injects —
//! nothing to configure. A raw process started outside a session has no
//! collaboration service; the descriptor is inherited, not a socket path an
//! app can fabricate.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
// Thin build only: pull the shared toolkit dylib
// (`libruntime_dylib.so`) into the link graph. It statically contains
// the `ui` toolkit this app links -- and with it wgpu, vello,
// cosmic-text, automerge, taffy and fluent -- so with `-C
// prefer-dynamic` rustc serves `ui` and libstd from the dylib instead
// of baking ~18 MB of `.text` into this binary. Naming the crate here
// is what keeps rustc from eliminating it as unused. Compiled out in
// normal (static) builds.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::text_render::TextRenderer;
use ui::{App, PresenceCtx, State, Theme, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use word_processor::{WordProcessorWidget, open_document};

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};

/// An Automerge actor id that is actually distinct from every other replica's.
///
/// `format!("{prefix}-{pid}")` was not: every instance here runs in its own PID
/// namespace, so every instance sees itself as pid 2, every session wrote under
/// the *same* actor, and Automerge refuses a frame whose actor already used that
/// sequence number — frames silently dropped in the one direction that matters,
/// a change made on one machine never reaching another.
///
/// `have` is the actor the document already carries (random per `AutoCommit`, or
/// persisted per state directory via `ui::persist`) — distinct by construction,
/// the property the old id only looked like it had. Deliberately reads no
/// environment: the failure was an id that was a function of things two replicas
/// share, so only a function of its own arguments can be tested against it.
fn distinct_actor(prefix: &str, have: &automerge::ActorId) -> String {
    let hex = have.to_hex_string();
    // `to_hex_string` is pure ASCII hex digits, so any byte offset is also a
    // char boundary; `.get` still keeps this panic-free rather than trusting it.
    let short = hex.get(..hex.len().min(12)).unwrap_or(hex.as_str());
    format!("{prefix}-{short}")
}

/// The name this replica is given, if anything has named it. `QUICK_APP_ID` is
/// how a dev run points two instances at one shell and gets a readable history;
/// `QOS_USER_ID` is the session's own identity, but doesn't reach a sandboxed
/// app (the launcher builds its environment from an allowlist, and this isn't on
/// it) — so in the product neither answers, and the actor comes from
/// [`distinct_actor`].
fn named_actor(prefix: &str) -> Option<String> {
    if let Ok(id) = std::env::var("QUICK_APP_ID")
        && !id.is_empty()
    {
        return Some(id);
    }
    match std::env::var("QOS_USER_ID") {
        Ok(user) if !user.is_empty() => Some(format!("{prefix}-{user}")),
        _ => None,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut shots: Option<String> = None;
    let mut file: Option<PathBuf> = None;
    let mut history = false;
    // An iterator over the args, rather than an index into a collected `Vec`,
    // so `--shots`'s value is just "the next item" — no offset that could
    // walk past the end.
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shots" => {
                shots = args.next();
            }
            "--history" => {
                history = true;
            }
            a if a.starts_with("--") => {}
            a => {
                file = Some(PathBuf::from(a));
            }
        }
    }

    if let (Some(out), true) = (shots.as_deref(), history) {
        // The strip renders the document's own change graph, so a shot of it is
        // only worth anything over a document that has one. Give the document a
        // few authored revisions first, through the same editing path a user
        // takes, then open the strip.
        let (mut state, mut wp) = build_concrete(file.as_deref())?;
        seed_revisions(&mut state, &mut wp)?;
        // Pin the floating status readout visible so the shot captures it.
        wp.arm_status_bubble_for_shots();
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
        // Give this instance a distinct CRDT actor so two editors are
        // distinguishable authors. Nothing else is wired here: the `App`
        // owns a `CollaborationManager`, announces itself in the workspace
        // it was launched into, pumps remote ops into `State`, broadcasts
        // local deltas and caret/selection presence, and feeds the
        // resulting `PresenceCtx` into the widget's draw — all of it
        // without an app saying so. `QUICK_APP_ID`/`QOS_USER_ID` name a
        // known replica; a sandboxed launch has neither, so the fallback has
        // to be sound on its own — see [`distinct_actor`].
        let app_id =
            named_actor("wp").unwrap_or_else(|| distinct_actor("wp", state.doc.get_actor()));
        ui::diag!("word-processor: automerge actor {app_id}");
        state
            .doc
            .set_actor(automerge::ActorId::from(app_id.as_bytes()));

        // The document is a record identified by its path; installing the
        // record set routes its deltas onto that record's own channel instead
        // of one shared by every word-processor window in the room (which
        // would merge unrelated documents together).
        //
        // The set is in-memory: this app's durable form is the file on disk,
        // not a per-record automerge store. What the set gives it is
        // identity, routing, and the room's index of which documents it holds.
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
/// something worth drawing: three working sessions (two by the local author
/// either side of a long break, one by a collaborator merged back in between),
/// committed with explicit wall clocks since the gaps are the point. Two runs
/// are named — one by the collaborator, under its own actor — so the strip
/// shows a name somebody else wrote. Nothing here seeds the history *pane*: it
/// seeds the document, and the pane derives its band from the resulting change
/// graph exactly as for a document a user typed.
///
/// # Errors
///
/// Returns `Err` if the freshly-built demo document doesn't have the schema
/// or the paragraph this seeding assumes, or if the collaborator fork fails
/// to merge back — all of which would mean the widget/model plumbing above
/// it is broken, not that the demo data itself is missing.
fn seed_revisions(state: &mut State, wp: &mut WordProcessorWidget) -> Result<(), Box<dyn Error>> {
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
        .map_or(0, |d| d.as_secs() as i64);
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
        let c = qdoc::model::open(&peer, &state.root)?;
        let blocks = qdoc::model::materialize(&peer, &c).blocks;
        let last = blocks
            .last()
            .ok_or("seed_revisions: no paragraph to append to")?;
        let at = last.text.chars().count();
        let mut ids = qdoc::IdGen::new(peer.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut peer,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&last.id, at, 0, " Costs fell by four percent.");
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
    state.doc.merge(&mut peer)?;

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
    if let (Some(first), Some(last)) = (session_three_start, state.doc.get_heads().first().copied())
    {
        let start = ui::history_source::entry_id_from_hash(&first.to_string());
        let end = ui::history_source::entry_id_from_hash(&last.to_string());
        ui::history_groups::name_range(&mut state.doc, &start, Some(&end), "Headcount pass");
        qdoc::history::commit_stamped_at(&mut state.doc, now - 120);
    }

    wp.toggle_history(&mut state.doc);
    wp.sync(&mut state.doc, &state.root);
    Ok(())
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
fn render_shot(
    mut state: State,
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &Path,
) -> Result<(), Box<dyn Error>> {
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
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
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
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, Color::TRANSPARENT, out);
    println!("word-processor: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod actor_tests {
    use super::distinct_actor;

    /// Two ids minted inside one process must differ, even though anything
    /// derived from the process alone (a pid, which is 2 in every sandbox here)
    /// produces the same string twice — which is exactly what made Automerge
    /// refuse a second replica's frames as duplicate sequence numbers.
    #[test]
    fn two_replicas_in_one_process_do_not_share_an_actor() {
        let a = automerge::AutoCommit::new();
        let b = automerge::AutoCommit::new();
        assert_ne!(
            distinct_actor("wp", a.get_actor()),
            distinct_actor("wp", b.get_actor()),
            "two replicas were given the same actor id, which is the failure \
             that silently drops one of them's changes",
        );
    }

    /// And it still says what it is, so a history can be read.
    #[test]
    fn the_actor_still_names_the_application() {
        let doc = automerge::AutoCommit::new();
        assert!(distinct_actor("wp", doc.get_actor()).starts_with("wp-"));
    }
}
