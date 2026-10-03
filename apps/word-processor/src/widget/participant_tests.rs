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

fn pid(name: &str) -> ParticipantId {
    ParticipantId::new(name).unwrap()
}

/// A synced widget over paragraphs `texts`, with the body block ids in order.
fn widget_with(texts: &[&str]) -> (WordProcessorWidget, ui::State, Vec<String>) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            wp.model_mut().enter(&mut state.doc);
        }
        wp.model_mut().insert_str(&mut state.doc, text);
    }
    wp.sync(&mut state.doc, &state.root);
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

fn has_action(resources: &[Resource], id: &str) -> bool {
    fn walk(resource: &Resource, id: &str) -> bool {
        resource.actions.iter().any(|spec| spec.id == id)
            || resource.children.iter().any(|child| walk(child, id))
    }
    resources.iter().any(|resource| walk(resource, id))
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

    // Selection-scoped operations exist only in the selecting participant's
    // tree; the other participant's and the window's trees are caret-only.
    let agent_tree = wp.participant_resources(Some(&agent), &state.doc);
    let other_tree = wp.participant_resources(Some(&other), &state.doc);
    let local_tree = wp.participant_resources(None, &state.doc);
    let selection = format!("word-processor.selection:{}:0:{}:5", ids[0], ids[0]);
    assert!(has_action(&agent_tree, "replace-selection"));
    assert!(has_resource_prefix(&agent_tree, &selection));
    assert!(!has_action(&other_tree, "replace-selection"));
    assert!(!has_resource_prefix(
        &other_tree,
        "word-processor.selection:"
    ));
    assert!(!has_action(&local_tree, "replace-selection"));

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
        !wp.participant_resources(Some(&stranger), &state.doc)
            .is_empty()
    );
}

