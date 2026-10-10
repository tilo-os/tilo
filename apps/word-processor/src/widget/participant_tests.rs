//! Participant isolation: several participants served
//! by one widget keep independent views, trees and presence, and their views
//! rebase under concurrent edits without touching the document.

use super::participants::{
    MAX_FIND_QUERY_BYTES, MAX_PARTICIPANT_VIEWS, PARTICIPANT_VIEW_TTL, ParticipantViews,
};
use super::*;
use crate::actions::participant::{FindState, ParticipantView, ViewPoint};
use crate::actions::{RegionKind, SearchTarget};
use tilo_actions::{ActionInvoker, ParticipantId, Resource};
use tilo_automerge::ApplyIncoming as _;
use ui::widget::Edits;

fn pid(name: &str) -> ParticipantId {
    ParticipantId::new(name).unwrap()
}

/// A synced widget over paragraphs `texts`, with the body block ids in order.
fn widget_with(texts: &[&str]) -> (WordProcessorWidget, ui::State, Vec<String>) {
    let mut state = crate::initialized_state();
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            wp.model_mut().enter(&mut Edits::lend(&mut state.doc));
        }
        wp.model_mut()
            .insert_str(&mut Edits::lend(&mut state.doc), text);
    }
    wp.sync(&state.doc, &state.root);
    let ids = body_ids(&state.doc);
    assert_eq!(ids.len(), texts.len());
    (wp, state, ids)
}

fn body_ids(doc: &AutoCommit) -> Vec<String> {
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
    tilo_doc::model::region_blocks_in_order(doc, &containers, Region::Body)
}

fn caret(block: &str, offset: usize) -> ParticipantView {
    ParticipantView::at(ViewPoint::new(block, offset), RegionKind::Body)
}

fn selecting(block: &str, from: usize, to: usize) -> ParticipantView {
    ParticipantView {
        anchor: Some(ViewPoint::new(block, from)),
        ..caret(block, to)
    }
}

fn has_resource_prefix(resources: &[Resource], prefix: &str) -> bool {
    fn walk(resource: &Resource, prefix: &str) -> bool {
        resource.id.as_str().starts_with(prefix)
            || resource.children.iter().any(|child| walk(child, prefix))
    }
    resources.iter().any(|resource| walk(resource, prefix))
}

#[test]
fn two_participants_keep_independent_views_and_trees() {
    let (mut wp, state, ids) = widget_with(&["hello world", "second"]);
    let local_before = wp.local_participant_view();
    let doc_before = state.doc.clone().save();

    let (agent, other) = (pid("agent-a"), pid("agent-b"));
    wp.set_participant_view(&agent, &selecting(&ids[0], 0, 5), &state.doc);
    wp.set_participant_view(&other, &caret(&ids[1], 3), &state.doc);

    // Each participant reads back exactly its own view.
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(selecting(&ids[0], 0, 5))
    );
    assert_eq!(
        wp.participant_view(&other, &state.doc),
        Some(caret(&ids[1], 3))
    );
    // The local window is untouched by either.
    assert_eq!(wp.local_participant_view(), local_before);
    assert!(!wp.model().has_selection());

    // Selection-scoped edits are bound only for the selecting participant;
    // the other participant's and the window's are caret-only.
    let bound = |participant: Option<&tilo_actions::ParticipantId>| -> Vec<&'static str> {
        let _scope = participant.map(|participant| {
            ui::invocation_scope::InvokerScope::enter(
                ActionInvoker::actor("agent-actor").with_participant(participant.clone()),
            )
        });
        wp.bound_edits().iter().map(|edit| edit.id).collect()
    };
    assert!(bound(Some(&agent)).contains(&"replace-selection"));
    assert!(!bound(Some(&other)).contains(&"replace-selection"));
    assert!(!bound(None).contains(&"replace-selection"));

    // Views are presentation state: the document bytes did not change.
    assert_eq!(state.doc.clone().save(), doc_before);
}

#[test]
fn unknown_participant_resolves_at_the_document_end() {
    let (wp, state, ids) = widget_with(&["one", "two"]);
    let stranger = pid("never-seen");
    assert_eq!(wp.participant_view(&stranger, &state.doc), None);
    assert_eq!(
        wp.participant_view_or_default(&stranger, &state.doc),
        Some(caret(&ids[1], 3))
    );
    assert!(
        wp.participant_resources(Some(&stranger), &state.doc)
            .is_empty(),
        "a stranger has found nothing"
    );
}

