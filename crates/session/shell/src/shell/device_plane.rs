//! The offer round this shell's offloadable actions run through: for every
//! admitted offloadable invocation it asks the other devices of the invoking
//! workspace whether they would run the job sooner, and sends it to the one
//! that wins — or runs it here, where the job was asked
//! ([`tilo_runtime::offload::offer_offload`]).
//!
//! This device asks as itself, on the endpoint its workspaces already reach
//! it on, and reads each workspace's devices as the native owner holds them
//! ([`OfferWorkspaces`]). It does not answer here: requests for a workspace
//! this device hosts reach it through the host's plane
//! ([`ShellNetwork::workspace_host`]), and what it says when another device
//! offers it work is that plane's too. So the two devices of one person
//! weigh the same facts, and the relay a person enrolled answers as one more
//! member device of the workspace.
//!
//! The inputs an offloaded run receives and the content it answers with are
//! this workspace's document media, read and kept through the record content
//! service the broker already answers `wantBlob` for ([`RouteMedia`],
//! [`RouteOutputs`]): exactly what the invoking app could read in that
//! workspace itself.

use std::sync::{Arc, Weak};

use tilo_action_broker::record_content::RouteContent;
use tilo_actions::offload::ActionOffload;
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ReadFuture, Workspaces};
use tilo_runtime::offload::offer_door;

use super::network::ShellNetwork;

/// The action door this shell's broker places its workspaces' offloadable
/// actions through, or `None` when offload is turned off or this device has
/// no network to ask on — every offloadable action then runs here, through
/// its app's own handler.
pub(crate) fn door(
    network: &Arc<ShellNetwork>,
    content: Arc<RouteContent>,
) -> Option<Arc<dyn ActionOffload>> {
    let (plane, endpoint, runtime) = ShellNetwork::offer_plane(network)?;
    offer_door(plane, endpoint, runtime, content)
}

/// The workspaces this device is a member of, as the plane asks them: each
/// read as the native owner holds it now, so a workspace enrolled or released
/// while the session runs is answered as it stands.
pub(crate) struct OfferWorkspaces(Weak<ShellNetwork>);

impl OfferWorkspaces {
    pub(crate) fn new(network: &Arc<ShellNetwork>) -> Self {
        Self(Arc::downgrade(network))
    }
}

impl Workspaces for OfferWorkspaces {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        let Some(owner) = self
            .0
            .upgrade()
            .and_then(|network| network.opened_native_owner())
        else {
            return Box::pin(std::future::ready(false));
        };
        let Some(workspace) = tilo_desktop_model::RoomId::parse(workspace) else {
            return Box::pin(std::future::ready(false));
        };
        Box::pin(async move {
            owner
                .with_current(workspace, |view| {
                    read(view.peer());
                    Ok(())
                })
                .is_ok()
        })
    }
}
