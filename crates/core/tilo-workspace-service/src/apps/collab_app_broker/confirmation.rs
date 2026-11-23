#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// A person's confirmation, given on their own device, of one invocation
/// someone acting for them makes.
pub(super) struct Approval {
    pub(super) binding: ApprovalBinding,
    pub(super) expires: std::time::Instant,
}

/// The original invocation and publication the person actually answered.
/// Arguments are hashed so the registry never retains disclosure contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalBinding {
    pub(super) actor: MemberId,
    pub(super) provider: String,
    pub(super) workspace: String,
    pub(super) revision: Option<String>,
    pub(super) publication: (u64, u64),
    pub(super) invocation: tilo_actions::InvocationId,
    pub(super) resource: String,
    pub(super) action: String,
    pub(super) argument_digest: [u8; 32],
}

#[derive(Clone, Copy)]
pub(super) enum ApprovalPublicationKind {
    Surface,
    Directory,
}

pub(super) struct ConfirmationGrant {
    /// Only set after consuming a host-issued approval for these exact arguments.
    pub(super) host_approved: Option<ApprovalBinding>,
    pub(super) peer_id: u64,
    pub(super) resource: String,
    pub(super) action: String,
    /// Binds the exact arguments without retaining secrets or other raw user
    /// input in the grant registry.
    pub(super) argument_digest: [u8; 32],
    pub(super) actor: MemberId,
    pub(super) expires: std::time::Instant,
}

use tilo_actions::remote::argument_digest;

impl AppBrokerState {
    /// Allocate a fresh incarnation even for an identical republication.
    /// The allocator survives disconnect, so reusing a peer id cannot revive
    /// a previously issued approval.
    pub(super) fn advance_approval_publication(&self, peer: u64, kind: ApprovalPublicationKind) {
        let mut publications = self
            .approval_publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let incarnation = self
            .next_approval_publication
            .fetch_add(1, Ordering::Relaxed);
        let publication = publications.entry(peer).or_default();
        match kind {
            ApprovalPublicationKind::Surface => publication.0 = incarnation,
            ApprovalPublicationKind::Directory => publication.1 = incarnation,
        }
        drop(publications);
    }
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
        evidence: Option<&tilo_actions::remote::Confirmation>,
        binding: &ApprovalBinding,
    ) -> Result<Option<String>, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::{ActionCallError, Confirmation};
        // Only a person acting themselves confirms by their own word; anyone
        // acting for them carries the answer the person gave on their device.
        let approved = match evidence {
            Some(Confirmation::Approved(token)) => {
                binding.actor == *actor.member()
                    && binding.resource == offered.resource
                    && binding.action == offered.action
                    && binding.argument_digest == argument_digest(argument)
                    && (self.take_approval(token, binding) || person_answer::taken(token, binding))
            }
            _ => false,
        };
        // Store disclosures must be answered on a trusted host sheet, even
        // when the publisher's connection is admitted as the person themself.
        if requires_store_approval(app, &offered.action) && !approved {
            return Err(ActionCallError::confirmation_required());
        }
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
            return Err(ActionCallError::confirmation_required());
        }
        self.mint_confirmation_grant(peer_id, offered, argument, actor)
            .and_then(|token| {
                if approved {
                    self.confirmation_grants
                        .lock()
                        .map_err(|_| "the confirmation registry is unavailable".to_owned())?
                        .get_mut(&token)
                        .ok_or("the confirmation grant disappeared")?
                        .host_approved = Some(binding.clone());
                }
                Ok(token)
            })
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
    pub(crate) fn mint_confirmation_grant(
        &self,
        peer_id: u64,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &BrokerActor,
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
                actor: actor.member().clone(),
                host_approved: None,
                expires,
            },
        );
        drop(grants);
        Ok(token)
    }
    /// Record that the person `actor` acts for confirmed `actor`'s invocation
    /// of `action` on `resource` with `argument` on their own device. The
    /// returned token, carried as [`Confirmation::Approved`], answers that
    /// one invocation once, within a minute.
    ///
    /// [`Confirmation::Approved`]: tilo_actions::remote::Confirmation::Approved
    pub(crate) fn approval_confirmation(&self, binding: &ApprovalBinding) -> Option<String> {
        let now = tilo_clock::now();
        let expires = now.checked_add(std::time::Duration::from_secs(60))?;
        let token = uuid::Uuid::new_v4().to_string();
        let mut approved = self.approved_confirmations.lock().ok()?;
        approved.retain(|_, approval| approval.expires >= now);
        approved.insert(
            token.clone(),
            Approval {
                binding: binding.clone(),
                expires,
            },
        );
        drop(approved);
        Some(token)
    }
    /// Spend the person's answer `token` names, when it was given for this
    /// very invocation.
    pub(super) fn take_approval(&self, token: &str, binding: &ApprovalBinding) -> bool {
        let Ok(mut approved) = self.approved_confirmations.lock() else {
            return false;
        };
        let matches = approved.get(token).is_some_and(|approval| {
            approval.expires >= tilo_clock::now() && approval.binding == *binding
        });
        if matches {
            approved.remove(token);
        }
        matches
    }
}
