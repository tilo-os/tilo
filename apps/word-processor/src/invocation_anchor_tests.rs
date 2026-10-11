use super::*;
use crate::actions::participant::{ParticipantView, ViewPoint};
use automerge::transaction::Transactable as _;
use tilo_actions::{ActionInvoker, ParticipantId};
use ui::widget::Widget as _;

fn fixture() -> (crate::WordProcessorWidget, ui::State, String) {
    let mut state = crate::initialized_state();
    let mut writer = crate::WordProcessorWidget::detached();
    writer.sync(&state.doc, &state.root);
    writer
        .model_mut()
        .insert_str(&mut ui::widget::Edits::lend(&mut state.doc), "A😀éZ");
    state.doc.commit();
    writer.sync(&state.doc, &state.root);
    let c = tilo_doc::model::open(&state.doc, &automerge::ROOT).unwrap();
    let block = tilo_doc::model::blocks_in_order(&state.doc, &c).remove(0);
    (writer, state, block)
}

fn encoded_for(
    writer: &crate::WordProcessorWidget,
    _doc: &AutoCommit,
    participant: &ParticipantId,
) -> EncodedAction {
    let _scope = ui::invocation_scope::InvokerScope::enter(
        ActionInvoker::actor("one-authenticated-principal").with_participant(participant.clone()),
    );
    // The framework owns document publication; use the surface's real
    // participant-bound declaration rather than the retired widget publisher.
    let declaration = ui::framework::ResourceSurface::bound_declarations(writer)
        .into_iter()
        .find(|declaration| declaration.id == "type-text")
        .unwrap();
    let supplied = serde_json::json!({"text": "new content"});
    let args = tilo_actions::ActionArgs::bind_json(
        declaration.params.as_slice(),
        supplied.as_object().unwrap(),
    )
    .unwrap();
    crate::actions::encode(&(declaration.build)(&args, 0).unwrap())
}

#[test]
fn invocation_anchor_real_participant_encoder_keeps_unicode_caret_and_selection_distinct() {
    let (mut writer, state, block) = fixture();
    let a = ParticipantId::new("first-run").unwrap();
    let b = ParticipantId::new("second-run").unwrap();
    let caret = ParticipantView::at(ViewPoint::new(&block, 2), RegionKind::Body);
    let selection = ParticipantView {
        anchor: Some(ViewPoint::new(&block, 1)),
        ..ParticipantView::at(ViewPoint::new(&block, 3), RegionKind::Body)
    };
    writer.set_participant_view(&a, &caret, &state.doc);
    writer.set_participant_view(&b, &selection, &state.doc);
    let local = writer.local_participant_view();
    let heads = state.doc.base_heads();
    let bytes = state.doc.clone().save();
    let address = ActionAddress::new("word-processor.document", "type-text");
    let encoded_a = encoded_for(&writer, &state.doc, &a);
    let encoded_b = encoded_for(&writer, &state.doc, &b);
    // Observation needs only the exact encoded action and document: it does
    // not consume the participant-only outcome queue or guess the local caret.
    assert_eq!(
        project(&state.doc, &address, &encoded_a),
        Some(InteractionAnchor::Text {
            region: "body".into(),
            ranges: vec![InteractionTextRange {
                block: block.clone(),
                start: 2,
                end: 2
            }]
        })
    );
    assert_eq!(
        project(&state.doc, &address, &encoded_b),
        Some(InteractionAnchor::Text {
            region: "body".into(),
            ranges: vec![InteractionTextRange {
                block,
                start: 1,
                end: 3
            }]
        })
    );
    assert_eq!(writer.local_participant_view(), local);
    assert_eq!(writer.participant_view(&a, &state.doc), Some(caret));
    assert_eq!(writer.participant_view(&b, &state.doc), Some(selection));
    assert_eq!(state.doc.base_heads(), heads);
    assert_eq!(state.doc.clone().save(), bytes);
}

