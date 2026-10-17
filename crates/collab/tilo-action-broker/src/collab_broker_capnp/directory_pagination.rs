//! Broker-owned continuation tokens never expose a source's offsets or ids.
use super::{BrokerActor, Rc, RefCell};
use std::collections::HashMap;

#[derive(Clone, Default)]
pub(super) struct DirectoryPagination(Rc<RefCell<HashMap<Vec<u8>, Continuation>>>);

struct Continuation {
    actor: String,
    revision: String,
    needle: String,
    parent: String,
    source: Vec<u8>,
}

impl DirectoryPagination {
    pub(super) fn source_cursor(
        &self,
        actor: &BrokerActor,
        revision: &str,
        needle: &str,
        parent: &str,
        token: &[u8],
    ) -> Result<Vec<u8>, capnp::Error> {
        if token.is_empty() {
            return Ok(Vec::new());
        }
        self.0
            .borrow()
            .get(token)
            .filter(|entry| {
                entry.actor == actor.id
                    && entry.revision == revision
                    && entry.needle == needle
                    && entry.parent == parent
            })
            .map(|entry| entry.source.clone())
            .ok_or_else(|| capnp::Error::failed("unknown directory continuation".into()))
    }

    pub(super) fn shield(
        &self,
        actor: &BrokerActor,
        needle: &str,
        parent: &str,
        page: &mut runtime_collab::ActionDirectoryPage,
    ) {
        if page.next_cursor.is_empty() {
            return;
        }
        let mut entries = self.0.borrow_mut();
        // Bound retained source metadata, even when a caller never continues.
        if entries.len() >= 256 {
            entries.clear();
        }
        let token = uuid::Uuid::new_v4().as_bytes().to_vec();
        let source = std::mem::replace(&mut page.next_cursor, token.clone());
        entries.insert(
            token,
            Continuation {
                actor: actor.id.clone(),
                revision: page.revision.clone(),
                needle: needle.to_owned(),
                parent: parent.to_owned(),
                source,
            },
        );
    }
}
