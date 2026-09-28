//! Native network adapter for the shared production publisher.
use super::network::ShellNetwork;
use collab_types::{ArtifactId, PeerPresence, SignedOp};
pub use tilo_action_broker::collab_outbound::*;
impl PeerPublishSink for ShellNetwork {
    fn app_document(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: SignedOp,
    ) {
        self.publish_document(app_id, cluster_id, &record_id, envelope);
    }

    fn app_document_covering(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: SignedOp,
        covers: Vec<[u8; 32]>,
    ) {
        self.publish_document_covering(app_id, cluster_id, &record_id, envelope, covers);
    }

    fn app_presence(&self, app_id: String, cluster_id: String, presence: PeerPresence) {
        self.publish_app_presence(app_id, cluster_id, presence);
    }

    fn scrollback_request(&self, artifact: ArtifactId, before_seq: u64, max: u16) {
        self.publish_scrollback_request(artifact, before_seq, max);
    }

    fn local_route(&self, app_id: &str, cluster_id: &str, record_id: &str) {
        let _ = self.register_local_record(app_id, cluster_id, record_id);
    }

    fn document_replay(&self, app_id: &str, cluster_id: &str, record_id: &str) {
        let _ = self.request_document_replay(app_id, cluster_id, record_id);
    }
}