#[test]
fn participant_tree_matches_the_pure_projection_of_its_view() {
    let (mut wp, state, ids) = widget_with(&["alpha beta", "gamma"]);
    let agent = pid("agent");
    let view = selecting(&ids[1], 1, 4);
    wp.set_participant_view(&agent, &view, &state.doc);
    let expected = crate::actions::provide_resources(&view.ctx(&state.doc).unwrap(), &state.doc);
    let actual = wp.participant_resources(Some(&agent), &state.doc);
    let ids_of = |resources: &[Resource]| -> Vec<String> {
        fn walk(resource: &Resource, out: &mut Vec<String>) {
            for spec in &resource.actions {
                out.push(format!("{}#{}", resource.id.as_str(), spec.id));
            }
            for child in &resource.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        for resource in resources {
            walk(resource, &mut out);
        }
        out
    };
    assert_eq!(ids_of(&actual), ids_of(&expected));
}

#[test]
fn local_view_projects_to_the_published_resource_context() {
    let (mut wp, mut state, ids) = widget_with(&["hello world", "second line"]);
    wp.model_mut().set_caret(Caret {
        block: ids[0].clone(),
        offset: 2,
    });
    wp.model_mut().set_anchor();
    wp.model_mut().set_caret(Caret {
        block: ids[1].clone(),
        offset: 4,
    });
    wp.sync(&mut state.doc, &state.root);
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
    wp.sync(&mut state.doc, &state.root);

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
    wp.sync(&mut state.doc, &state.root);
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
    wp.apply_participant_edit(&invoker, &mut state.doc, outcome)
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
            &mut state.doc,
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
    assert_eq!(typist_p.key, block_presence_key(false, &ids[0]));
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
    let start = ui::clock::Instant::now();
    for i in 0..=MAX_PARTICIPANT_VIEWS {
        let at = start + ui::clock::Duration::from_millis(u64::try_from(i).unwrap());
        views.set(&pid(&format!("p{i}")), &caret(&ids[0], 1), doc, at);
    }
    assert_eq!(views.len(), MAX_PARTICIPANT_VIEWS);
    assert!(
        !views.contains(&pid("p0")),
        "least recently used goes first"
    );
    assert!(views.contains(&pid(&format!("p{MAX_PARTICIPANT_VIEWS}"))));

    let later = start + PARTICIPANT_VIEW_TTL + ui::clock::Duration::from_secs(1);
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
    let taffy: TaffyTree<()> = TaffyTree::new();
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    // SAFETY: as in `widget::tests::type_char` — every public field tolerates
    // the zero bit pattern and the ones `handle_event` reads are overwritten.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
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
    wp.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &mods,
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: NodeId::new(0),
            taffy: &taffy,
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
    keyboard.sync(&mut state.doc, &state.root);
    keyboard.lifecycle.focused = true;

    let mut agent_state = ui::State::new();
    agent_state.doc = state.doc.clone();
    let mut agent_widget = WordProcessorWidget::new();
    agent_widget.sync(&mut agent_state.doc, &agent_state.root);
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
            .apply_participant_edit(&invoker, &mut agent_state.doc, (envelopes, next))
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
                wp.dispatch_paragraph_style(&mut state.doc, "list_item", None, Some(false));
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
    use crate::actions::view_ops::{ViewOperation, WORD_PROCESSOR_VIEW_OPERATION_TYPE};
    let (mut wp, mut state, ids) = widget_with(&["find the needle", "and more"]);
    let doc_before = state.doc.clone().save();
    let local_before = wp.local_participant_view();
    let agent = pid("agent");
    let invoker = ActionInvoker::actor("agent-actor").with_participant(agent.clone());
    let encode = |op: &ViewOperation| {
        tilo_actions::EncodedAction::encode_operation(WORD_PROCESSOR_VIEW_OPERATION_TYPE, op)
            .unwrap()
    };

    let select = encode(&ViewOperation::SelectBlock {
        block: ids[1].clone(),
    });
    wp.invoke_participant_view_operation(&invoker, &select.type_id, &select.payload, &state.doc)
        .unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc)
            .unwrap()
            .selection(&state.doc)
            .map(|s| (s.start_block, s.start, s.end_block, s.end)),
        Some((ids[1].clone(), 0, ids[1].clone(), 8))
    );

    let find = encode(&ViewOperation::FindText {
        query: "needle".into(),
    });
    wp.invoke_participant_view_operation(&invoker, &find.type_id, &find.payload, &state.doc)
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

    // search-hit#go-to for the agent selects the hit in the agent's view.
    let hit = &found.find.hits[0];
    let go = tilo_actions::EncodedAction::encode_operation(
        crate::actions::WORD_PROCESSOR_OPERATION_TYPE,
        &crate::actions::WordProcessorOperation::NavigateTo {
            block: hit.block.clone(),
            start: hit.start,
            end: hit.end,
        },
    )
    .unwrap();
    wp.invoke_participant_view_operation(&invoker, &go.type_id, &go.payload, &state.doc)
        .unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc).unwrap().caret,
        ViewPoint::new(&ids[0], 15)
    );

    let end = encode(&ViewOperation::MoveCaret {
        to: CaretTarget::DocumentEnd,
    });
    wp.invoke_participant_view_operation(&invoker, &end.type_id, &end.payload, &state.doc)
        .unwrap();
    assert_eq!(
        wp.participant_view(&agent, &state.doc).unwrap().caret,
        ViewPoint::new(&ids[1], 8)
    );

    // None of this touched the window's view or the document.
    assert_eq!(wp.local_participant_view(), local_before);
    assert_eq!(state.doc.clone().save(), doc_before);
    assert!(
        wp.invoke_participant_view_operation(
            &ActionInvoker::actor("agent-actor"),
            &end.type_id,
            &end.payload,
            &state.doc
        )
        .is_err()
    );

    // Without an invoker the same operation moves the window's own caret.
    let start = encode(&ViewOperation::MoveCaret {
        to: CaretTarget::DocumentStart,
    });
    assert!(
        wp.start_operation(
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                &start.type_id,
                &start.payload
            ),
            &mut state.doc,
            &state.root
        )
        .map(|started| started.is_some())
        .unwrap()
    );
    assert_eq!(wp.model().caret().offset, 0);
    assert_eq!(wp.model().caret().block, ids[0]);
    let _ = &mut state;
}