#[test]
fn participant_edits_match_the_pure_projection_of_its_view() {
    let (mut wp, state, ids) = widget_with(&["alpha beta", "gamma"]);
    let agent = pid("agent");
    let view = selecting(&ids[1], 1, 4);
    wp.set_participant_view(&agent, &view, &state.doc);
    let expected: Vec<&str> =
        crate::actions::bound::declarations(&crate::actions::live_presentation_resources(
            &view.ctx(&state.doc).unwrap(),
            &state.doc,
            None,
        ))
        .iter()
        .map(|decl| decl.id)
        .collect();
    let actual: Vec<&str> = {
        let _viewer = ui::invocation_scope::ViewerScope::enter(agent.clone());
        wp.bound_edits().iter().map(|decl| decl.id).collect()
    };
    assert_eq!(actual, expected);
    assert!(actual.contains(&"replace-selection"));
}

#[test]
fn local_view_projects_to_the_published_resource_context() {
    let (mut wp, state, ids) = widget_with(&["hello world", "second line"]);
    wp.model_mut().set_caret(Caret {
        block: ids[0].clone(),
        offset: 2,
    });
    wp.model_mut().set_anchor();
    wp.model_mut().set_caret(Caret {
        block: ids[1].clone(),
        offset: 4,
    });
    wp.sync(&state.doc, &state.root);
    let view = wp.local_participant_view();
    // `WpResourceCtx` has no `PartialEq`; its `Debug` form is what the
    // resource memo keys on, so equal forms mean an equal published tree.
    let projected = view.ctx(&state.doc).expect("a bound caret projects");
    assert!(projected.selection.is_some());
    assert_eq!(
        format!("{projected:?}"),
        format!("{:?}", wp.wp_resource_ctx(&state.doc).unwrap())
    );
}

#[test]
fn views_rebase_under_concurrent_edits() {
    let (mut wp, mut state, ids) = widget_with(&["hello world", "tail"]);
    let (writer, reader) = (pid("writer"), pid("reader"));
    // writer's caret sits after "hello"; reader selects "world".
    wp.set_participant_view(&writer, &caret(&ids[0], 5), &state.doc);
    wp.set_participant_view(&reader, &selecting(&ids[0], 6, 11), &state.doc);

    // A remote peer inserts "Oh, " at the block start.
    let mut remote = state.doc.fork();
    {
        let containers = tilo_doc::model::open(&remote, &automerge::ROOT).unwrap();
        let mut id_gen = tilo_doc::model::seeded_id_gen(&remote, &containers);
        let mut dm = tilo_doc::DocMut {
            doc: &mut remote,
            c: &containers,
            ids: &mut id_gen,
        };
        dm.splice_text(&ids[0], 0, 0, "Oh, ");
    }
    state.doc.merge_incoming(&mut remote).unwrap();
    wp.sync(&state.doc, &state.root);

    assert_eq!(
        wp.participant_view(&writer, &state.doc),
        Some(caret(&ids[0], 9)),
        "an insert before the caret shifts it with its characters"
    );
    assert_eq!(
        wp.participant_view(&reader, &state.doc),
        Some(selecting(&ids[0], 10, 15)),
        "a selection keeps covering the same word"
    );
    let reader_ctx = wp
        .participant_view(&reader, &state.doc)
        .unwrap()
        .ctx(&state.doc)
        .unwrap();
    let span = &reader_ctx.format_spans[0];
    let text = &wp.model().block_texts(&state.doc)[0];
    assert_eq!(
        text.chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect::<String>(),
        "world"
    );

    // Deleting the text around a caret collapses it onto the next survivor.
    let mut remote = state.doc.fork();
    {
        let containers = tilo_doc::model::open(&remote, &automerge::ROOT).unwrap();
        let mut id_gen = tilo_doc::model::seeded_id_gen(&remote, &containers);
        let mut dm = tilo_doc::DocMut {
            doc: &mut remote,
            c: &containers,
            ids: &mut id_gen,
        };
        // "Oh, hello world" → "Oh, world": removes [4, 10), around writer's 9.
        dm.splice_text(&ids[0], 4, 6, "");
    }
    state.doc.merge_incoming(&mut remote).unwrap();
    assert_eq!(
        wp.participant_view(&writer, &state.doc),
        Some(caret(&ids[0], 4))
    );

    // The caret block disappears: park at the document end, no selection.
    wp.set_participant_view(&reader, &selecting(&ids[1], 0, 2), &state.doc);
    let mut remote = state.doc.fork();
    let containers = tilo_doc::model::open(&remote, &automerge::ROOT).unwrap();
    remote.delete(&containers.blocks, ids[1].as_str()).unwrap();
    state.doc.merge_incoming(&mut remote).unwrap();
    wp.sync(&state.doc, &state.root);
    let parked = wp.participant_view(&reader, &state.doc).unwrap();
    assert_eq!(parked.anchor, None);
    assert_eq!(parked.caret, ViewPoint::new(&ids[0], 9));
}

