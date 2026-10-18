use super::*;
use automerge::{ROOT, transaction::Transactable};

#[derive(Debug, serde::Serialize)]
struct Put(u64);
impl tilo_actions::Action for Put {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "input", self.0)?;
        Ok(())
    }
}

#[test]
fn route_snapshot_aliases_are_frozen_before_typed_execution_and_suspend_restores_them() {
    let queue = Queue::new();
    let aliases = Rc::new(std::collections::HashMap::from([(
        "body".into(),
        "published-body".into(),
    )]));
    let pin = Rc::new(String::from("inert snapshot ownership"));
    let weak_pin = Rc::downgrade(&pin);
    let input = enter_input(vec![InputRoute {
        custody: None,
        owner: None,
        queue: queue.clone(),
        revision: "published-before-input".into(),
        aliases,
        pin: Some(pin),
    }]);
    let invocation = with_invocation(PublishedInvocation {
        resource: "body".into(),
        action: "insert".into(),
        arguments: serde_json::Map::new(),
    });
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    host.try_dispatch(&mut doc, "put", &Put(1)).unwrap();
    {
        let _background = suspend();
        host.try_dispatch(&mut doc, "put", &Put(2)).unwrap();
    }
    host.try_dispatch(&mut doc, "put", &Put(3)).unwrap();
    drop(invocation);
    drop(input);
    host.try_dispatch(&mut doc, "put", &Put(4)).unwrap();
    assert!(
        weak_pin.upgrade().is_some(),
        "queued captures own the pre-mutation pin"
    );
    let captures = queue.drain_leased();
    assert_eq!(captures.len(), 2);
    assert!(
        weak_pin.upgrade().is_some(),
        "in-flight reports still own the pin"
    );
    for (capture, _lease) in captures {
        assert_eq!(capture.revision, "published-before-input");
        assert_eq!(capture.published.unwrap().resource, "published-body");
        assert!(!capture.mutation_hashes.is_empty());
    }
    assert!(
        weak_pin.upgrade().is_none(),
        "definitive replies release the pin"
    );
}
