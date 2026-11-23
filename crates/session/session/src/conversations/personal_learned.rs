use tilo_resource::{Invoke, Replica};
use ui::resource::DocumentId;

use super::{CHAT_OWNER, Learned};

/// Keep the workspace where a fact was learned as context in the person's own workspace.
pub(super) fn record_id(workspace: &str) -> DocumentId {
    DocumentId::derived(
        &format!("chat.learned:{workspace}"),
        &DocumentId::for_tab(0),
    )
}

fn open(bytes: &[u8]) -> Result<Replica<Learned>, String> {
    let mut doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<Learned>(&mut doc).map_err(|error| error.to_string())?;
    Ok(Replica::from_doc(doc))
}

/// Merge `bytes` into the document `(CHAT_OWNER, id)` of the person's own workspace.
fn keep(store: &crate::person_resources::PersonResources, id: &DocumentId, bytes: Vec<u8>) {
    store.edit_document((CHAT_OWNER, id.as_str()), move |document| {
        let mut source = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        tilo_automerge::ApplyIncoming::merge_incoming(document, &mut source)
            .map_err(|error| error.to_string())?;
        Ok(())
    });
}

pub(super) fn change(
    store: &crate::person_resources::PersonResources,
    id: &DocumentId,
    agent: &str,
    earlier: Option<&[u8]>,
    invoke: Invoke<Learned>,
) -> Result<(), String> {
    let held = store.document((CHAT_OWNER, id.as_str()));
    let mut record = held
        .as_deref()
        .or(earlier)
        .map_or_else(|| Ok(Replica::new()), open)?
        .as_acting(tilo_actions::Acting::agent(agent))
        .in_app(CHAT_OWNER, "Chat");
    let now_ms = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX);
    record.set_now_ms(now_ms);
    record.apply(invoke).map_err(|error| error.to_string())?;
    keep(store, id, record.save());
    Ok(())
}