#[test]
fn a_participant_edit_advances_its_own_caret_but_not_the_windows_undo() {
    let (mut wp, mut state, ids) = widget_with(&["hello", "other"]);
    let _ = ui::undo::take_committed_action(&mut wp);
    let (agent, human) = (pid("agent"), pid("human-2"));
    wp.set_participant_view(&agent, &caret(&ids[0], 5), &state.doc);
    wp.set_participant_view(&human, &caret(&ids[0], 5), &state.doc);
    let local_before = wp.local_participant_view();

    let invoker = ActionInvoker::actor("agent-actor").with_participant(agent.clone());
    let outcome = (
        vec![crate::actions::splice_text_action(
            RegionKind::Body,
            &ids[0],
            5,
            5,
            " world",
        )],
        caret(&ids[0], 11),
    );
    wp.apply_participant_edit(&invoker, &mut Edits::lend(&mut state.doc), outcome)
        .unwrap();

    assert_eq!(wp.model().block_texts(&state.doc)[0], "hello world");
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(caret(&ids[0], 11))
    );
    // A second participant at the same point is pushed along by the insert
    // (it was pinned to the text after it); the window's caret is untouched.
    assert_eq!(
        wp.participant_view(&human, &state.doc),
        Some(caret(&ids[0], 11))
    );
    assert_eq!(wp.local_participant_view(), local_before);
    assert_eq!(
        ui::undo::take_committed_action(&mut wp),
        None,
        "a participant's edit never opens the local window's undo group"
    );

    let no_participant = ActionInvoker::actor("agent-actor");
    assert!(
        wp.apply_participant_edit(
            &no_participant,
            &mut Edits::lend(&mut state.doc),
            (Vec::new(), caret(&ids[0], 0))
        )
        .is_err()
    );
}

#[test]
fn participant_presence_uses_the_block_key_and_byte_offsets() {
    let (mut wp, state, ids) = widget_with(&["héllo wörld", "two"]);
    let (typist, selector) = (pid("typist"), pid("selector"));
    wp.set_participant_view(&typist, &caret(&ids[0], 2), &state.doc);
    wp.set_participant_label(&typist, "Agent for Ada");
    wp.set_participant_view(&selector, &selecting(&ids[0], 6, 11), &state.doc);

    let presence = wp.participant_presence(&state.doc);
    assert_eq!(presence.len(), 2);
    let selector_p = presence.iter().find(|p| p.participant == selector).unwrap();
    let typist_p = presence.iter().find(|p| p.participant == typist).unwrap();
    assert_eq!(typist_p.key, block_presence_key(&ids[0]));
    assert_eq!(typist_p.label.as_deref(), Some("Agent for Ada"));
    // "hé" is 3 bytes.
    assert_eq!(typist_p.payload, ui::PresencePayload::Caret { pos: 3 });
    // "héllo " is 7 bytes; "wörld" is 6.
    assert_eq!(
        selector_p.payload,
        ui::PresencePayload::Selection { start: 7, end: 13 }
    );

    assert!(wp.remove_participant(&typist));
    assert_eq!(wp.participant_presence(&state.doc).len(), 1);
}

