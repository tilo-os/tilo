use super::*;
use automerge::ReadDoc;
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionTransport};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TextEdit {
    field: String,
    value: String,
}
impl Action for TextEdit {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        doc.put(ROOT, &self.field, &self.value)?;
        Ok(())
    }
    fn inverse_encoded(&self, doc: &AutoCommit) -> Option<tilo_actions::EncodedAction> {
        let value = doc
            .get(ROOT, &self.field)
            .ok()?
            .and_then(|(v, _)| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        tilo_actions::EncodedAction::encode(
            "burst.text",
            &Self {
                field: self.field.clone(),
                value,
            },
        )
        .ok()
    }
}

#[derive(Default)]
struct TextOwner {
    emitter: Option<crate::widget::ActionEmitter<String>>,
}
impl Widget for TextOwner {
    crate::provides!(
        dyn crate::history_mode::capability::DocumentHistory,
        dyn crate::widget::controls::ActionControls
    );
    fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {}
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &crate::Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        tree.new_leaf(taffy::Style::default()).unwrap()
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
    fn name(&self) -> &str {
        "Text owner"
    }
}
impl crate::widget::controls::ActionControls for TextOwner {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.emitter
            .as_mut()
            .and_then(crate::widget::ActionEmitter::take)
    }
}
impl crate::history_mode::capability::DocumentHistory for TextOwner {
    fn physical_text_policy(
        &self,
        previous: Option<&tilo_actions::EncodedAction>,
        next: &tilo_actions::EncodedAction,
    ) -> (bool, bool) {
        let next: TextEdit = serde_json::from_slice(&next.payload).unwrap();
        let previous =
            previous.and_then(|encoded| serde_json::from_slice::<TextEdit>(&encoded.payload).ok());
        (
            previous.is_some_and(|previous| previous.field == next.field),
            true,
        )
    }
}
#[derive(Default)]
struct Wire(Vec<tilo_actions::ActionEnvelope>);
impl ActionTransport for Wire {
    fn replicate(&mut self, envelope: &tilo_actions::ActionEnvelope) {
        self.0.push(envelope.clone());
    }
    fn submit_to_host(&mut self, _: &tilo_actions::ActionEnvelope) -> tilo_actions::HostDecision {
        panic!("client-local text must apply locally")
    }
}
fn registry() -> tilo_actions::ActionRegistry {
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<TextEdit>("burst.text");
    registry
}
fn edit(
    binding: &CallbackBinding,
    outer: &mut AutoCommit,
    field: &str,
    value: &str,
    physical: bool,
    wire: &mut Wire,
) {
    let action = TextEdit {
        field: field.into(),
        value: value.into(),
    };
    let spec =
        tilo_actions::ActionSpec::immediate("text", "Edit text", "burst.text", &action).unwrap();
    let resource = tilo_actions::Resource::new("burst", "burst", "Text");
    binding
        .with_record_frame(outer, false, |doc, path| {
            let mut operation =
                BoundOperation::enter(binding, doc, tilo_actions::OperationKind::Document).unwrap();
            let input = if physical {
                Input::Continuing
            } else {
                Input::Discrete
            };
            with_native_text(input, || {
                execute_resource_operation(
                    &mut TextOwner::default(),
                    operation.doc,
                    path,
                    &registry(),
                    wire,
                    ResourceOperation {
                        target_route: None,
                        address: &tilo_actions::ActionAddress::new("burst", "text"),
                        resource: &resource,
                        spec: &spec,
                        encoded: tilo_actions::EncodedAction::encode("burst.text", &action)
                            .unwrap(),
                        actor: &binding.author.0,
                        local_author: &binding.author.0,
                        broker_admitted: false,
                        invocation: tilo_actions::InvocationId::new(),
                        source_invocation: None,
                        ambient: None,
                        scope: None,
                    },
                )
            })
            .unwrap();
            assert!(
                !operation.finish(),
                "canonical text execution violates no effect frontier"
            );
        })
        .unwrap();
}
fn value(records: &RecordStore, checkout: &RecordCheckout, field: &str) -> String {
    records
        .read(&checkout.id, |doc| {
            doc.get(ROOT, field)
                .unwrap()
                .and_then(|(value, _)| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        })
        .unwrap()
}
fn restore(
    binding: &CallbackBinding,
    outer: &mut AutoCommit,
    request: crate::undo::UndoRequest,
) -> crate::state::UndoEntry {
    binding
        .history_request_with_registry(outer, request, Some(&registry()))
        .unwrap()
        .expect("the actual owned history entry")
}

#[test]
fn native_text_keeps_first_compensation_latest_redo_and_different_field_boundaries() {
    let records = crate::record::RecordOwners::portable(
        crate::portable_storage::PortableStorage::new("burst", "device").unwrap(),
    )
    .open("so.tilo.test", "room")
    .unwrap();
    let checkout = RecordCheckout::new(
        crate::record::RecordId::minted(),
        crate::record::RecordKind::new("text"),
    );
    records.create_record(&checkout).unwrap();
    let (binding, _scope, _owner) =
        super::author_writer_tests::mount(&records, &checkout, "You", 1, true);
    let mut outer = AutoCommit::new();
    let mut wire = Wire::default();
    for typed in ["a", "ab", "abc"] {
        edit(&binding, &mut outer, "first", typed, true, &mut wire);
    }
    for typed in ["x", "xy"] {
        edit(&binding, &mut outer, "second", typed, true, &mut wire);
    }
    assert_eq!(
        wire.0.len(),
        5,
        "coalescing history does not combine canonical envelopes"
    );
    restore(&binding, &mut outer, crate::undo::UndoRequest::Undo);
    assert_eq!(value(&records, &checkout, "second"), "");
    assert_eq!(value(&records, &checkout, "first"), "abc");
    let entry = restore(&binding, &mut outer, crate::undo::UndoRequest::Undo);
    // The burst's newest change records how to take it back and bring it
    // back; undo takes the whole burst back.
    let step = entry.step.unwrap();
    let inverse: TextEdit = serde_json::from_slice(&step.compensation.unwrap().payload).unwrap();
    let redo: TextEdit = serde_json::from_slice(&step.action.unwrap().payload).unwrap();
    assert_eq!(inverse.value, "ab");
    assert_eq!(redo.value, "abc");
    assert_eq!(value(&records, &checkout, "first"), "");
    restore(&binding, &mut outer, crate::undo::UndoRequest::Redo);
    assert_eq!(value(&records, &checkout, "first"), "abc");
    restore(&binding, &mut outer, crate::undo::UndoRequest::Redo);
    assert_eq!(value(&records, &checkout, "second"), "xy");
}

#[test]
fn native_text_does_not_cross_collaborator_instance_or_programmatic_edits() {
    for author in ["You", "Collaborator"] {
        let records = crate::record::RecordOwners::portable(
            crate::portable_storage::PortableStorage::new("burst", "device").unwrap(),
        )
        .open("so.tilo.test", "room")
        .unwrap();
        let checkout = RecordCheckout::new(
            crate::record::RecordId::minted(),
            crate::record::RecordKind::new("text"),
        );
        records.create_record(&checkout).unwrap();
        let (one, _one_scope, _one_owner) =
            super::author_writer_tests::mount(&records, &checkout, "You", 1, true);
        let (two, _two_scope, _two_owner) =
            super::author_writer_tests::mount(&records, &checkout, author, 2, true);
        let mut outer = AutoCommit::new();
        let mut wire = Wire::default();
        for text in ["a", "ab"] {
            edit(&one, &mut outer, "first", text, true, &mut wire);
        }
        edit(&two, &mut outer, "other", "foreign", true, &mut wire);
        edit(&one, &mut outer, "first", "abc", true, &mut wire);
        restore(&one, &mut outer, crate::undo::UndoRequest::Undo);
        assert_eq!(value(&records, &checkout, "first"), "ab");
        assert_eq!(value(&records, &checkout, "other"), "foreign");
        restore(&one, &mut outer, crate::undo::UndoRequest::Undo);
        assert_eq!(value(&records, &checkout, "first"), "");
        assert_eq!(value(&records, &checkout, "other"), "foreign");
        for text in ["1", "12", "123"] {
            edit(&one, &mut outer, "first", text, false, &mut wire);
        }
        restore(&one, &mut outer, crate::undo::UndoRequest::Undo);
        assert_eq!(
            value(&records, &checkout, "first"),
            "12",
            "an agent/programmatic call is one step"
        );
        restore(&one, &mut outer, crate::undo::UndoRequest::Undo);
        assert_eq!(value(&records, &checkout, "first"), "1");
    }
}

#[test]
fn physical_text_provenance_is_captured_in_the_exact_owned_queue_entry() {
    let records = crate::record::RecordOwners::portable(
        crate::portable_storage::PortableStorage::new("burst", "device").unwrap(),
    )
    .open("so.tilo.test", "room")
    .unwrap();
    let checkout = RecordCheckout::new(
        crate::record::RecordId::minted(),
        crate::record::RecordKind::new("text"),
    );
    records.create_record(&checkout).unwrap();
    let (binding, _scope, _owner) =
        super::author_writer_tests::mount(&records, &checkout, "You", 1, true);
    let domain = crate::ControlDomain::new();
    domain.set_owner(crate::control_custody::ControlOwner::Component(binding));
    let mut emitter =
        crate::widget::ActionEmitter::new_in(crate::widget::test_string_action_binding(), &domain);
    with_physical_text(true, || emitter.emit("same".into())).unwrap();
    emitter.emit("same".into()).unwrap();
    let queued = domain.drain_partitioned().components;
    let mut queued = queued.into_iter();
    let (physical, first) = queued.next().unwrap();
    let (programmatic, second) = queued.next().unwrap();
    assert_eq!(physical.input, Input::Continuing);
    assert_eq!(programmatic.input, Input::Discrete);
    assert_eq!(
        first, second,
        "equal descriptor values retain distinct provenance"
    );
    assert!(queued.next().is_none());
    assert!(!physical_text(), "the scoped producer flag cannot leak");
}

#[test]
fn physical_text_provenance_survives_initial_custody_transfer_and_legacy_draining() {
    let mut initial =
        crate::widget::ActionEmitter::new(crate::widget::test_string_action_binding());
    with_physical_text(true, || initial.emit("same".into())).unwrap();
    initial.emit("same".into()).unwrap();
    let records = crate::record::RecordOwners::portable(
        crate::portable_storage::PortableStorage::new("burst", "device").unwrap(),
    )
    .open("so.tilo.test", "room")
    .unwrap();
    let checkout = RecordCheckout::new(
        crate::record::RecordId::minted(),
        crate::record::RecordKind::new("text"),
    );
    records.create_record(&checkout).unwrap();
    let (binding, _scope, _owner) =
        super::author_writer_tests::mount(&records, &checkout, "You", 1, true);
    let domain = crate::ControlDomain::new();
    domain.set_owner(crate::control_custody::ControlOwner::Component(binding));
    initial
        .install_custody(&domain, crate::control_custody::CustodyAttachment::Initial)
        .unwrap();
    let mut queued = domain.drain_partitioned().components.into_iter();
    let (physical, first) = queued.next().unwrap();
    let (programmatic, second) = queued.next().unwrap();
    assert!(
        physical.input == Input::Continuing,
        "initial installation retains the producer's exact queued provenance"
    );
    assert_eq!(programmatic.input, Input::Discrete);
    assert_eq!(first, second);
    assert!(queued.next().is_none());
    assert!(
        initial.take().is_none(),
        "registered holders cannot leak descriptors through legacy taking"
    );
    let mut legacy = crate::widget::ActionEmitter::new(crate::widget::test_string_action_binding());
    with_physical_text(true, || legacy.emit("same".into())).unwrap();
    legacy.emit("same".into()).unwrap();
    let mut parent = crate::widget::Column::new().push(Box::new(TextOwner {
        emitter: Some(legacy),
    }));
    let (physical, marked) = capture_physical_drain(|| {
        crate::widget::controls::take_attributed_control_invocation(&mut parent)
    });
    let (programmatic, unmarked) = capture_physical_drain(|| {
        crate::widget::controls::take_attributed_control_invocation(&mut parent)
    });
    assert_eq!(marked, Input::Continuing);
    assert_eq!(unmarked, Input::Discrete);
    assert_eq!(physical, programmatic);
    assert_eq!(DRAINED_INPUT.with(Cell::get), Input::Discrete);
}