#[test]
fn joined_participants_start_at_the_document_end_and_leave_cleanly() {
    let (mut wp, mut state, ids) = widget_with(&["one", "two"]);
    let agent = pid("agent-task:7");
    assert!(ui::collaboration::participants::join_participant(
        &mut wp, &agent
    ));
    assert!(
        !ui::collaboration::participants::join_participant(&mut wp, &agent),
        "idempotent"
    );
    // No document at join time: placed on the next sync.
    wp.sync(&mut state.doc, &state.root);
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(caret(&ids[1], 3))
    );
    assert_eq!(wp.participant_presence(&state.doc).len(), 1);
    // Per-participant resolution through the Widget hooks.
    assert!(
        ui::resources::resource_for(&wp, Some(&agent), &state.doc, "word-processor.document")
            .is_some()
    );
    ui::collaboration::participants::leave_participant(&mut wp, &agent);
    assert_eq!(wp.participant_view(&agent, &state.doc), None);
    assert!(wp.participant_presence(&state.doc).is_empty());
}

#[test]
fn a_noted_outcome_is_adopted_only_after_its_edit_applied() {
    let (mut wp, mut state, ids) = widget_with(&["split here", "tail"]);
    let agent = pid("agent-task:9");
    wp.set_participant_view(&agent, &caret(&ids[0], 5), &state.doc);
    let view = wp.participant_view(&agent, &state.doc).unwrap();
    let invoker = ActionInvoker::actor("agent-actor").with_participant(agent.clone());

    // Refused (never dispatched): the noted view is dropped.
    {
        let _scope = ui::invocation_scope::InvokerScope::enter(invoker.clone());
        let _ = wp.participant_resources(Some(&agent), &state.doc);
        let (_, next) = crate::actions::editing::new_paragraph(&view, &state.doc).unwrap();
        note_participant_outcome(&next);
    }
    wp.sync(&mut state.doc, &state.root);
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(caret(&ids[0], 5))
    );

    // Applied: the caret lands in the block the split created.
    {
        let _scope = ui::invocation_scope::InvokerScope::enter(invoker);
        let _ = wp.participant_resources(Some(&agent), &state.doc);
        let (envelopes, next) = crate::actions::editing::new_paragraph(&view, &state.doc).unwrap();
        note_participant_outcome(&next);
        let mut host = ui::ActionHost::new();
        for action in &envelopes {
            host.try_dispatch(
                &mut state.doc,
                crate::actions::WORD_PROCESSOR_ACTION_TYPE,
                action,
            )
            .unwrap();
        }
    }
    wp.sync(&mut state.doc, &state.root);
    let order = body_ids(&state.doc);
    assert_eq!(order.len(), 3);
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(caret(&order[1], 0))
    );
}

#[test]
fn a_participant_host_publishes_the_schema_as_its_own_tree() {
    let (mut wp, state, _) = widget_with(&["x"]);
    ui::collaboration::participants::set_participant_host(&mut wp, true);
    let published = ui::resources::resources(&wp, &state.doc);
    let schema = wp.publish_catalogue_schema(&state.doc);
    assert_eq!(published.len(), schema.len());
    assert_eq!(
        published
            .iter()
            .map(|r| r.id.as_str().to_string())
            .collect::<Vec<_>>(),
        schema
            .iter()
            .map(|r| r.id.as_str().to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn plain_paste_matches_the_paste_primitive() {
    use crate::actions::editing::{PasteContent, paste};
    let (mut wp, mut state, ids) = widget_with(&["start end"]);
    wp.model_mut().set_caret(Caret {
        block: ids[0].clone(),
        offset: 6,
    });
    let view = wp.local_participant_view();
    let content = PasteContent::Text("one\ntwo ".into());
    let (envelopes, _) = paste(&view, &state.doc, &content).unwrap();
    wp.paste_plain_text_via_action(&mut state.doc, "one\ntwo ");
    let expected = tilo_actions::EncodedAction::encode(
        crate::actions::WORD_PROCESSOR_ACTION_TYPE,
        envelopes.last().unwrap(),
    )
    .unwrap();
    assert_eq!(wp.last_dispatched_for_test(), Some(&expected));
    assert_eq!(
        wp.model().block_texts(&state.doc),
        vec!["start one".to_string(), "two end".to_string()]
    );
    let order = body_ids(&state.doc);
    assert_eq!(wp.model().caret().block, order[1]);
    assert_eq!(wp.model().caret().offset, 4);
}

#[test]
fn a_delivered_view_operation_moves_only_its_participants_view() {
    use crate::actions::editing::CaretTarget;
    use crate::actions::view_ops::{ViewOperation, WORD_PROCESSOR_VIEW_OPERATION_TYPE};
    let (mut wp, mut state, ids) = widget_with(&["one", "two"]);
    let local_before = wp.local_participant_view();
    let agent = pid("agent-task:3");
    let op = tilo_actions::EncodedAction::encode_operation(
        WORD_PROCESSOR_VIEW_OPERATION_TYPE,
        &ViewOperation::MoveCaret {
            to: CaretTarget::DocumentStart,
        },
    )
    .unwrap();
    {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ActionInvoker::actor("agent-actor").with_participant(agent.clone()),
        );
        assert!(
            wp.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    &op.type_id,
                    &op.payload
                ),
                &mut state.doc,
                &state.root
            )
            .map(|started| started.is_some())
            .unwrap()
        );
    }
    assert_eq!(
        wp.participant_view(&agent, &state.doc),
        Some(caret(&ids[0], 0))
    );
    assert_eq!(wp.local_participant_view(), local_before);
}

