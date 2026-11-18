//! A workspace's document starting over from a base ([`workspace::base`]).
use super::{StatePeer, err, workspace};

impl StatePeer {
    /// Start the workspace's document over from a base, as its authority
    /// does when the workspace admits someone new. Returns whether it did: a
    /// document holding nothing past its base stays as it is.
    pub fn rebase_workspace(&mut self) -> Result<bool, String> {
        let Some(held) = self.core.workspace_doc.as_deref() else {
            return Ok(false);
        };
        let mut doc = held.clone();
        let heads = doc.get_heads();
        if heads.is_empty() || workspace::base::root(&mut doc).is_some_and(|root| heads == [root]) {
            return Ok(false);
        }
        let actor = self.workspace_actor_for(&self.core.owner);
        // A row two devices wrote at once holds both their forms, and a base
        // keeps one value of a row: it is the form that row holds.
        super::work_delegations::settle(&mut doc, &actor).map_err(err)?;
        let base = workspace::base::rebase(&doc, actor).map_err(err)?;
        self.core.workspace_doc = Some(base.into());
        Ok(true)
    }
}
