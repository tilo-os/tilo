//! What a component's pass reads of the resource store: each record it reads, followed until its
//! frontier moves.

use tilo_ui_base::{component_inputs::Key, component_inputs::Reads, component_inputs::fold, component_inputs::probe};
use crate::wake::WakeHandle;

/// Run `pass` as the pass of the component `owner` names, gathering what it reads outside the
/// component, the records it reads of the store among them.
pub(crate) fn reading<R>(owner: &WakeHandle, pass: impl FnOnce() -> R) -> (R, Reads) {
    // The store tells this pass which resources it read.
    super::observe_reads(read_resource);
    tilo_ui_base::component_inputs::reading(owner, pass)
}

/// A pass read `document`, record `id` of `store`: the pass reads it again once its frontier moved,
/// whoever wrote it.
#[inline(never)]
fn read_resource(
    store: &super::ResourceStore,
    id: &super::DocumentId,
    document: &automerge::AutoCommit,
) {
    let key = Key::Record(store.key(), fold(0, id.as_str().as_bytes()));
    probe(
        || key,
        || {
            let (store, id) = (store.clone(), id.clone());
            (
                Some(frontier(document)),
                Box::new(move || store.read(&id, frontier)),
            )
        },
    );
}

fn frontier(document: &automerge::AutoCommit) -> u64 {
    let (heads, pending) = tilo_ui_base::doc::settle_mark(document);
    heads
        .iter()
        .fold(pending as u64, |folded, head| fold(folded, &head.0))
}