/// Lab proof6 root cause: a bound component resolves a delivered invocation
/// through `Widget::resources` on the record document. On a participant
/// host that returned the catalogue schema, whose synthetic view selects
/// `[0, 1)` of the first block, so every agent `type-text` replaced the
/// first character of the first paragraph. During a participant's
/// resolution, `resources` is that participant's own tree.
#[test]
fn a_participant_host_resolves_a_delivered_invocation_against_the_invokers_view() {
    let (mut wp, state, ids) = widget_with(&["first paragraph", "tail"]);
    ui::collaboration::participants::set_participant_host(&mut wp, true);
    let agent = pid("agent-task:5");
    wp.set_participant_view(&agent, &caret(&ids[1], 4), &state.doc);
    let tree = {
        let _scope = ui::invocation_scope::InvokerScope::enter(
            ActionInvoker::actor("agent-actor").with_participant(agent.clone()),
        );
        ui::resources::resources(&wp, &state.doc)
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
        !has_action(&tree, "replace-selection"),
        "no schema selection leaks into a collapsed participant's tree"
    );
    let spec = tree[0]
        .actions
        .iter()
        .find(|spec| spec.id == crate::actions::editing::op::TYPE_TEXT)
        .expect("type-text published");
    let tilo_actions::ActionInput::Prompted { build, .. } = &spec.input else {
        panic!("type-text takes its text")
    };
    let encoded = build("X");
    let action: crate::actions::RichTextAction = serde_json::from_slice(&encoded.payload).unwrap();
    assert_eq!(
        action,
        crate::actions::splice_text_action(RegionKind::Body, &ids[1], 4, 4, "X")
    );
    // Outside any invocation the host still publishes the schema.
    assert_eq!(
        ui::resources::resources(&wp, &state.doc).len(),
        wp.publish_catalogue_schema(&state.doc).len()
    );
}

/// A certified job or a person's command on the host worker carries an
/// invoker without a participant: it resolves against the worker's own view
/// (invocable envelopes), never the declaration-only schema.
#[test]
fn a_participant_host_resolves_a_participantless_invocation_against_its_own_view() {
    let (mut wp, state, _) = widget_with(&["first paragraph"]);
    ui::collaboration::participants::set_participant_host(&mut wp, true);
    let own = wp.publish_resources(&state.doc);
    let tree = {
        let _scope =
            ui::invocation_scope::InvokerScope::enter(ActionInvoker::actor("certified-job"));
        ui::resources::resources(&wp, &state.doc)
    };
    let ids = |resources: &[Resource]| -> Vec<String> {
        fn walk(resource: &Resource, out: &mut Vec<String>) {
            for spec in &resource.actions {
                out.push(format!("{}#{}", resource.id.as_str(), spec.id));
            }
            for child in &resource.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        for resource in resources {
            walk(resource, &mut out);
        }
        out
    };
    assert_eq!(ids(&tree), ids(&own));
    let spec = tree[0]
        .actions
        .iter()
        .find(|spec| spec.id == "insert-table")
        .expect("insert-table at the worker caret");
    let tilo_actions::ActionInput::Immediate(encoded) = &spec.input else {
        panic!("insert-table is immediate")
    };
    assert_eq!(encoded.type_id, crate::actions::WORD_PROCESSOR_ACTION_TYPE);
}
