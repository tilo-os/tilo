use super::*;
use crate::history_source::authors::{AuthorDirectory, actor_handle};
use crate::widget::history::HistoryAuthor;
use automerge::transaction::Transactable;
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, EncodedAction, ExecutionLocus, LocalTransport};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Write {
    text: String,
}
impl Action for Write {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), ActionError> {
        doc.put(ROOT, "body", self.text.as_str())?;
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// One person typing on two devices at once: each device's change is its
/// own actor's, so the replicas merge, and history still names the person.
#[test]
fn one_author_typing_on_two_devices_writes_changes_that_merge() {
    let mut base = AutoCommit::new().with_actor(automerge::ActorId::from(b"genesis".as_slice()));
    base.put(ROOT, "body", "").unwrap();
    base.commit();
    let mut laptop = base
        .fork()
        .with_actor(automerge::ActorId::from(b"laptop".as_slice()));
    let mut browser = base
        .fork()
        .with_actor(automerge::ActorId::from(b"browser".as_slice()));
    for (doc, text) in [
        (&mut laptop, "from the laptop"),
        (&mut browser, "from the browser"),
    ] {
        let action = Write { text: text.into() };
        let encoded = EncodedAction::encode("test.write", &action).unwrap();
        execute_document(
            doc,
            &action,
            encoded,
            &mut LocalTransport::default(),
            "Ada",
            false,
            tilo_actions::ActionContext::default(),
        )
        .unwrap();
    }
    laptop
        .merge(&mut browser)
        .expect("the two devices' changes merge");

    let actors: Vec<String> = laptop
        .get_changes(&base.get_heads())
        .iter()
        .map(|change| change.actor_id().to_hex_string())
        .collect();
    assert_eq!(actors.len(), 2);
    assert_ne!(actors[0], actors[1]);
    let mut directory = AuthorDirectory::new();
    directory.register(
        &automerge::ActorId::from(b"Ada".as_slice()).to_hex_string(),
        HistoryAuthor::new("Ada Lovelace", 10.0, "416461"),
    );
    for actor in &actors {
        assert_eq!(actor_handle(actor).as_deref(), Some("Ada"));
        assert_eq!(directory.resolve(actor).name, "Ada Lovelace");
    }
}
