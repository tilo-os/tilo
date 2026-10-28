//! The content a record references: its lanes and stores are the collab
//! client's (`collaboration::record_content`); what a view reads of it, as
//! arrivals, is the toolkit's.

pub use ::collaboration::record_content::*;
use std::collections::HashMap;
use tilo_content::{ContentState, ContentStore};

/// Content a record references that cannot arrive: every device that
/// could hand it over said it keeps none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentUnavailable;

impl std::fmt::Display for ContentUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::I18nManager::lookup_plain(
            "content-unavailable",
        ))
    }
}

/// The content a view shows, each address a read
/// ([`crate::arriving::Arriving`]): ready once its bytes are on this device
/// (read them from the store), pending while it is on its way from the
/// room ([`crate::room_sync::source`]), failed when no device has it.
/// Content this device has is ready on the first read, so a view of it
/// opens complete.
#[derive(Clone)]
pub struct ContentReads {
    store: ContentStore,
    reads: std::rc::Rc<
        std::cell::RefCell<HashMap<String, crate::arriving::Arriving<(), ContentUnavailable>>>,
    >,
}

impl ContentReads {
    /// Reads of the content in `store`, the one the window's host lent.
    #[must_use]
    pub fn new(store: ContentStore) -> Self {
        Self {
            store,
            reads: std::rc::Rc::default(),
        }
    }

    /// The content at `hash`, as a read: the first read of content this
    /// device lacks asks the room for it. Each call brings the read to
    /// where the content stands, so a view that reads it as it draws
    /// follows it.
    #[must_use]
    pub fn read(&self, hash: &str) -> crate::arriving::Arriving<(), ContentUnavailable> {
        let arriving = self
            .reads
            .borrow_mut()
            .entry(hash.to_owned())
            .or_insert_with(|| {
                if !self.store.has(hash) {
                    // Opening content a record references and this device
                    // lacks asks the record's route for it.
                    let _ = self.store.open(hash);
                }
                crate::arriving::Arriving::pending(crate::arriving::Source::Local)
            })
            .clone();
        let now = content_standing(self.store.state(hash));
        if arriving.with(|was| *was != now) {
            arriving.stand(now);
        }
        arriving
    }

    /// The store the content is read from.
    #[must_use]
    pub fn store(&self) -> &ContentStore {
        &self.store
    }
}

/// Where content that stands at `state` has got to, as an arrival.
fn content_standing(state: ContentState) -> crate::arriving::Arrival<(), ContentUnavailable> {
    use crate::arriving::Arrival;
    match state {
        ContentState::Resolved => Arrival::Ready(()),
        ContentState::Pending | ContentState::Waiting => Arrival::Pending {
            from: crate::room_sync::source(),
        },
        ContentState::Unavailable => Arrival::Failed(ContentUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn record_store(record: &str) -> (ContentStore, Arc<ContentLane>) {
        let lane = ContentLane::of("so.tilo.record-content-test", record);
        (
            RecordContent::store(ContentStore::memory(), Arc::clone(&lane)),
            lane,
        )
    }

    #[test]
    fn content_reads_arrive_from_the_room_and_content_here_is_ready_at_once() {
        use crate::arriving::Arrival;
        let (store, lane) = record_store("read-as-arriving");
        let here = store.put(b"a picture here").unwrap();
        let reads = ContentReads::new(store);
        assert_eq!(
            reads.read(&here).get(),
            Arrival::Ready(()),
            "content on this device opens complete"
        );

        let bytes = b"bytes on another device".to_vec();
        let away = tilo_content::hash_bytes(&bytes);
        let read = reads.read(&away);
        assert!(read.get().is_pending(), "on its way from the room");
        assert!(
            lane.take_requests().wants.contains(&away),
            "the first read asks the room for it"
        );
        lane.stands(away.clone(), ContentState::Unavailable);
        assert_eq!(reads.read(&away).get(), Arrival::Failed(ContentUnavailable));
        lane.stands(away.clone(), ContentState::Waiting);
        assert!(reads.read(&away).get().is_pending());
        lane.arrive(away.clone(), bytes);
        assert_eq!(reads.read(&away).get(), Arrival::Ready(()));
        assert_eq!(read.get(), Arrival::Ready(()), "the one read answers");
    }
}
