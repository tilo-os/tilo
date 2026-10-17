use super::*;
use ui_core::automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

thread_local! {
    static ENCODINGS: Cell<usize> = const { Cell::new(0) };
}

struct Document;

#[derive(Clone, Debug, serde::Deserialize)]
struct Put(u64);

impl serde::Serialize for Put {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ENCODINGS.with(|count| count.set(count.get() + 1));
        serializer.serialize_u64(self.0)
    }
}

impl tilo_actions::Action for Put {
    fn apply(&self, doc: &mut tilo_resource::Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "value", self.0)?;
        Ok(())
    }
}

// Explicitly test-only for the source-scanning resource catalogue.
#[cfg(test)]
impl Resource for Document {
    const KIND: &'static str = "test.dispatch-encoding";
    const LABEL: &'static str = "Document";
    type State = ();
    type Action = Put;

    fn read(_: &tilo_resource::Doc) -> Result<(), tilo_resource::ModelError> {
        Ok(())
    }
}

#[test]
fn standalone_immediate_dispatch_has_no_capture_encoding() {
    let _nonlocal = ui_core::local_document_action::suspend();
    let dispatcher = Dispatcher::<Document>::new("test.dispatch", "body", false, Some("author"));
    let mut doc = AutoCommit::new();
    ENCODINGS.with(|count| count.set(0));
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &Put(7))
        .unwrap();
    assert_eq!(ENCODINGS.with(Cell::get), 1, "only the executor encodes");
    assert_eq!(doc.get(ROOT, "value").unwrap().unwrap().0.to_u64(), Some(7));
    assert!(!ui_core::local_document_action::has_invocation());

    let queue = ui_core::local_document_action::Queue::new();
    let _input = queue.enter("published-revision".into());
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &Put(8))
        .unwrap();
    let captures = queue.drain();
    assert_eq!(captures.len(), 1);
    let invocation = captures[0].published.as_ref().unwrap();
    assert_eq!(invocation.resource, "body");
    assert_eq!(invocation.action, tilo_resource::APPLY);
    assert_eq!(captures[0].mutation_hashes.len(), 1);
    assert!(queue.take_refusals().is_empty());
}
