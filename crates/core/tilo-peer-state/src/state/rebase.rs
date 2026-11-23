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
        let base = workspace::base::rebase(&doc, actor).map_err(err)?;
        self.core.workspace_doc = Some(base.into());
        Ok(true)
    }

    /// Write again onto the newer base this copy adopted from `before` the
    /// changes `device` made there that the base does not hold
    /// ([`workspace::base::carried`]): a holder that wrote against the
    /// history its base maker replaced keeps what it wrote. Returns whether
    /// anything was carried; a copy that adopted no newer base carries nothing.
    ///
    /// # Errors
    /// The documents could not be read or written.
    pub fn carry_onto_base(&mut self, before: &Self, device: &[u8; 32]) -> Result<bool, String> {
        let (Some(was), Some(is)) = (
            before.core.workspace_doc.as_deref(),
            self.core.workspace_doc.as_deref(),
        ) else {
            return Ok(false);
        };
        if workspace::base::generation(is) <= workspace::base::generation(was) {
            return Ok(false);
        }
        let mut doc = is.clone();
        let actor = self.workspace_actor_for(device);
        if !workspace::base::carried(was, &mut doc, &actor).map_err(err)? {
            return Ok(false);
        }
        self.core.workspace_doc = Some(doc.into());
        Ok(true)
    }
}
