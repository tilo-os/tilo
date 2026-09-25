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
        let mut progressed = false;
        if let Some(message) = a_to_b.generate(a) {
            b_to_a.receive(b, &message);
            progressed = true;
        }
        if let Some(message) = b_to_a.generate(b) {
            a_to_b.receive(a, &message);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::converge;
    use automerge::AutoCommit;

    use crate::{doc, seed};

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        let library = seed::sample_library();
        doc::seed(&mut doc, &library, &seed::sample_playback(&library));
        doc
    }

    #[test]
    fn platform_document_handshake_converges_library_edits_both_ways() {
        let mut laptop = seeded();
        let mut phone = AutoCommit::new();
        converge(&mut laptop, &mut phone);

        doc::set_liked(&mut laptop, "tk_v2", true);
        doc::create_playlist(&mut phone, "Phone picks");
        converge(&mut laptop, &mut phone);

        let laptop_library = doc::load_library(&laptop);
        let phone_library = doc::load_library(&phone);
        assert!(laptop_library.is_liked("tk_v2"));
        assert!(phone_library.is_liked("tk_v2"));
        assert!(
            laptop_library
                .playlists
                .iter()
                .any(|playlist| playlist.name == "Phone picks")
        );
        assert!(
            phone_library
                .playlists
                .iter()
                .any(|playlist| playlist.name == "Phone picks")
        );
    }
}