#[test]
fn the_view_map_is_bounded_expires_and_caps_find() {
    let (_wp, state, ids) = widget_with(&["abc"]);
    let doc = &state.doc;
    let mut views = ParticipantViews::default();
    let start = ui::time::Instant::now();
    for i in 0..=MAX_PARTICIPANT_VIEWS {
        let at = start + ui::time::Duration::from_millis(u64::try_from(i).unwrap());
        views.set(&pid(&format!("p{i}")), &caret(&ids[0], 1), doc, at);
    }
    assert_eq!(views.len(), MAX_PARTICIPANT_VIEWS);
    assert!(
        !views.contains(&pid("p0")),
        "least recently used goes first"
    );
    assert!(views.contains(&pid(&format!("p{MAX_PARTICIPANT_VIEWS}"))));

    let later = start + PARTICIPANT_VIEW_TTL + ui::time::Duration::from_secs(1);
    views.touch(&pid("p5"), later);
    views.expire(later);
    assert_eq!(views.len(), 1);
    assert!(views.contains(&pid("p5")));

    let hits = (0..500).map(|i| SearchTarget {
        block: ids[0].clone(),
        start: i % 3,
        end: i % 3 + 1,
    });
    let finder = ParticipantView {
        find: FindState::new("q".repeat(4 * MAX_FIND_QUERY_BYTES), hits),
        ..caret(&ids[0], 0)
    };
    views.set(&pid("finder"), &finder, doc, later);
    let stored = views.view(&pid("finder"), doc).unwrap();
    assert_eq!(stored.find.query.len(), MAX_FIND_QUERY_BYTES);
    assert_eq!(
        stored.find.hits.len(),
        crate::actions::MAX_RESOURCE_SEARCH_HITS
    );
    assert!(views.remove(&pid("finder")));
    assert!(!views.is_empty());
}

// ---- keyboard ↔ action parity ---------------------------------------------

enum Input {
    Char(char),
    Named(NamedKey),
    Ctrl(char),
}

/// Drive one real key press through `handle_event`.
fn press(wp: &mut WordProcessorWidget, state: &mut ui::State, input: &Input) {
    use winit::keyboard::{ModifiersState, NativeKeyCode, PhysicalKey, SmolStr};
    let mut ev = ui::synthetic_input::key_event(
        winit::keyboard::Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
        None,
        winit::event::ElementState::Pressed,
    );
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    let mut mods = Modifiers::default();
    match input {
        Input::Char(ch) => {
            ev.logical_key = Key::Character(SmolStr::new(ch.to_string()));
            ev.text = Some(SmolStr::new(ch.to_string()));
        }
        Input::Named(key) => {
            ev.logical_key = Key::Named(*key);
            ev.text = None;
        }
        Input::Ctrl(ch) => {
            ev.logical_key = Key::Character(SmolStr::new(ch.to_string()));
            ev.text = None;
            mods = ModifiersState::CONTROL.into();
        }
    }
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ui::testing::Dispatch::new()
        .modifiers(mods)
        .doc(&mut state.doc)
        .path(&state.root)
        .send(
            wp,
            &WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: ev,
                is_synthetic: false,
            },
        );
}

type Step = Box<dyn Fn(&ParticipantView, &AutoCommit) -> crate::actions::editing::EditResult>;

/// Apply `inputs` as key presses in one window, and the matching `steps` as
/// published primitives invoked by another participant (in a second widget
/// over an identical document) starting from the same view. Each keystroke's
/// last envelope must be byte-identical to the step's last envelope, and the
/// documents and resulting carets/selections must agree.
fn assert_keyboard_action_parity(
    case: &str,
    texts: &[&str],
    setup: impl FnOnce(&mut WordProcessorWidget, &mut ui::State, &[String]),
    inputs: &[Input],
    steps: &[Step],
) {
    assert_eq!(inputs.len(), steps.len(), "{case}");
    let (mut keyboard, mut state, ids) = widget_with(texts);
    setup(&mut keyboard, &mut state, &ids);
    keyboard.sync(&state.doc, &state.root);
    keyboard.lifecycle.focused = true;

    let mut agent_state = crate::initialized_state();
    agent_state.doc = state.doc.clone();
    let mut agent_widget = WordProcessorWidget::detached();
    agent_widget.sync(&agent_state.doc, &agent_state.root);
    let agent = pid("parity-agent");
    let invoker = ActionInvoker::actor("agent-actor").with_participant(agent.clone());
    agent_widget.set_participant_view(&agent, &keyboard.local_participant_view(), &agent_state.doc);

    for (input, step) in inputs.iter().zip(steps) {
        let view = agent_widget
            .participant_view(&agent, &agent_state.doc)
            .unwrap();
        let (envelopes, next) = step(&view, &agent_state.doc).expect("the primitive applies");
        press(&mut keyboard, &mut state, input);
        if let Some(last) = envelopes.last() {
            let expected = tilo_actions::EncodedAction::encode(
                crate::actions::WORD_PROCESSOR_ACTION_TYPE,
                last,
            )
            .unwrap();
            assert_eq!(
                keyboard.last_dispatched_for_test(),
                Some(&expected),
                "{case}: keystroke and published primitive build the same envelope"
            );
        }
        agent_widget
            .apply_participant_edit(
                &invoker,
                &mut Edits::lend(&mut agent_state.doc),
                (envelopes, next),
            )
            .unwrap();
    }
    assert_eq!(
        keyboard.model().block_texts(&state.doc),
        agent_widget.model().block_texts(&agent_state.doc),
        "{case}"
    );
    let local = keyboard.local_participant_view();
    let remote = agent_widget
        .participant_view(&agent, &agent_state.doc)
        .unwrap();
    assert_eq!(local.caret, remote.caret, "{case}");
    assert_eq!(
        local.anchor.filter(|a| *a != local.caret),
        remote.anchor.filter(|a| *a != remote.caret),
        "{case}"
    );
}

