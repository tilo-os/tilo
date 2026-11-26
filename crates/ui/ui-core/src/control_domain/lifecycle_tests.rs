use tilo_ui_base::control_domain::*;
use tilo_ui_base::widget::Widget;
use tilo_actions::ControlInvocation;
use std::rc::Rc;
use std::cell::RefCell;
fn request(label: &str) -> ControlInvocation {
    ControlInvocation {
        address: tilo_actions::ActionAddress::new(label, "run"),
        arguments: serde_json::Map::new(),
    }
}
tilo_test_env::scenario! {
    fn control_ownership_survives_replacement_and_covering_leases;

    phase control_ownership_core_actual_modelview_replacement_preserves_installed_order;
    {
        let domain = ControlDomain::outer();
        let key = Rc::new(RefCell::new(0));
        let projection = key.clone();
        let a = domain.producer(ControlProducerDiscipline::ReplaceOne);
        let mut model = crate::widget::ModelView::from_document_in(
            &domain,
            move |_, _| *projection.borrow(),
            |key, domain| {
                let outbox = domain.producer(ControlProducerDiscipline::ReplaceOne);
                outbox
                    .emit(request(if *key == 0 { "B0" } else { "B1" }))
                    .unwrap();
                Box::new(crate::widget::Text::new("passive"))
            },
        );
        tilo_ui_base::widget::controls::attach_controls(
            &mut model,
            &domain,
            DomainAttachment::Initial,
        );
        let c = domain.producer(ControlProducerDiscipline::ReplaceOne);
        let doc = automerge::AutoCommit::new();
        for revision in 0..128 {
            let want = if revision == 0 { "B0" } else { "B1" };
            *key.borrow_mut() = revision;
            model.sync(&doc, &automerge::ROOT);
            let scope = domain.children().into_iter().next().unwrap();
            assert_eq!(domain.0.borrow().order.len(), 3, "stable A/subtree/C slots");
            assert_eq!(
                scope.0.borrow().order.len(),
                1,
                "closed generations unlink from installed scope"
            );
            assert_eq!(scope.children().len(), 1);
            a.emit(request("A")).unwrap();
            c.emit(request("C")).unwrap();
            let (lease, saved) = domain.suspend();
            assert_eq!(
                saved
                    .iter()
                    .map(|item| item.invocation.address.resource.as_str())
                    .collect::<Vec<_>>(),
                ["A", want, "C"]
            );
            drop(lease);
        }
    }

    phase control_ownership_core_continuation_waits_for_every_covering_surface;
    {
        let root = ControlDomain::outer();
        let child = root.child();
        let producer = child.producer(ControlProducerDiscipline::ReplaceOne);
        producer.emit(request("saved")).unwrap();
        let (inner, saved) = child.suspend();
        let (outer, duplicate) = root.suspend();
        assert!(duplicate.is_empty());
        let mut pending = crate::history_mode::pending_controls::SuspendedLocalControls::default();
        pending.begin_history_entry();
        for item in saved {
            pending.capture_registered(
                item,
                Err("fixture observation is intentionally not authority".into()),
            );
        }
        drop(inner);
        pending.return_to_live();
        let id = pending.review()[0].id;
        let token = pending.token(id).unwrap();
        assert!(
            pending.prepare(&token).is_none(),
            "outer covering lease blocks saved Continue too"
        );
        drop(outer);
        assert!(
            pending.prepare(&token).is_some(),
            "blocked token was not consumed"
        );
    }
}
