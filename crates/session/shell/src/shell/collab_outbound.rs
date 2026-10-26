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

    fn app_presence(&self, app_id: String, cluster_id: String, presence: PeerPresence) {
        self.publish_app_presence(app_id, cluster_id, presence);
    }

    fn scrollback_request(
        &self,
        cluster_id: &str,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        self.publish_scrollback_request(cluster_id, artifact, before_seq, max);
    }

    fn app_live_presence(
        &self,
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        presence: &PeerPresence,
    ) {
        self.publish_live_presence(app_id, cluster_id, record_id, presence);
    }

    fn local_route(&self, app_id: &str, cluster_id: &str, record_id: &str) {
        let _ = self.register_local_record(app_id, cluster_id, record_id);
        self.request_room_replay(cluster_id);
    }

    fn sealed_history_wanted(&self, _app_id: &str, cluster_id: &str, _record_id: &str) {
        self.request_sealed_history(cluster_id);
    }
}