/// Put the caret at `to` in `block`, anchored at `from`.
fn select_in(wp: &mut WordProcessorWidget, block: &str, from: usize, to: usize) {
    wp.model_mut().set_caret(Caret {
        block: block.to_owned(),
        offset: from,
    });
    if from != to {
        wp.model_mut().set_anchor();
        wp.model_mut().set_caret(Caret {
            block: block.to_owned(),
            offset: to,
        });
    }
}

fn typed(text: &'static str) -> Step {
    Box::new(move |v, d| crate::actions::editing::type_text(v, d, text))
}

type Setup = fn(&mut WordProcessorWidget, &mut ui::State, &[String]);

/// A named scenario: paragraphs, setup, the keystrokes, and the published
/// primitives they must match.
type Case = (
    &'static str,
    &'static [&'static str],
    Setup,
    Vec<Input>,
    Vec<Step>,
);

#[test]
fn keystrokes_match_their_published_primitives() {
    use crate::actions::editing::{
        DeleteDirection, InlineMark, delete, new_paragraph, select_all, toggle_mark,
    };
    let bold: fn() -> Step = || Box::new(|v, d| toggle_mark(v, d, InlineMark::Strong));
    let cases: Vec<Case> = vec![
        (
            "typing matches type_text",
            &["hello world"],
            |wp, _, ids| select_in(wp, &ids[0], 5, 5),
            vec![Input::Char('a'), Input::Char('b'), Input::Char('c')],
            vec![typed("a"), typed("b"), typed("c")],
        ),
        (
            "typing over a selection matches type_text",
            &["abcdef"],
            |wp, _, ids| select_in(wp, &ids[0], 0, 3),
            vec![Input::Char('x'), Input::Char('y')],
            vec![typed("x"), typed("y")],
        ),
        (
            "Enter in a list matches new_paragraph",
            &["item"],
            |wp, state, ids| {
                select_in(wp, &ids[0], 4, 4);
                wp.dispatch_paragraph_style(
                    &mut Edits::lend(&mut state.doc),
                    "list_item",
                    None,
                    Some(false),
                );
            },
            vec![Input::Named(NamedKey::Enter), Input::Named(NamedKey::Enter)],
            vec![Box::new(new_paragraph), Box::new(new_paragraph)],
        ),
        (
            "Backspace and Delete across blocks match delete",
            &["first", "second"],
            |wp, _, ids| select_in(wp, &ids[1], 0, 0),
            vec![
                Input::Named(NamedKey::Backspace),
                Input::Named(NamedKey::Delete),
            ],
            vec![
                Box::new(|v, d| delete(v, d, DeleteDirection::Backward, 1)),
                Box::new(|v, d| delete(v, d, DeleteDirection::Forward, 1)),
            ],
        ),
        (
            "Ctrl+B on a selection matches toggle_mark",
            &["make this bold"],
            |wp, _, ids| select_in(wp, &ids[0], 5, 9),
            vec![Input::Ctrl('b'), Input::Ctrl('b')],
            vec![bold(), bold()],
        ),
        (
            "Ctrl+A matches select_all",
            &["one", "two"],
            |_, _, _| {},
            vec![Input::Ctrl('a')],
            vec![Box::new(select_all)],
        ),
    ];
    for (case, texts, setup, inputs, steps) in cases {
        assert_keyboard_action_parity(case, texts, setup, &inputs, &steps);
    }
}

