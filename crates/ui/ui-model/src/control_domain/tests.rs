use std::cell::RefCell;
use std::rc::Rc;
use tilo_actions::ControlInvocation;
use tilo_ui_base::control_domain::*;
use tilo_ui_base::{widget::ActionEmitter, widget::Widget};
fn invocation(value: bool) -> ControlInvocation {
    tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)
        .invoke(value)
        .unwrap()
}
fn unit_binding() -> tilo_actions::ActionBinding<()> {
    let spec = tilo_actions::ActionSpec::typed_operation(
        "run",
        "Run",
        "Run",
        tilo_actions::OperationKind::Effect,
        "fixture.run",
        vec![],
        |_| tilo_actions::EncodedAction::encode_operation("fixture.run", &()).unwrap(),
    );
    let resource =
        tilo_actions::ResourceSpec::new("fixture", "fixture", "Fixture").with_action(spec.clone());
    tilo_actions::ActionBinding::new(&resource, &spec, |()| serde_json::Map::new()).unwrap()
}
fn raw_request(resource: &str, action: &str, marker: &str) -> ControlInvocation {
    let mut arguments = serde_json::Map::new();
    arguments.insert("marker".into(), serde_json::Value::String(marker.into()));
    ControlInvocation {
        address: tilo_actions::ActionAddress::new(resource, action),
        arguments,
    }
}
tilo_test_env::scenario! {
    fn a_raw_control_holder_transfers_once_and_refuses_foreign_ownership;

    phase control_ownership_raw_holder_keeps_one_queue_replace_one_and_clears;
    {
        let mut slot = RawControlSlot::replace_one();
        let expected_b = raw_request("resource-b", "apply", "copied-b");

        assert!(matches!(
            slot.emit(raw_request("resource-a", "apply", "copied-a")),
            Ok(LocalEmission::Queued(_))
        ));
        assert!(matches!(
            slot.emit(expected_b.clone()),
            Ok(LocalEmission::Queued(_))
        ));
        assert_eq!(slot.controlled_pending_for_test(), Some(expected_b));

        slot.emit(raw_request("resource-c", "clear", "copied-c"))
            .unwrap();
        slot.clear();
        assert_eq!(slot.controlled_pending_for_test(), None);
    }

    phase control_ownership_raw_holder_initial_transfer_is_once_and_registered_never_takes;
    {
        let domain = ControlDomain::outer();
        let expected = raw_request("initial", "apply", "before-install");
        let mut slot = RawControlSlot::replace_one();
        slot.emit(expected.clone()).unwrap();

        slot.attach(&domain, DomainAttachment::Initial)
            .unwrap();
        slot.attach(&domain, DomainAttachment::Dynamic)
            .unwrap();
        let drained = domain.drain_live();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].invocation, expected);
        assert!(
            domain.drain_live().is_empty(),
            "the adopted queue drains once"
        );
    }

    phase control_ownership_raw_holder_suspension_closed_foreign_and_late_refuse;
    {
        let domain = ControlDomain::outer();
        let mut slot = RawControlSlot::replace_one_in(&domain);
        let (lease, saved) = domain.suspend();
        assert!(saved.is_empty());
        assert_eq!(
            slot.emit(raw_request("hidden", "apply", "suppressed")),
            Ok(LocalEmission::Suppressed)
        );
        drop(lease);
        assert!(domain.drain_live().is_empty());

        let foreign = ControlDomain::outer();
        assert_eq!(
            slot.attach(&foreign, DomainAttachment::Initial),
            Err(AttachError::ForeignOwner)
        );
        assert_eq!(foreign.coverage().unsafe_late_attachments, 1);

        foreign.finish_initial_installation();
        let mut late = RawControlSlot::replace_one();
        late.emit(raw_request("late", "apply", "retained")).unwrap();
        assert_eq!(
            late.attach(&foreign, DomainAttachment::Dynamic),
            Err(AttachError::UnsafeLateAttachment)
        );
        assert_eq!(foreign.coverage().unsafe_late_attachments, 2);

        domain.close();
        assert_eq!(
            slot.emit(raw_request("closed", "apply", "refused")),
            Err(EmitError::Closed)
        );
        assert_eq!(
            slot.attach(&domain, DomainAttachment::Initial),
            Err(AttachError::Closed)
        );
    }

    phase control_ownership_raw_holder_exact_renewal_transfers_no_old_output_and_accepts_fresh;
    {
        let previous = ControlDomain::outer();
        let mut slot = RawControlSlot::replace_one_in(&previous);
        slot.emit(raw_request("old", "apply", "must-not-transfer"))
            .unwrap();
        previous.close();

        let replacement = ControlDomain::outer();
        let renewal = replacement.renewal_from(&previous);
        slot.attach(&replacement, renewal).unwrap();
        assert!(replacement.drain_live().is_empty());
        assert!(matches!(
            slot.emit(raw_request("fresh", "apply", "fresh-only")),
            Ok(LocalEmission::Queued(_))
        ));
        let drained = replacement.drain_live();
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].invocation,
            raw_request("fresh", "apply", "fresh-only")
        );

        let wrong_replacement = ControlDomain::outer();
        assert_eq!(
            slot.attach(&wrong_replacement, renewal),
            Err(AttachError::ForeignOwner)
        );
        assert_eq!(wrong_replacement.coverage().unsafe_late_attachments, 1);
    }
}
tilo_test_env::scenario! {
    fn control_ownership_registers_orders_and_suspends_its_producers;

    phase control_ownership_core_registration_is_single_ownership_and_late_attach_is_unqualified;
    {
        let domain = ControlDomain::outer();
        let mut initial = ActionEmitter::new(tilo_ui::Bind::bound(tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)));
        initial.emit(true).unwrap();
        initial
            .attach(&domain, DomainAttachment::Initial)
            .unwrap();
        assert!(initial.take().is_none());
        let (lease, saved) = domain.suspend();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].invocation, invocation(true));
        initial.emit(false).unwrap();
        initial.refresh_availability(&Default::default());
        assert_eq!(saved[0].invocation, invocation(true));
        drop(lease);
        assert!(domain.drain_live().is_empty());
        let mut late = ActionEmitter::new(tilo_ui::Bind::bound(tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)));
        late.emit(false).unwrap();
        assert!(late.take().is_some());
        assert_eq!(
            late.attach(&domain, DomainAttachment::Dynamic),
            Err(AttachError::UnsafeLateAttachment)
        );
        assert_eq!(domain.coverage().unsafe_late_attachments, 1);
        domain.finish_initial_installation();
        let mut falsely_initial = ActionEmitter::new(tilo_ui::Bind::bound(tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)));
        falsely_initial.emit(true).unwrap();
        assert_eq!(
            falsely_initial.attach(&domain, DomainAttachment::Initial),
            Err(AttachError::UnsafeLateAttachment)
        );
        let foreign = ControlDomain::outer();
        assert_eq!(
            initial.attach(&foreign, DomainAttachment::Initial),
            Err(AttachError::ForeignOwner)
        );
        domain.close();
        assert_eq!(
            initial.attach(&domain, DomainAttachment::Initial),
            Err(AttachError::Closed)
        );
        initial.emit(true).unwrap();
        assert!(initial.take().is_none());
        let mut closed = ActionEmitter::new_in(tilo_ui::Bind::bound(tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)), &domain);
        closed.emit(true).unwrap();
        assert!(closed.take().is_none());
    }

    phase control_ownership_core_nested_leases_close_and_replacement;
    {
        for outer_first in [false, true] {
            let outer = ControlDomain::outer();
            let child = outer.child();
            let outbox = child.producer(ControlProducerDiscipline::Fifo { limit: 64 });
            outbox.emit(invocation(true)).unwrap();
            let (a, saved) = outer.suspend();
            let (b, duplicate) = child.suspend();
            assert_eq!(saved.len(), 1);
            assert!(duplicate.is_empty());
            let remaining = if outer_first {
                drop(a);
                b
            } else {
                drop(b);
                a
            };
            assert_eq!(
                outbox.emit(invocation(false)),
                Ok(LocalEmission::Suppressed)
            );
            drop(remaining);
            assert!(matches!(
                outbox.emit(invocation(false)),
                Ok(LocalEmission::Queued(_))
            ));
            assert_eq!(outer.drain_live().len(), 1);
            child.close();
            assert_eq!(outbox.emit(invocation(true)), Err(EmitError::Closed));
            let replacement = outer.child();
            let fresh = replacement.producer(ControlProducerDiscipline::ReplaceOne);
            fresh.emit(invocation(false)).unwrap();
            assert_eq!(outer.drain_live().len(), 1);
            outer.close();
            let illegal_child = outer.child();
            assert_eq!(
                illegal_child
                    .producer(ControlProducerDiscipline::ReplaceOne)
                    .emit(invocation(true)),
                Err(EmitError::Closed)
            );
        }
    }

    phase control_ownership_core_a_control_without_a_host_keeps_its_own_queue;
    {
        let mut control = ActionEmitter::new(tilo_ui::Bind::bound(tilo_ui::widget::test_value_action_binding::<bool>(tilo_actions::ParamType::Boolean)));
        control.emit(true).unwrap();
        assert_eq!(control.take(), Some(invocation(true)));
        assert!(control.take().is_none());
        assert_eq!(
            ControlDomain::outer().coverage(),
            ControlCoverage::default()
        );
    }

    phase control_ownership_core_order_keeps_interleaved_subtrees_and_replace_one;
    {
        let outer = ControlDomain::outer();
        let a = outer.producer(ControlProducerDiscipline::Fifo { limit: 2 });
        let inner = outer.child();
        let b = inner.producer(ControlProducerDiscipline::ReplaceOne);
        let c = outer.producer(ControlProducerDiscipline::Fifo { limit: 2 });
        for (box_, label) in [(&a, "A"), (&b, "old B"), (&b, "B"), (&c, "C")] {
            let mut request = invocation(true);
            request.address.resource = label.into();
            box_.emit(request).unwrap();
        }
        let (_lease, saved) = outer.suspend();
        assert_eq!(
            saved
                .iter()
                .map(|request| request.invocation.address.resource.as_str())
                .collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
        let mut items = crate::history_mode::pending_controls::SuspendedLocalControls::default();
        items.begin_history_entry();
        for item in saved {
            items.capture_registered(item, Err("ordering-only observation".into()));
        }
        items.return_to_live();
        let first = items.review();
        items.begin_history_entry();
        items.return_to_live();
        assert_eq!(
            items.review(),
            first,
            "sealed descriptors keep their order across reentry/return"
        );
    }

    phase control_ownership_core_dynamic_modelview_inherits_suspension_before_emit;
    {
        let domain = ControlDomain::outer();
        let retained = Rc::new(RefCell::new(None));
        let output = retained.clone();
        let mut view = tilo_ui::widget::ModelView::from_document_in(
            &domain,
            |_, _| 1,
            move |_, domain| {
                let producer = domain.producer(ControlProducerDiscipline::ReplaceOne);
                assert_eq!(
                    producer.emit(invocation(true)),
                    Ok(LocalEmission::Suppressed)
                );
                *output.borrow_mut() = Some(producer);
                Box::new(tilo_ui::widget::Button::new("passive", "Passive").on_action(unit_binding()))
            },
        );
        tilo_ui_base::widget::controls::attach_controls(
            &mut view,
            &domain,
            DomainAttachment::Initial,
        );
        let (lease, saved) = domain.suspend();
        assert!(saved.is_empty());
        let mut doc = automerge::AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        drop(lease);
        assert!(domain.drain_live().is_empty());
        assert!(view.children_mut()[0].perform_action(
            &tilo_ui_base::a11y::Action::Activate,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        let requests = domain.drain_live();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].invocation.arguments.is_empty());
        assert!(matches!(
            retained.borrow().as_ref().unwrap().emit(invocation(false)),
            Ok(LocalEmission::Queued(_))
        ));
        assert_eq!(domain.drain_live().len(), 1);
    }
}
