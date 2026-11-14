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

    /// Bring the membership log a 0.1 release kept up into this layout's, as
    /// the device that governs the workspace, the owner's, with `key`
    /// ([`crate::members::earlier`]). Whether it did: a document with no
    /// earlier log, or one this layout's log already holds, is left as it is.
    /// Brought up, the governance is published anew like any other brought up
    /// ([`Self::governance_brought_up`]), and the earlier log is in no
    /// history its base keeps.
    ///
    /// # Errors
    ///
    /// `key`'s device holds no certificate here, an operation cannot be
    /// written, or the log written does not give the earlier one's people at
    /// their roles; the document is then unchanged.
    pub fn carry_earlier_membership(
        &mut self,
        key: &dyn collab_types::signer::DeviceSigner,
        now: super::SystemTime,
    ) -> Result<bool, String> {
        use crate::members::earlier;
        use automerge::transaction::Transactable as _;
        let Some(held) = self.core.workspace_doc.as_deref() else {
            return Ok(false);
        };
        if !earlier::holds_earlier_log(held) {
            return Ok(false);
        }
        let workspace = self.core.workspace.clone();
        let Some(membership) = earlier::membership(held, &workspace) else {
            return Ok(false);
        };
        let device = self
            .certificate(&key.device_public_key(), now)
            .map_err(|error| error.to_string())?
            .clone();
        let mut doc = held.clone();
        doc.set_actor(self.workspace_actor_of(key));
        let written = earlier::found(
            &mut doc,
            &workspace,
            &membership,
            (&device, key),
            super::now_ns(now),
        )?;
        let founded = crate::members::replay(&doc, &workspace);
        if earlier::roles(&founded) != earlier::roles(&membership) {
            return Err(format!(
                "the membership log the workspace kept before reads as {:?}, and the one written \
                 for it as {:?}",
                earlier::roles(&membership),
                earlier::roles(&founded)
            ));
        }
        for key in earlier::earlier_keys(&doc) {
            doc.delete(automerge::ROOT, key.as_str())
                .map_err(|error| error.to_string())?;
        }
        doc.commit();
        tracing::info!(
            workspace = %workspace,
            people = founded.members.len(),
            operations = written,
            "the membership log an earlier release kept is brought up"
        );
        self.core.workspace_doc = Some(super::CachedDocument::from(doc));
        self.brought_up = true;
        Ok(true)
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
