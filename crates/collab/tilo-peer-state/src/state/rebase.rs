//! A workspace's document starting over from a base ([`workspace::base`]).
use super::{StatePeer, err, workspace};

impl StatePeer {
    /// Whether [`Self::restore`] brought this peer's governance up from the
    /// layout a build before this one stored ([`workspace::layout`]). What a
    /// device that governs the workspace published before is in that layout:
    /// it starts the workspace's document over and publishes it anew.
    #[must_use]
    pub const fn governance_brought_up(&self) -> bool {
        self.brought_up
    }

    /// Start the workspace's document over from a base, as its authority
    /// does when the workspace admits someone new. Returns whether it did: a
    /// document holding nothing past its base stays as it is.
    pub fn rebase_workspace(&mut self) -> Result<bool, String> {
        self.rebase_workspace_when(false)
    }

    /// [`Self::rebase_workspace`], even when the document is a base already:
    /// a governance brought up from an earlier layout is a base each device
    /// wrote for itself ([`Self::governance_brought_up`]), and the base its
    /// governing device publishes must be newer than any of them.
    pub fn start_governance_over(&mut self) -> Result<bool, String> {
        self.rebase_workspace_when(true)
    }

    fn rebase_workspace_when(&mut self, even_a_base: bool) -> Result<bool, String> {
        let Some(held) = self.core.workspace_doc.as_deref() else {
            return Ok(false);
        };
        let mut doc = held.clone();
        let heads = doc.get_heads();
        if heads.is_empty()
            || (!even_a_base && workspace::base::root(&mut doc).is_some_and(|root| heads == [root]))
        {
            return Ok(false);
        }
        let actor = self.workspace_actor_for(&self.core.owner);
        let base = workspace::base::rebase(&doc, actor).map_err(err)?;
        self.core.workspace_doc = Some(base.into());
        Ok(true)
    }
}