#[test]
fn invocation_anchor_refuses_wrong_target_region_range_or_unbounded_payload() {
    let (_, state, block) = fixture();
    let address = ActionAddress::new("word-processor.document", "type-text");
    let build = |region, block: &str, at, del| {
        crate::actions::encode(&RichTextAction::SpliceText {
            region,
            block: block.into(),
            at,
            del,
            ins: "x".into(),
        })
    };
    for encoded in [
        build(RegionKind::Header, &block, 0, 0),
        build(RegionKind::Body, "missing", 0, 0),
        build(RegionKind::Body, &block, 5, 0),
        build(RegionKind::Body, &block, usize::MAX, 1),
        crate::actions::encode(&RichTextAction::ReplaceSelection {
            region: RegionKind::Body,
            start_block: block.clone(),
            start: 0,
            end_block: "different-block".into(),
            end: 1,
            ins: "x".into(),
        }),
    ] {
        assert!(project(&state.doc, &address, &encoded).is_none());
    }
    let encoded = build(RegionKind::Body, &block, 2, 0);
    assert!(
        project(
            &state.doc,
            &ActionAddress::new("foreign", "type-text"),
            &encoded
        )
        .is_none()
    );
    assert!(
        project(
            &state.doc,
            &ActionAddress::new("word-processor.document", "read-text"),
            &encoded
        )
        .is_none()
    );
    let oversized = EncodedAction {
        type_id: encoded.type_id,
        payload: vec![b' '; ui::invocation_anchor::MAX_PROJECTION_BYTES + 1],
    };
    assert!(project(&state.doc, &address, &oversized).is_none());
}

#[test]
fn invocation_anchor_applied_uses_real_encoded_participant_unicode_insertion_and_replacement() {
    for (start, end, expected) in [(2, 2, 13), (1, 3, 12)] {
        let (mut writer, mut state, block) = fixture();
        let participant = ParticipantId::new("actual-run").unwrap();
        let view = ParticipantView {
            anchor: (start != end).then(|| ViewPoint::new(&block, start)),
            ..ParticipantView::at(ViewPoint::new(&block, end), RegionKind::Body)
        };
        writer.set_participant_view(&participant, &view, &state.doc);
        let local = writer.local_participant_view();
        let encoded = encoded_for(&writer, &state.doc, &participant);
        let mut registry = tilo_actions::ActionRegistry::new();
        crate::actions::register(&mut registry);
        let action = registry.decode(&encoded).unwrap();
        let before = state.doc.base_heads();
        let envelope =
            tilo_actions::apply_local(&mut state.doc, action.as_ref(), encoded.clone()).unwrap();
        assert_eq!(envelope.action, encoded);
        assert_ne!(state.doc.base_heads(), before);
        assert_eq!(state.doc.pending_ops(), 0);
        let heads = state.doc.base_heads();
        let bytes = state.doc.clone().save();
        let anchor = project_applied(
            &state.doc,
            &ActionAddress::new("word-processor.document", "type-text"),
            &envelope.action,
        )
        .unwrap();
        assert_eq!(
            anchor,
            InteractionAnchor::Text {
                region: "body".into(),
                ranges: vec![InteractionTextRange {
                    block: block.clone(),
                    start: expected,
                    end: expected
                }]
            }
        );
        let containers = tilo_doc::model::open(&state.doc, &automerge::ROOT).unwrap();
        assert_eq!(
            tilo_doc::model::block_text(&state.doc, &containers, &block).unwrap(),
            if start == end {
                "A😀new contentéZ"
            } else {
                "Anew contentZ"
            }
        );
        assert_eq!(
            crate::actions::splice_target_len(&state.doc, &containers, RegionKind::Body, &block),
            Some(if start == end { 15 } else { 13 })
        );
        assert_eq!(writer.local_participant_view(), local);
        assert_eq!(state.doc.base_heads(), heads);
        assert_eq!(state.doc.clone().save(), bytes);
    }
}

#[test]
fn invocation_anchor_applied_refuses_foreign_cross_block_overflow_and_payload() {
    let (_, state, block) = fixture();
    let address = ActionAddress::new("word-processor.document", "type-text");
    for action in [
        RichTextAction::SpliceText {
            region: RegionKind::Header,
            block: block.clone(),
            at: 0,
            del: 0,
            ins: "x".into(),
        },
        RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: "missing".into(),
            at: 0,
            del: 0,
            ins: "x".into(),
        },
        RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: block.clone(),
            at: usize::MAX,
            del: 0,
            ins: "x".into(),
        },
        RichTextAction::ReplaceSelection {
            region: RegionKind::Body,
            start_block: block,
            start: 0,
            end_block: "other".into(),
            end: 1,
            ins: "x".into(),
        },
    ] {
        assert!(project_applied(&state.doc, &address, &crate::actions::encode(&action)).is_none());
    }
    let oversized = EncodedAction {
        type_id: WORD_PROCESSOR_ACTION_TYPE.into(),
        payload: vec![0; MAX_PROJECTION_BYTES + 1],
    };
    assert!(project_applied(&state.doc, &address, &oversized).is_none());
}
