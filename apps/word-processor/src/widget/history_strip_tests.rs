use super::tests::{mount, pipeline_draw};
use super::*;
use tilo_automerge::ApplyIncoming;
use ui::headless::Framed;

/// A full left click: move, press, release. The release matters — pressing
/// on the scrubber's track latches the handle to the pointer, so a
/// press-only helper leaves the widget mid-drag and the next cursor move
/// drags the version out from under the test.
fn click(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mods = Modifiers::default();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    let mut send = |wp: &mut WordProcessorWidget, ev: WindowEvent| {
        wp.handle_event(
            &ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    };
    send(
        wp,
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (pt.x, pt.y).into(),
        },
    );
    send(
        wp,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
    );
    send(
        wp,
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    );
}

/// Document text as the reader sees it, joined across blocks.
fn body(state: &ui::State) -> String {
    let c = tilo_doc::model::open(&state.doc, &state.root).expect("qdoc schema");
    tilo_doc::model::materialize(&state.doc, &c)
        .blocks
        .iter()
        .map(|b| b.text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An editor holding a document with three committed versions, authored by
/// two identities — the second edit arrives from a peer, merged the way
/// collaboration merges it.
fn editor_with_history(w: u32, h: u32) -> Framed<WordProcessorWidget> {
    let mut state = ui::State::new();
    state
        .doc
        .set_actor(automerge::ActorId::from("alice".as_bytes()));
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    state.commit_action(None, None);

    wp.model_mut().insert_str(&mut state.doc, "first");
    state.commit_action(None, None);

    // Bob forks from Alice's state, edits under his own actor, and syncs
    // back — the same shape `load_incremental` produces on a live peer.
    let mut bob = state.doc.fork();
    bob.set_actor(automerge::ActorId::from("bob".as_bytes()));
    {
        let c = tilo_doc::model::open(&bob, &state.root).expect("qdoc schema");
        let blocks = tilo_doc::model::materialize(&bob, &c).blocks;
        let mut ids = tilo_doc::IdGen::new(bob.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut bob,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&blocks[0].id, 5, 0, " and second");
    }
    ui::tilo_resource::history::commit_stamped(&mut bob);
    state.doc.merge_incoming(&mut bob).expect("peers merge");

    // Keep the fixture's authored labels independent of the developer's
    // local Tilo identity, which production history otherwise registers for
    // the local Automerge actor on refresh.
    for (handle, hue) in [("alice", 210.0_f32), ("bob", 25.0_f32)] {
        let actor = automerge::ActorId::from(handle.as_bytes()).to_hex_string();
        wp.history
            .authors_mut()
            .register(&actor, ui::HistoryAuthor::new(handle, hue, handle));
    }

    mount(state, wp, w, h)
}

/// The history strip shows this document's real versions, opening it reserves a band out of the
/// canvas, and it captures clicks instead of seating the caret.
#[test]
fn the_history_strip_lists_real_versions_in_its_own_band() {
    // Phase: the_history_strip_shows_this_documents_real_versions.
    // The toolbar's History button opens a strip whose ticks are the
    // document's own versions, attributed to the identities that made them.
    // No seeded demo data anywhere in this path.
    {
        let mut framed = editor_with_history(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        assert!(!wp.history.is_open(), "the strip starts closed");

        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        assert!(wp.history.is_open(), "the toolbar action opens it");

        let entries = wp.history.entries();
        assert!(
            entries.len() >= 3,
            "schema init plus two authored edits: {:?}",
            entries
                .iter()
                .map(|e| (&e.author.name, &e.summary))
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = entries.iter().map(|e| e.author.name.as_str()).collect();
        assert!(
            names.contains(&"alice") && names.contains(&"bob"),
            "both identities are named from the change graph, got {names:?}"
        );
        let summaries: Vec<&str> = entries.iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.contains(&"+5 -0 chars"),
            "alice typed five characters: {summaries:?}"
        );
        assert!(
            summaries.contains(&"+11 -0 chars"),
            "bob typed eleven: {summaries:?}"
        );
    }

    // Phase: opening_the_strip_reserves_a_band_out_of_the_canvas.
    // Opening the strip shrinks the canvas rather than painting over the page.
    {
        let mut framed = editor_with_history(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

        let band = wp.history.band();
        assert!(band.height() > 100.0, "the strip has a real band: {band:?}");
        assert!(
            band.y1 <= wp.rect.y1 - LAYOUT_BAR_H,
            "the strip sits above the bottom layout bar"
        );
        assert_eq!(band.width(), wp.rect.width(), "full-width strip");

        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        assert_eq!(
            wp.history.band_height(),
            0.0,
            "closing gives the space back"
        );
    }

    // Phase: the_strip_captures_clicks_instead_of_seating_the_caret.
    // A click on the strip must not fall through to the page and move the
    // caret.
    {
        let mut framed = editor_with_history(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

        let caret_before = wp.model().caret().clone();
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(0), y);
        click(wp, state, tick_pt);
        assert_eq!(
            *wp.model().caret(),
            caret_before,
            "a click on the strip does not reseat the caret in the page"
        );
    }
}

/// Clicking preview shows the old text without editing, and clicking restore returns the earlier text as
/// a new version.
#[test]
fn history_versions_preview_and_restore_without_editing() {
    // Phase: clicking_preview_shows_the_old_text_without_editing.
    // Preview reads the past and leaves the present alone.
    {
        let mut framed = editor_with_history(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);

        let target = wp
            .history
            .entries()
            .iter()
            .position(|e| e.summary == "+5 -0 chars")
            .expect("alice's edit is on the track");
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(target), y);
        click(wp, state, tick_pt);
        let preview_pt = wp.history.preview_button_rect().center();
        click(wp, state, preview_pt);

        assert_eq!(wp.history.preview_text(), Some("first"));
        assert_eq!(body(state), "first and second", "previewing edits nothing");
        assert!(!ui::undo::needs_commit(wp), "previewing is not an edit");
    }

    // Phase: clicking_restore_returns_the_earlier_text_as_a_new_version.
    // THE END-TO-END CLAIM. Click a tick, click Restore, and the document
    // holds the earlier content — with the restore appended to history as a
    // new version rather than history being rewritten.
    {
        let mut framed = editor_with_history(1000, 700);
        let (wp, state) = Framed::split(&mut framed);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        let (wp, state) = pipeline_draw(&mut framed, 1000, 700);
        assert_eq!(body(state), "first and second", "precondition");

        let before: Vec<_> = wp.history.entries().iter().map(|e| e.id).collect();
        // The tick for the version whose summary is alice's five characters.
        let target = wp
            .history
            .entries()
            .iter()
            .position(|e| e.summary == "+5 -0 chars")
            .expect("alice's edit is on the track");

        // Click that tick on the track, then click Restore — both through the
        // production `handle_event`.
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(target), y);
        click(wp, state, tick_pt);
        assert_eq!(
            wp.history.current_tick(),
            target,
            "the click moved the handle"
        );

        let rb = wp.history.restore_button_rect().center();
        click(wp, state, rb);

        assert!(
            ui::undo::needs_commit(wp),
            "the restore asks the host to commit it"
        );
        // This is what `App::commit_action` does on the next frame.
        state.commit_action(None, None);
        ui::undo::on_commit(wp);

        assert_eq!(body(state), "first", "the earlier content is back");

        let (wp, _) = pipeline_draw(&mut framed, 1000, 700);
        let after: Vec<_> = wp.history.entries().iter().map(|e| e.id).collect();
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the restore is a new version, not a rewind"
        );
        for id in &before {
            assert!(
                after.contains(id),
                "every earlier version is still on the track: history was rewritten"
            );
        }
    }
}
