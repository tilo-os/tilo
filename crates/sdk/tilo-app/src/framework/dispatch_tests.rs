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
fn framework_edits_finish_before_observation_drain_and_refuse_before_mutation() {
    let dispatcher = Dispatcher::<Document>::new("test.dispatch", "body", false, Some("author"));
    let mut doc = AutoCommit::new();
    let queue = ui_core::local_document_action::Queue::new();
    let _input = queue.enter("published-revision".into());
    // No drain, admission or host response occurs between these real SDK edits.
    let mut completed = 0;
    for value in 0..=128 {
        let heads = doc.get_heads();
        if dispatcher
            .apply_now(&mut Edits::lend(&mut doc), &Put(value))
            .is_err()
        {
            assert_eq!(doc.get_heads(), heads, "refusal precedes mutation");
            break;
        }
        completed += 1;
        assert_eq!(
            doc.get(ROOT, "value").unwrap().unwrap().0.as_u64(),
            Some(value)
        );
    }
    // Either the byte budget or the count budget may be reached first.
    assert!((2..=128).contains(&completed));
    assert_eq!(
        doc.get(ROOT, "value").unwrap().unwrap().0.as_u64(),
        Some(completed - 1)
    );
    assert_eq!(queue.take_refusals().len(), 1);
    let captures = queue.drain();
    assert_eq!(captures.len(), completed as usize);
    for capture in captures {
        assert_eq!(capture.mutation_hashes.len(), 1);
        assert_eq!(capture.published.unwrap().resource, "body");
    }
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &Put(129))
        .unwrap();
    assert_eq!(
        queue.drain().len(),
        1,
        "released capacity permits the next edit"
    );
}

#[test]
fn standalone_dispatch_before_after_timing_distribution() {
    let _nonlocal = ui_core::local_document_action::suspend();
    let mut before = Vec::new();
    let mut after = Vec::new();
    // Reproduce the removed eager Invoke construction, not an old binary.
    // Both cases still execute the real SDK dispatcher; timings are descriptive
    // only (run with --nocapture), never a machine-dependent correctness gate.
    for round in 0..4 {
        for eager in if round % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let dispatcher =
                Dispatcher::<Document>::new("test.dispatch", "body", false, Some("author"));
            let mut doc = AutoCommit::new();
            ENCODINGS.with(|count| count.set(0));
            for value in 0..128 {
                let action = Put(value);
                let start = std::time::Instant::now();
                if eager {
                    std::hint::black_box(Invoke::<Document>::apply(&action).unwrap());
                }
                dispatcher
                    .apply_now(&mut Edits::lend(&mut doc), &action)
                    .unwrap();
                let elapsed = start.elapsed().as_nanos();
                if eager {
                    before.push(elapsed);
                } else {
                    after.push(elapsed);
                }
            }
            assert_eq!(ENCODINGS.with(Cell::get), if eager { 256 } else { 128 });
            assert_eq!(
                doc.get(ROOT, "value").unwrap().unwrap().0.as_u64(),
                Some(127)
            );
        }
    }
    for (label, mut samples) in [
        ("before (eager Invoke emulation)", before),
        ("after", after),
    ] {
        samples.sort_unstable();
        eprintln!(
            "{label}: n={} ns/edit min={} p50={} p95={} max={}",
            samples.len(),
            samples[0],
            samples[samples.len() / 2],
            samples[samples.len() * 95 / 100],
            samples[samples.len() - 1]
        );
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
    assert_eq!(doc.get(ROOT, "value").unwrap().unwrap().0.as_u64(), Some(7));
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
