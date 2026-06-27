//! Collaboration pane — invite links and active peers per cluster.
//
// TODO(qs_actions): wire to `shell::qs_actions::collaboration::*`
//   - `active_peers() -> Vec<PeerInfo>` (id, display_name, cluster)
//   - `mint_invite(cluster_id, scope: InviteScope) -> Url`
//   - `revoke_invite(invite_id)`
//   - `kick_peer(peer_id)`
// Until that lands the pane is a `Coming soon` stub.

use ui::widget::Widget;

pub fn build() -> Box<dyn Widget> {
    super::coming_soon(
        "Invite links and the list of active peers land here once \
         shell::qs_actions::collaboration is wired in.",
    )
}
