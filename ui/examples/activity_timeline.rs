//! `activity_timeline` example — the audit pane and the history scrubber over
//! a **real** document's change graph.
//!
//! ```bash
//! cargo run -p ui --example ui-lab -- activity_timeline                  # interactive
//! cargo run -p ui --example ui-lab -- activity_timeline --shots o.png    # headless frame
//! ```
//!
//! Nothing here is hand-authored feed data. The example builds a document that
//! two identities edit — one forks, edits under its own actor and merges back,
//! the way a peer's sync arrives — and then asks [`ui::HistorySource`] what
//! happened. Every row's author, timestamp, op class and summary is derived
//! from the automerge changes those edits produced.

use std::collections::HashMap;
use std::path::Path;

use automerge::{ActorId, AutoCommit, ROOT};
use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::text_render::TextRenderer;
use ui::widget::{ActivityTimeline, Column, HistoryScrubber};
use ui::{App, AuthorDirectory, HistoryAuthor, HistorySource, PresenceCtx, State, Theme, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Edit a document as two people and hand back the resulting change graph.
fn collaborated_document() -> AutoCommit {
    let mut alice = AutoCommit::new();
    alice.set_actor(ActorId::from("alice".as_bytes()));
    let c = qdoc::model::init(&mut alice, &ROOT);
    let mut alice_ids = qdoc::IdGen::new(alice.get_actor().to_hex_string());
    qdoc::history::commit_stamped(&mut alice);

    let title = {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(None, "heading");
        dm.splice_text(&id, 0, 0, "Quarterly report");
        id
    };
    qdoc::history::commit_stamped(&mut alice);

    let body = {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(Some(&title), "paragraph");
        dm.splice_text(&id, 0, 0, "Revenue held steady through the quarter.");
        id
    };
    qdoc::history::commit_stamped(&mut alice);

    // Bob syncs, edits under his own actor, and merges back.
    let mut bob = alice.fork();
    bob.set_actor(ActorId::from("bob".as_bytes()));
    {
        let mut bob_ids = qdoc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut bob_ids,
        };
        dm.splice_text(&body, 40, 0, " Costs fell by four percent.");
    }
    qdoc::history::commit_stamped(&mut bob);
    alice.merge(&mut bob).expect("peers merge");

    // Alice renames the heading, adds a scratch note, then deletes it.
    {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.splice_text(&title, 10, 6, "results");
    }
    qdoc::history::commit_stamped(&mut alice);

    let scratch = {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        let id = dm.insert_block(Some(&body), "paragraph");
        dm.splice_text(&id, 0, 0, "TODO: check the headcount figure.");
        id
    };
    qdoc::history::commit_stamped(&mut alice);

    {
        let mut dm = qdoc::DocMut {
            doc: &mut alice,
            c: &c,
            ids: &mut alice_ids,
        };
        dm.remove_block(&scratch);
    }
    qdoc::history::commit_stamped(&mut alice);

    alice
}

pub fn run(args: &[String]) {
    let shot = args
        .iter()
        .position(|a| a == "--shots")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let mut doc = collaborated_document();

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
        println!("  {:<14} {:<10?} {}", e.author.name, e.op_class, e.summary);
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

    match shot {
        Some(out) => render_shot(root_widget, (860, 660), Path::new(&out)),
        None => App::new(State::new(), root_widget).run(),
    }
}

/// Render one headless frame, mirroring the apps' `--shots` mode.
fn render_shot(mut widget: Box<dyn Widget>, viewport: (u32, u32), out: &Path) {
    let (w, h) = viewport;
    let mut state = State::new();
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .expect("root node");
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
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
            )
            .expect("layout");
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
    OffscreenRenderer::new(w, h).render_png(&scene, Color::from_rgb8(0x14, 0x16, 0x1c), out);
    println!("activity_timeline: wrote shot to {}", out.display());
}
