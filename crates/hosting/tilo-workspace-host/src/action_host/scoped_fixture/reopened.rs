//! Real-clock reopened owner using the existing genuine scoped test evaluator.
use super::*;

pub(crate) struct ReopenedAppFixture {
    pub(crate) owner: SessionOwner,
    pub(crate) host: ActionHost,
    signatures: SignatureMailbox,
    commits: OwnerCommitMailbox,
    network: tokio::sync::mpsc::Receiver<NeighbourMsg>,
    replayed: std::collections::BTreeSet<(String, String)>,
    _root: tempfile::TempDir,
}
impl ReopenedAppFixture {
    pub(crate) fn new(
        root: tempfile::TempDir,
        owner: SessionOwner,
        committed: CommittedAuthority,
    ) -> Self {
        let authority = Authority::new(committed);
        authority.set_ready(true);
        let (handle, commits) = OwnerCommitMailbox::bounded(8);
        let (signing, signatures) = HostSigning::new(handle);
        let (sender, network) = tokio::sync::mpsc::channel(64);
        let host = ActionHost::start_inner(&owner, authority, signing, sender, None, true).unwrap();
        Self {
            owner,
            host,
            signatures,
            commits,
            network,
            replayed: Default::default(),
            _root: root,
        }
    }
    pub(crate) async fn pump(&mut self) -> Result<(), String> {
        self.host.pump(&self.owner)?;
        while let Ok(pending) = self.signatures.try_recv() {
            let routes = self
                .owner
                .peer()
                .rows()
                .into_iter()
                .filter(|row| pending.matches_route(&row.app_id, &row.record_id))
                .map(|row| (row.app_id, row.record_id))
                .collect::<Vec<_>>();
            let update = self
                .host
                .authority
                .committed
                .begin(&self.owner)
                .map_err(|e| e.to_string())?;
            let (complete, result) = pending.apply(&mut self.owner);
            update.publish(&self.owner).map_err(|e| e.to_string())?;
            complete();
            result?;
            self.replayed.extend(routes);
        }
        while let Ok(pending) = self.commits.try_recv() {
            let update = self
                .host
                .authority
                .committed
                .begin(&self.owner)
                .map_err(|e| e.to_string())?;
            let complete = pending.apply_deferred(&mut self.owner);
            update.publish(&self.owner).map_err(|e| e.to_string())?;
            complete()?;
        }
        while self.network.try_recv().is_ok() {}
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Ok(())
    }
    pub(crate) fn action_live(&self, app: &str, record: &str, action: &str) -> bool {
        self.host
            .broker
            .published_action_surfaces()
            .iter()
            .any(|surface| {
                surface.live
                    && surface.app_id == app
                    && surface.route.record_id == record
                    && surface.actions.iter().any(|entry| entry.action == action)
            })
    }
    pub(crate) fn settled(&self) -> Result<bool, String> {
        self.host.signing.retirement_ready()
    }
    pub(crate) fn replayed(&self, app: &str, record: &str) -> bool {
        self.replayed.contains(&(app.to_owned(), record.to_owned()))
    }
    pub(crate) fn close(&mut self) -> Result<(), String> {
        self.host.close(&self.owner)
    }
}