#[test]
fn typing_over_a_selection_keeps_typed_order() {
    let (mut wp, mut state, ids) = widget_with(&["abcdef"]);
    wp.lifecycle.focused = true;
    select_in(&mut wp, &ids[0], 0, 3);
    press(&mut wp, &mut state, &Input::Char('x'));
    press(&mut wp, &mut state, &Input::Char('y'));
    assert_eq!(wp.model().block_texts(&state.doc)[0], "xydef");
}

#[test]
fn view_operations_move_only_the_invokers_view() {
    use crate::actions::editing::CaretTarget;
    use crate::actions::view_ops::ViewOperation;
    use crate::effects::ViewMove;
    let (mut wp, state, ids) = widget_with(&["find the needle", "and more"]);
    let doc_before = state.doc.clone().save();
    let local_before = wp.local_participant_view();
    let agent = pid("agent");

    wp.move_participant_view(
        &agent,
        &ViewMove::Operation(ViewOperation::SelectBlock {
            block: ids[1].clone(),
        }),
        &state.doc,
    )
    .unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc)
            .unwrap()
            .selection(&state.doc)
            .map(|s| (s.start_block, s.start, s.end_block, s.end)),
        Some((ids[1].clone(), 0, ids[1].clone(), 8))
    );

    wp.move_participant_view(
        &agent,
        &ViewMove::Operation(ViewOperation::FindText {
            query: "needle".into(),
        }),
        &state.doc,
    )
    .unwrap();
    let found = wp.participant_view(&agent, &state.doc).unwrap();
    assert_eq!(found.find.query, "needle");
    assert_eq!(found.find.hits.len(), 1);
    let tree = wp.participant_resources(Some(&agent), &state.doc);
    assert!(has_resource_prefix(&tree, "word-processor.search-hit:"));
    assert!(!has_resource_prefix(
        &wp.participant_resources(None, &state.doc),
        "word-processor.search-hit:"
    ));

    // go-to on the agent's search hit selects it in the agent's view.
    let hit = &found.find.hits[0];
    wp.move_participant_view(
        &agent,
        &ViewMove::NavigateTo {
            block: hit.block.clone(),
            start: hit.start,
            end: hit.end,
        },
        &state.doc,
    )
    .unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc).unwrap().caret,
        ViewPoint::new(&ids[0], 15)
    );

    let end = ViewMove::Operation(ViewOperation::MoveCaret {
        to: CaretTarget::DocumentEnd,
    });
    wp.move_participant_view(&agent, &end, &state.doc).unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc).unwrap().caret,
        ViewPoint::new(&ids[1], 8)
    );

    // None of this touched the window's view or the document.
    assert_eq!(wp.local_participant_view(), local_before);
    assert_eq!(state.doc.clone().save(), doc_before);

    // The window's own view moves the same way.
    wp.apply_local_view_operation(
        &state.doc,
        &ViewMove::Operation(ViewOperation::MoveCaret {
            to: CaretTarget::DocumentStart,
        }),
    )
    .unwrap();
    assert_eq!(wp.model().caret().offset, 0);
    assert_eq!(wp.model().caret().block, ids[0]);
}

/// A delivered invocation by a participant builds its edit at that
/// participant's own caret: the edits the document publishes during its
/// resolution are bound to the invoker's view, never this window's.
#[test]
fn a_delivered_invocation_builds_at_the_invokers_view() {
    let (mut wp, state, ids) = widget_with(&["first paragraph", "tail"]);
    let agent = pid("agent-task:5");
    wp.set_participant_view(&agent, &caret(&ids[1], 4), &state.doc);
    wp.sync(&state.doc, &state.root);
    let (edits, tree) = {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ActionInvoker::actor("agent-actor").with_participant(agent.clone()),
        );
        (wp.bound_edits(), ui::resources::resources(&wp, &state.doc))
    };
    assert_eq!(
        format!(
            "{:?}",
            tree.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        ),
        format!(
            "{:?}",
            wp.participant_resources(Some(&agent), &state.doc)
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
        )
    );
    assert!(
        edits.iter().all(|edit| edit.id != "replace-selection"),
        "a collapsed participant has no selection to replace"
    );
    let typing = edits
        .iter()
        .find(|edit| edit.id == crate::actions::editing::op::TYPE_TEXT)
        .expect("type-text is bound to the participant");
    let args = tilo_actions::ActionArgs::bind(
        &typing.params,
        [("text".to_owned(), tilo_actions::ArgValue::Text("X".into()))],
    )
    .unwrap();
    assert_eq!(
        (typing.build)(&args, 0).unwrap(),
        crate::actions::splice_text_action(RegionKind::Body, &ids[1], 4, 4, "X")
    );
}

