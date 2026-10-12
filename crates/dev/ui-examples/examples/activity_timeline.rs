//! `activity_timeline` example — the audit pane and the history scrubber over
//! a **real** document's change graph.
//!
//! ```bash
//! cargo run -p ui-examples --example ui-lab -- activity_timeline                  # interactive
//! cargo run -p ui-examples --example ui-lab -- activity_timeline --shots o.png    # headless frame
//! TILO_SHOTS_THEME=light cargo run -p ui-examples --example ui-lab -- activity_timeline --shots o.png
//! ```
//!
//! `--shots` honours `TILO_SHOTS_THEME` (`dark` by default) and composites the
//! frame on the theme's own base surface, so the PNG is what the pane looks
//! like inside an app rather than over a hard-coded backdrop.
//!
//! Nothing here is hand-authored feed data. The example builds a document that
//! two identities edit — one forks, edits under its own actor and merges back,
//! the way a peer's sync arrives — and then asks [`tilo_ui::HistorySource`] what
//! happened. Every row's author, timestamp, op class and summary is derived
//! from the automerge changes those edits produced.

use std::collections::HashMap;
use std::path::Path;
use tilo_automerge::ApplyIncoming;

use automerge::{ActorId, AutoCommit, AutomergeError, ROOT};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::widget::{ActivityTimeline, Column, HistoryScrubber};
use tilo_ui::{App, AuthorDirectory, HistoryAuthor, HistorySource, PresenceCtx, State, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};

/// Edit a document as two people and hand back the resulting change graph.
///
/// # Errors
///
/// Returns an error if merging Bob's fork back into Alice's document fails.
/// Never happens for two forks of the same freshly-created document under
/// this example's synthetic edits, but `AutoCommit::merge` is fallible.
fn collaborated_document() -> Result<AutoCommit, AutomergeError> {
    let mut alice = AutoCommit::new();
    alice.set_actor(ActorId::from("alice".as_bytes()));
    let c = tilo_doc::model::init(&mut alice, &ROOT);
    let mut alice_ids = tilo_doc::IdGen::new(alice.get_actor().to_hex_string());
    tilo_resource::history::commit_stamped(&mut alice);

    let title = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(None, "heading");
        dm.splice_text(&id, 0, 0, "Quarterly report");
        id
    };
    tilo_resource::history::commit_stamped(&mut alice);

    let body = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(Some(&title), "paragraph");
        dm.splice_text(&id, 0, 0, "Revenue held steady through the quarter.");
        id
    };
    tilo_resource::history::commit_stamped(&mut alice);

    // Bob syncs, edits under his own actor, and merges back.
    let mut bob = alice.fork();
    bob.set_actor(ActorId::from("bob".as_bytes()));
    {
        let mut bob_ids = tilo_doc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut bob_ids,
        };
        dm.splice_text(&body, 40, 0, " Costs fell by four percent.");
    }
    tilo_resource::history::commit_stamped(&mut bob);
    alice.merge_incoming(&mut bob)?;

    // Alice renames the heading, adds a scratch note, then deletes it.
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.splice_text(&title, 10, 6, "results");
    }
    tilo_resource::history::commit_stamped(&mut alice);

    let scratch = {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(Some(&body), "paragraph");
        dm.splice_text(&id, 0, 0, "TODO: check the headcount figure.");
        id
    };
    tilo_resource::history::commit_stamped(&mut alice);

    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.remove_block(&scratch);
    }
    tilo_resource::history::commit_stamped(&mut alice);

    Ok(alice)
}

#[allow(
    clippy::unreachable,
    reason = "live activity timeline construction always allocates a wake scope"
)]
pub fn run(args: &[String]) {
    let _detached = tilo_ui::wake::WakeScope::enter_detached();
    let shot = args
        .iter()
        .position(|a| a == "--shots")
        .and_then(|i| args.get(i.saturating_add(1)))
        .cloned();
    let wake_scope = shot.is_none().then(tilo_ui::wake::WakeScope::new);
    let _wake_entry = wake_scope.as_ref().map(tilo_ui::wake::WakeScope::enter);

    let mut doc = match collaborated_document() {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!("activity_timeline: failed to build demo document: {err}");
            return;
        }
    };

    // Bind each automerge actor to a person. In an app this is the local
    // identity on disk plus whatever the collaboration layer knows about the
    // peers; here both are named explicitly so the example is self-contained.
    // An actor with no registration would still get an honest row — see
    // `AuthorDirectory`'s unknown-participant path.
    let mut authors = AuthorDirectory::new();
    authors.register(
        &ActorId::from("alice".as_bytes()).to_hex_string(),
        HistoryAuthor::new("Alice Okafor", 205.0, "a11ce000"),
    );
    authors.register(
        &ActorId::from("bob".as_bytes()).to_hex_string(),
        HistoryAuthor::new("Bob Lindqvist", 28.0, "b0b1e551"),
    );

    let entries = HistorySource::new().with_authors(authors).entries(&mut doc);
    println!(
        "derived {} entries from the document's change graph:",
        entries.len()
    );
    for e in &entries {
        println!(
            "  {:<14} {:<10?} {}",
            e.author.name, e.entry_kind, e.summary
        );
    }

    let timeline = ActivityTimeline::with_entries(entries.clone())
        .on_revert(|id| eprintln!("revert clicked: {id:?}"));
    let scrubber = HistoryScrubber::new(entries)
        .on_preview(|id| eprintln!("preview tick: {id:?}"))
        .on_restore(|id| eprintln!("restore tick: {id:?}"));

    let root_widget: Box<dyn Widget> = Box::new(
        Column::new()
            .push(Box::new(scrubber))
            .push(Box::new(timeline)),
    );

    match (shot, wake_scope) {
        (Some(out), _) => render_shot(root_widget, (860, 660), Path::new(&out)),
        (None, Some(scope)) => App::from_scoped_parts(scope, State::new(), root_widget).run(),
        (None, None) => unreachable!("live activity timeline always allocates its scope"),
    }
}

/// Render one headless frame, mirroring the apps' `--shots` mode.
#[allow(
    clippy::cast_precision_loss,
    reason = "viewport dimensions are screen-sized pixel counts, always far below 2^24 (the largest exactly-representable f32 integer)"
)]
fn render_shot(mut widget: Box<dyn Widget>, viewport: (u32, u32), out: &Path) {
    let (w, h) = viewport;
    let state = State::new();
    let theme = crate::registry::shot_theme();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = match taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    ) {
        Ok(root) => root,
        Err(err) => {
            eprintln!("activity_timeline: failed to build root node: {err}");
            return;
        }
    };
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        let layout_result = taffy.compute_layout_with_measure(
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
        );
        if let Err(err) = layout_result {
            eprintln!("activity_timeline: failed to compute layout: {err}");
            return;
        }
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, theme.tokens.colors.surfaces.base, out);
    println!(
        "activity_timeline: wrote {} shot to {}",
        theme.name,
        out.display()
    );
}
