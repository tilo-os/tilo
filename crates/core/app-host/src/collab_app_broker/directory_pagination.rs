//! Broker-owned continuation tokens never expose a source's offsets or ids.
use super::{BrokerActor, Rc, RefCell};
use std::collections::HashMap;

/// The bytes every continuation this host issues starts with: one that does
/// not is no continuation of ours, and is refused as malformed rather than
/// looked up as one read before a change.
const TOKEN_PREFIX: &[u8] = b"continuation-v1:";

/// What a caller is told when its continuation names a page read before the
/// app changed its directory: the directory moved on, so the caller reads the
/// level again from its first page, and says that it did.
pub(crate) const STALE_CONTINUATION: &str = "the directory changed; read it from the start";

/// Why a continuation did not name a page.
#[derive(Debug)]
pub(super) enum CursorRefusal {
    /// Bytes no continuation this host issued carries.
    Malformed,
    /// A continuation this host issued, for a page read before the directory
    /// moved on: the app's own cursor is bound to the revision it read.
    ReadBeforeAChange,
}

#[derive(Clone, Default)]
pub(super) struct DirectoryPagination(Rc<RefCell<HashMap<Vec<u8>, Continuation>>>);

struct Continuation {
    actor: tilo_action_policy::Actor,
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
    ) -> Result<Vec<u8>, CursorRefusal> {
        if token.is_empty() {
            return Ok(Vec::new());
        }
        let Some(id) = token.strip_prefix(TOKEN_PREFIX) else {
            return Err(CursorRefusal::Malformed);
        };
        self.0
            .borrow()
            .get(id)
            .filter(|entry| {
                entry.actor == *actor.actor()
                    && entry.revision == revision
                    && entry.needle == needle
                    && entry.parent == parent
            })
            .map(|entry| entry.source.clone())
            .ok_or(CursorRefusal::ReadBeforeAChange)
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
        let id = uuid::Uuid::new_v4().as_bytes().to_vec();
        let mut token = TOKEN_PREFIX.to_vec();
        token.extend_from_slice(&id);
        let source = std::mem::replace(&mut page.next_cursor, token);
        entries.insert(
            id,
            Continuation {
                actor: actor.actor().clone(),
                revision: page.revision.clone(),
                needle: needle.to_owned(),
                parent: parent.to_owned(),
                source,
            },
        );
    }
}