/// A certified job or a person's command carries an invoker without a
/// participant: it builds against this window's own view.
#[test]
fn a_participantless_invocation_builds_at_the_windows_own_view() {
    let (mut wp, state, ids) = widget_with(&["first paragraph"]);
    wp.sync(&state.doc, &state.root);
    let local: Vec<&str> = wp.bound_edits().iter().map(|edit| edit.id).collect();
    let edits = {
        let _scope =
            ui::invocation_scope::InvokerScope::enter(ActionInvoker::actor("certified-job"));
        wp.bound_edits()
    };
    assert_eq!(edits.iter().map(|edit| edit.id).collect::<Vec<_>>(), local);
    let table = edits
        .iter()
        .find(|edit| edit.id == "insert-table")
        .expect("insert-table at the window's caret");
    assert!(matches!(
        (table.build)(&tilo_actions::ActionArgs::none(), 0).unwrap(),
        crate::actions::RichTextAction::InsertTable { ref after, .. } if *after == ids[0]
    ));
}

/// Invoke Writer's published `action` for one admitted execution of the
/// rehearsal person, through the app's host door as the host delivers it.
fn invoke_opening_action(
    app: &mut ui::framework::testing::TestApp<crate::framework::WordProcessorApp>,
    participant: &ParticipantId,
    action: &str,
    arguments: serde_json::Value,
) {
    let resource = <crate::framework::Document as ui::tilo_resource::Resource>::KIND;
    let published = tilo_actions::door::survey(&app.harness().resources())
        .into_iter()
        .any(|descriptor| {
            descriptor.address.resource.as_str() == resource && descriptor.address.action == action
        });
    assert!(published, "Writer publishes the opening action `{action}`");
    let invoker =
        ActionInvoker::actor("person:opening-rehearsal").with_participant(participant.clone());
    app.call_by(invoker, resource, action, arguments)
        .unwrap_or_else(|error| panic!("`{action}` for {participant:?}: {error}"));
    app.settle();
}

fn writer_view<O>(
    app: &mut ui::framework::testing::TestApp<crate::framework::WordProcessorApp>,
    read: impl FnOnce(&WordProcessorWidget, &AutoCommit) -> O,
) -> O {
    let doc = app.read_document(Clone::clone);
    app.with_widget(|wp: &mut WordProcessorWidget| read(wp, &doc))
        .expect("the app's view is Writer's widget")
}

/// The PNG `hash` holds sits as an image block, with its real size and the
/// paragraph after it, right after the second of the opening's paragraphs
/// `ids`; its bytes are referenced and described by the document.
fn assert_png_placed_after(doc: &AutoCommit, ids: &[String], hash: &str, png: &[u8]) {
    let containers = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
    let blocks = tilo_doc::model::materialize(doc, &containers).blocks;
    assert_eq!(blocks.len(), 5);
    assert_eq!(blocks[0].id, ids[0]);
    assert_eq!(blocks[0].text, "Hello to Tilo");
    assert_eq!(blocks[1].id, ids[1]);
    assert_eq!(blocks[1].text, "");
    assert_eq!(blocks[2].kind, "image");
    assert_eq!(
        blocks[2].attrs.get("blob"),
        Some(&tilo_doc::AttrVal::Str(hash.to_owned()))
    );
    assert_eq!(
        blocks[2].attrs.get("width"),
        Some(&tilo_doc::AttrVal::Int(2))
    );
    assert_eq!(
        blocks[2].attrs.get("height"),
        Some(&tilo_doc::AttrVal::Int(1))
    );
    assert_eq!(blocks[3].kind, "paragraph");
    assert!(blocks[3].text.is_empty());
    assert_eq!(blocks[4].id, ids[2]);
    assert_eq!(blocks[4].kind, "paragraph");
    assert!(blocks[4].text.is_empty());
    assert!(tilo_doc::model::referenced_content(doc, &containers).contains(hash));
    let meta = tilo_doc::model::blob_meta(doc, &containers, hash).unwrap();
    assert_eq!(meta.mime, "image/png");
    assert_eq!((meta.w, meta.h), (2, 1));
    assert_eq!(meta.len, u64::try_from(png.len()).unwrap());
}

