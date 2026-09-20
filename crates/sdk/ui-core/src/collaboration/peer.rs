//! Host-injected authenticated peer transport. No runtime, socket, GPU or Send
//! requirements: the browser adapter owns its asynchronous transport locally.
use super::{ActionDirectoryRequest, CollabEvent, CollabRequest, RouteSpec};

/// Transport adapters must authenticate ordinary peer joins and verify inbound
/// envelopes before publishing events. Installing this adapter grants no model,
/// action, HostContext, or storage authority. Queue failures are fail-closed.
pub trait PeerTransport {
    fn is_connected(&self) -> bool;
    fn send(&mut self, request: CollabRequest) -> Result<(), String>;
    fn poll(&mut self) -> Vec<CollabEvent>;
    /// The real broker acknowledgement of the latest Hello, never an optimistic echo.
    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        None
    }
    fn join_route(&mut self, _: RouteSpec) -> Result<(), String> {
        Err("route joining unavailable".into())
    }
    fn send_to_route(&mut self, _: RouteSpec, _: CollabRequest) -> Result<(), String> {
        Err("route sending unavailable".into())
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Err("action directory unavailable".into())
    }
    fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        vec![]
    }
    fn install_document_encryption(
        &mut self,
        _: RouteSpec,
        _: channel_crypto::ChannelGroup,
    ) -> Result<(), String> {
        Err("document encryption unavailable".into())
    }
}
