#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

pub(super) struct ConfirmationGrant {
    /// Minted on the person's own answer for this call: their approve of its asked record, or
    /// the word of the host that placed this device's window
    /// ([`AppBrokerState::answered_for_person`]).
    pub(super) person_approved: bool,
    pub(super) peer_id: u64,
    pub(super) resource: String,
    pub(super) action: String,
    /// Binds the exact arguments without retaining secrets or other raw user
    /// input in the grant registry.
    pub(super) argument_digest: [u8; 32],
    pub(super) actor: tilo_action_policy::Actor,
    pub(super) expires: std::time::Instant,
}

use tilo_actions::remote::argument_digest;

impl AppBrokerState {
    /// Check `evidence` against the offered action's confirmation policy and,
    /// when one is required, mint the one-use grant that binds it to this
    /// exact peer, address, argument and actor.
    pub(crate) fn confirmation_token(
        &self,
        peer_id: u64,
        app: &str,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
        (evidence, approved): (Option<&tilo_actions::remote::Confirmation>, bool),
    ) -> Result<Option<String>, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::{ActionCallError, Confirmation};
        // Store disclosures must be answered on a trusted host sheet, even
        // when the publisher's connection is admitted as the person themself:
        // admission asks them.
        if requires_store_approval(app, &offered.action) && !approved {
            return Ok(None);
        }
        // Only a person acting themselves confirms by their own word; anyone
        // acting for them carries the answer the person gave (`approved`,
        // [`Self::answered_for_person`]).
        let own = actor.direct().then_some(evidence).flatten();
        // Decided from the arguments, without the app's code: a field edit
        // asks only when it removes what a person may want back.
        let policy = offered.contract.confirmation_for_argument(argument);
        if matches!(policy, tilo_actions::ConfirmationPolicy::None) {
            // A confirmation given anyway answers the workspace's ask, if the
            // grants ask; admission decides whether they do.
            if !approved && !matches!(own, Some(Confirmation::Confirmed)) {
                return Ok(None);
            }
        } else if !approved && !confirmation_satisfied(&policy, own) {
            // Nobody answered for this call yet: admission asks the person.
            return Ok(None);
        }
        self.mint_grant(peer_id, offered, argument, actor, approved)
            .map(Some)
            .map_err(|reason| ActionCallError::Unavailable { reason })
    }
    /// Trusted internal callers only: admission must already have checked
    /// the person's answer for the exact invocation. Untrusted confirmations
    /// must go through `confirmation_token` instead.
    ///
    /// Mint the one-use grant carrying a confirmation already decided to
    /// the dispatch of `actor`'s invocation of `offered` on `peer_id` with
    /// `argument`, and nothing else.
    #[cfg(test)]
    pub(crate) fn mint_confirmation_grant(
        &self,
        peer_id: u64,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
    ) -> Result<String, String> {
        self.mint_grant(peer_id, offered, argument, actor, false)
    }

    /// [`Self::mint_confirmation_grant`], minted on the person's own answer where
    /// `person_approved`.
    fn mint_grant(
        &self,
        peer_id: u64,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
        person_approved: bool,
    ) -> Result<String, String> {
        let unavailable = || "the confirmation registry is unavailable".to_owned();
        let token = uuid::Uuid::new_v4().to_string();
        let now = tilo_clock::now();
        let expires = now
            .checked_add(std::time::Duration::from_secs(60))
            .ok_or_else(unavailable)?;
        let mut grants = self.confirmation_grants.lock().map_err(|_| unavailable())?;
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                argument_digest: argument_digest(argument),
                actor: actor.actor().clone(),
                person_approved,
                expires,
            },
        );
        drop(grants);
        Ok(token)
    }
}
