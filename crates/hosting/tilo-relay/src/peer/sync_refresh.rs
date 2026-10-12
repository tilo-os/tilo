//! Bounded native-owner refresh requests from an existing authenticated subscription.
//! This is a host-local mailbox; it adds no wire operation or remote authority DTO.
use super::{SharedWorkspace, StateSource};
use tokio::sync::mpsc;

/// Bounded sender installed by the actual host owner for one workspace.
#[derive(Clone)]
pub struct Mailbox {
    sender: mpsc::Sender<Request>,
}
/// Closed evidence retained from the actual subscribed relay connection.
pub struct Request {
    workspace_id: u64,
    workspace: SharedWorkspace,
    source: StateSource,
    deadline: std::time::Instant,
}
impl Mailbox {
    /// At most 32 requests, each containing only bounded native identity handles.
    pub fn bounded() -> (Self, mpsc::Receiver<Request>) {
        let (sender, receiver) = mpsc::channel(32);
        (Self { sender }, receiver)
    }
    pub(super) fn enqueue(
        &self,
        workspace_id: u64,
        workspace: SharedWorkspace,
        source: StateSource,
    ) {
        // Saturation never creates an unbounded waiting future. A later request
        // on the same live session can retry; no success/ACK is reported here.
        let _ = self.sender.try_send(Request {
            workspace_id,
            workspace,
            source,
            deadline: std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(3))
                .unwrap_or_else(std::time::Instant::now),
        });
    }
}
impl Request {
    /// Runs a synchronous actual-owner turn only while the original subscription
    /// remains current. The caller must additionally check its current governed
    /// certificate and exact room Read permission before refreshing.
    ///
    /// # Errors
    ///
    /// Identifies the expired gate or failed owner turn without exposing source data.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the runtime guard is held so the refresh request and its checkpoint request observe one roster"
    )]
    pub async fn execute<T>(
        self,
        turn: impl FnOnce(u64, &tilo_user_identity::DeviceCert) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut workspace = crate::workspace_io::fresh_membership(&self.workspace)
            .await
            .map_err(|error| error.to_string())?;
        let subscribed = workspace
            .subscribers
            .get(&self.source.peer.device_public_key)
            .is_some_and(|current| current.tx.same_channel(&self.source.tx));
        if std::time::Instant::now() >= self.deadline {
            return Err("checkpoint refresh deadline elapsed".into());
        }
        if !subscribed {
            return Err("checkpoint refresh subscription is no longer current".into());
        }
        if !self.source.authorized(&workspace) {
            return Err("checkpoint refresh source authorization ended".into());
        }
        if self.source.session_live.is_none() {
            return Err("checkpoint refresh has no live session evidence".into());
        }
        // No await while the current subscription gate and native turn are held.
        let result = turn(self.workspace_id, &self.source.peer.cert)?;
        if !self.source.alive() {
            return Err("checkpoint refresh session ended during owner turn".into());
        }
        if !self.source.admits(&workspace, &self.source.peer) {
            return Err("checkpoint refresh authorization changed during owner turn".into());
        }
        workspace.request_checkpoint(&self.source.peer.device_public_key);
        Ok(result)
    }
}
