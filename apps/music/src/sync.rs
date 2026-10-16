//! Test-only Automerge convergence helpers.
//!
//! Music's collaborative library state is owned by the platform's canonical
//! app document and `ActionHost`. The platform supplies persistence, peer
//! replication, history, and collaboration; this application must not offer a
//! second pairing or relay protocol. These helpers exercise the exact
//! Automerge message handshake with an in-process peer in unit tests only.

#[cfg(test)]
use automerge::AutoCommit;
#[cfg(test)]
use automerge::sync::{State as SyncState, SyncDoc};
use tilo_automerge::ApplyIncoming;

/// One side of a test-only Automerge sync session.
#[cfg(test)]
#[derive(Default)]
struct SyncSession {
    state: SyncState,
}

#[cfg(test)]
impl SyncSession {
    fn generate(&mut self, doc: &mut AutoCommit) -> Option<Vec<u8>> {
        doc.sync()
            .generate_sync_message(&mut self.state)
            .map(automerge::sync::Message::encode)
    }

    fn receive(&mut self, doc: &mut AutoCommit, data: &[u8]) -> bool {
        match automerge::sync::Message::decode(data) {
            Ok(message) => {
                let _ = doc.receive_incoming_sync(&mut self.state, message);
                true
            }
            Err(_) => false,
        }
    }
}

/// Converge two platform-document replicas in a test without introducing a
/// product-owned transport. The production host provides this boundary.
#[cfg(test)]
pub(crate) fn converge(a: &mut AutoCommit, b: &mut AutoCommit) {
    // Flush unfinalised changes before advertising heads. A newly forked doc
    // otherwise can announce stale heads and stall the small-diff handshake.
    let _ = a.get_heads();
    let _ = b.get_heads();
    let mut a_to_b = SyncSession::default();
    let mut b_to_a = SyncSession::default();
    for _ in 0..200 {
        let forward = if let Some(message) = a_to_b.generate(a) {
            b_to_a.receive(b, &message);
            true
        } else {
            false
        };
        let backward = if let Some(message) = b_to_a.generate(b) {
            a_to_b.receive(a, &message);
            true
        } else {
            false
        };
        if !forward && !backward {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::converge;
    use automerge::AutoCommit;
    use ui::tilo_resource::{Invoke, Replica};

    use crate::model::Library;
    use crate::resource::library::CreatePlaylist;

    fn seeded() -> Replica<Library> {
        let mut library = Replica::<Library>::new();
        for action in crate::seed::sample_actions() {
            library.apply(action).unwrap();
        }
        library
    }

    #[test]
    fn platform_document_handshake_converges_library_edits_both_ways() {
        let mut laptop = seeded();
        let mut phone = Replica::<Library>::from_doc(AutoCommit::new());
        converge(laptop.doc_mut(), phone.doc_mut());

        laptop
            .apply(Invoke::edit(&Library::like("tk_v2", true)).unwrap())
            .unwrap();
        phone
            .apply(Library::CREATE_PLAYLIST.with(CreatePlaylist {
                name: "Phone picks".into(),
                track: None,
            }))
            .unwrap();
        converge(laptop.doc_mut(), phone.doc_mut());

        for library in [laptop.state().unwrap(), phone.state().unwrap()] {
            assert!(library.is_liked("tk_v2"));
            assert!(
                library
                    .playlists()
                    .any(|playlist| playlist.name == "Phone picks")
            );
        }
    }
}