/// Synthetic opening proof only: two host-minted person executions drive actual
/// Writer actions on one document. No Photos export or MCP transport is involved.
#[test]
fn two_person_executions_type_the_opening_and_place_real_png_independently() {
    let callers = [
        tilo_actions::remote::Caller::local_person("person:opening-rehearsal").in_new_execution(),
        tilo_actions::remote::Caller::local_person("person:opening-rehearsal").in_new_execution(),
    ];
    assert_eq!(callers[0].principal(), callers[1].principal());
    assert_ne!(callers[0].execution_id(), callers[1].execution_id());
    let participants =
        callers.map(|caller| ParticipantId::for_execution(caller.execution_id().unwrap()));
    assert_ne!(participants[0], participants[1]);
    let mut app =
        ui::framework::testing::TestApp::<crate::framework::WordProcessorApp>::new().unwrap();
    let resource = <crate::framework::Document as ui::tilo_resource::Resource>::KIND;
    for _ in 0..2 {
        app.call_on(resource, "new-paragraph", serde_json::json!({}))
            .unwrap();
    }
    app.settle();
    let ids = app.read_document(body_ids);
    assert_eq!(ids.len(), 3);
    assert!(app.read_document(|doc| {
        tilo_doc::model::open(doc, &automerge::ROOT)
            .map(|containers| tilo_doc::model::materialize(doc, &containers).blocks)
            .unwrap()
            .iter()
            .all(|block| block.text.is_empty())
    }));
    let local_before = writer_view(&mut app, |wp, _| wp.local_participant_view());
    for (participant, block) in participants.iter().zip(&ids) {
        invoke_opening_action(
            &mut app,
            participant,
            "move-caret",
            serde_json::json!({"to": "point", "block": block, "offset": 0}),
        );
        assert_eq!(
            writer_view(&mut app, |wp, doc| wp.participant_view(participant, doc)),
            Some(caret(block, 0))
        );
    }
    assert_eq!(
        writer_view(&mut app, |wp, _| wp.local_participant_view()),
        local_before
    );
    invoke_opening_action(
        &mut app,
        &participants[0],
        "type-text",
        serde_json::json!({"text": "Hello to Tilo"}),
    );
    assert_eq!(
        writer_view(&mut app, |wp, doc| wp.model().block_texts(doc)),
        ["Hello to Tilo", "", ""]
    );
    let typed_view = writer_view(&mut app, |wp, doc| {
        wp.participant_view(&participants[0], doc)
    })
    .unwrap();
    assert_eq!(typed_view, caret(&ids[0], "Hello to Tilo".len()));
    assert_eq!(
        writer_view(&mut app, |wp, doc| wp
            .participant_view(&participants[1], doc)),
        Some(caret(&ids[1], 0))
    );
    assert_eq!(
        writer_view(&mut app, |wp, _| wp.local_participant_view()),
        local_before
    );

    // Real PNG bytes in the content store, not a stand-in hash or image metadata.
    let png = ui::png::encode_rgba8(2, 1, &[255, 160, 40, 255, 30, 70, 180, 255]).unwrap();
    let content = writer_view(&mut app, |wp, _| wp.inner.content_store().clone());
    let hash = content.put(&png).unwrap();
    assert_eq!(hash, tilo_content::hash_bytes(&png));
    invoke_opening_action(
        &mut app,
        &participants[1],
        "place-image",
        serde_json::json!({tilo_actions::PROMPTED_PARAM: hash}),
    );
    let doc = app.read_document(Clone::clone);
    assert_png_placed_after(&doc, &ids, &hash, &png);
    assert_eq!(content.read(&hash).unwrap(), png);
    assert_eq!(
        writer_view(&mut app, |wp, doc| wp
            .participant_view(&participants[0], doc)),
        Some(typed_view)
    );
    assert_eq!(
        writer_view(&mut app, |wp, doc| wp
            .participant_view(&participants[1], doc)),
        Some(caret(&ids[1], 0))
    );
    assert_eq!(
        writer_view(&mut app, |wp, _| wp.local_participant_view()),
        local_before
    );
}
