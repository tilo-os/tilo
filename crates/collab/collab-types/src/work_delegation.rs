//! A member's signed delegation of their own work to a server device.
//!
//! Work in a workspace is the workspace's or a member's. Any key holder takes
//! the workspace's work; a member's work — an agent using their mail, an
//! outbox entry that needs their password, a Browser they are signed in to —
//! is taken by a server of another person only because the member said so. The
//! member's own device signs this delegation: it names the server device, this
//! workspace, the work by the grant paths the workspace decides actions by,
//! the actor the server acts as (the member, or the member's agent), and until
//! when. It lives in the workspace, so every holder replays the same rule; the
//! member revokes it by signing it again with the revocation set.
//!
//! Every form of a delegation is the member's own signed statement, and the
//! forms of one row are ordered by what they say, never by who wrote the row,
//! when a form arrived or what a device's clock read
//! ([`WorkDelegation::replaces`]). Each form signs the number of its giving. A
//! withdrawal stands over every form of its giving and of the givings before
//! it, and only work given by a device that holds the withdrawal takes the
//! next number and stands over it ([`WorkDelegation::given_after`]). A
//! delegation lives [`MAX_LIFETIME_NS`] at most; the member's own devices sign
//! it again to live on ([`WorkDelegation::renewed`]), so a device that never
//! saw a withdrawal is held by an end that is near.
//!
//! A withdrawal carries its cut under the same signature: what the member's
//! device held of the server's work when it took the work back, document by
//! document ([`HeldWork`]). A holder of the withdrawal takes of that work what
//! the cut keeps and nothing else ([`WorkDelegation::keeps`]). The time a
//! write names is its author's and the time a withdrawal names is its
//! signer's, so neither decides: a withdrawal is read live at no time.
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::signer::{DeviceSigner, sign_now};
use tilo_action_policy::{ActionKind, Answer, Grant, TreePath};

/// Key of the workspace's work delegations in its own document: a map from
/// each delegation's row (the server's device key and the member's root, hex,
/// [`WorkDelegation::row_key`]) to the signed delegation (JSON).
pub const WORK_DELEGATIONS_KEY: &str = "work_delegations_v1";

/// The signing domain: a signature made over another shape never verifies.
const WORK_DELEGATION_DOMAIN: &[u8] = b"tilo-work-delegation-v3\0";

/// Longest a delegation lives from its issue: seven days. A withdrawal is kept
/// by every device that saw it; a device that never saw it is held by nothing
/// but the delegation's end, so the end is near. Nothing signs past it and
/// nothing longer verifies.
pub const MAX_LIFETIME_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// Most grant paths one delegation may name.
pub const MAX_DELEGATED_WORK: usize = 32;
/// Longest agent id a delegation may act as.
pub const MAX_DELEGATED_ACTOR_BYTES: usize = crate::member::MAX_AGENT_ID_BYTES;
/// Most of the member's vault items one delegation may let the server use.
pub const MAX_DELEGATED_SECRETS: usize = 32;

/// What the member's device held of the server's work in one document when it
/// took the work back: the place in that document's history its withdrawal
/// keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldWork {
    /// The document, as the workspace keys its documents: by its app and its
    /// own id.
    pub route: String,
    /// The number of the server's last write to it that the device held:
    /// every write carries its author's own running number per document.
    pub last: u64,
    /// The server's compactions in the document's history that the device
    /// held, each by that write's id. A compaction restates history under a
    /// number its author used before, so its number says nothing of it.
    pub compactions: Vec<[u8; 32]>,
}

/// Why a work delegation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkDelegationError {
    /// The signature does not verify under the signing device's key.
    #[error("the work delegation's signature does not verify")]
    Signature,
    /// It names another workspace.
    #[error("the work delegation names another workspace")]
    OtherWorkspace,
    /// Outside its validity window, or withdrawn.
    #[error("the work delegation is not current")]
    NotCurrent,
    /// A field is out of bounds, or it names no work at all.
    #[error("invalid work delegation: {0}")]
    Invalid(&'static str),
    /// The signer answered only later, outside the turn that writes it.
    #[error("the signing device answers only later")]
    Signer,
}

/// A member's signed statement that `server` may take their work in
/// `workspace` as `agent` (or as the member themselves) within `grants`, until
/// `expires_at_ns`, unless revoked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDelegation {
    /// The member delegating their work: their user root key.
    pub person: [u8; 32],
    /// The member's device that signed it.
    pub device: [u8; 32],
    /// The server device that may take the work.
    pub server: [u8; 32],
    /// The workspace the delegation is for, as the workspace names it.
    pub workspace: String,
    /// The agent of the member's the server acts as; `None` acts as the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Which work, by the paths the workspace decides actions by: a change is
    /// the member's work only where one of these allows it.
    pub grants: Vec<Grant>,
    /// Which of the member's vault items the server may use, by their ids: a
    /// delegation names none, and then the server uses no secret of theirs.
    /// An item's own leave and its person's answer still decide each use; this
    /// says only which items the server may be answered about at all.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// The number of this giving of the work, from 1. A form signed again to
    /// live on ([`Self::renewed`]) and a withdrawal keep the number of the
    /// form they are signed over; only work given by a device that holds the
    /// withdrawal of a giving takes the next number ([`Self::given_after`]).
    /// The forms of a row are ordered by it ([`Self::replaces`]), so no
    /// device's clock decides whether a withdrawal stays.
    pub giving: u64,
    /// When this form was issued, ns since the epoch.
    pub issued_at_ns: u64,
    /// When it stops working, ns since the epoch.
    pub expires_at_ns: u64,
    /// When the member revoked it, ns since the epoch; `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ns: Option<u64>,
    /// The cut of a withdrawal: what the member's device held of the server's
    /// work when it took the work back, one entry per document, ordered by
    /// route. A holder of the withdrawal takes of the server's work for this
    /// member what the cut keeps ([`Self::keeps`]) and nothing else, whatever
    /// time a write names. Only a withdrawal carries one.
    #[serde(default)]
    pub held: Vec<HeldWork>,
    /// The signing device's signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct Unsigned<'a> {
    person: &'a [u8; 32],
    device: &'a [u8; 32],
    server: &'a [u8; 32],
    workspace: &'a str,
    agent: &'a Option<String>,
    grants: &'a [Grant],
    secrets: &'a [String],
    giving: u64,
    issued_at_ns: u64,
    expires_at_ns: u64,
    revoked_at_ns: &'a Option<u64>,
    held: &'a [HeldWork],
}

impl WorkDelegation {
    /// A delegation by `key`'s device of its person's work to `server` in
    /// `workspace`, as `agent` (or as the person) and within `grants`, letting
    /// it use the vault items `secrets`, from `valid`'s first instant until its
    /// second, both ns since the epoch and [`MAX_LIFETIME_NS`] apart at most.
    /// It is the first giving of that work: work given where a form of its
    /// row is held follows that form ([`Self::given_after`]).
    ///
    /// # Errors
    /// [`WorkDelegationError::Invalid`] for out-of-bounds fields,
    /// [`WorkDelegationError::Signer`] when the device key answers only later.
    pub fn sign(
        key: &dyn DeviceSigner,
        person: [u8; 32],
        server: [u8; 32],
        workspace: impl Into<String>,
        agent: Option<String>,
        grants: Vec<Grant>,
        secrets: Vec<String>,
        (issued_at_ns, expires_at_ns): (u64, u64),
    ) -> Result<Self, WorkDelegationError> {
        Self::sealed(
            key,
            Self {
                person,
                device: key.device_public_key(),
                server,
                workspace: workspace.into(),
                agent,
                grants,
                secrets,
                giving: 1,
                issued_at_ns,
                expires_at_ns,
                revoked_at_ns: None,
                held: Vec::new(),
                signature: [0; 64],
            },
        )
    }

    /// `delegation` with its signature set: the one place a delegation is made
    /// whole, so what is checked is what is signed.
    fn sealed(key: &dyn DeviceSigner, mut delegation: Self) -> Result<Self, WorkDelegationError> {
        delegation.signature = [0; 64];
        delegation.validate()?;
        delegation.signature =
            sign_now(key, delegation.signing_bytes()).map_err(|_| WorkDelegationError::Signer)?;
        Ok(delegation)
    }

    /// The same delegation, revoked at `revoked_at_ns` and signed again by the
    /// member's device `key`, with a cut that names no document: it keeps
    /// nothing of the server's work ([`Self::withdrawn`]).
    ///
    /// # Errors
    /// As [`Self::withdrawn`].
    pub fn revoked(
        &self,
        key: &dyn DeviceSigner,
        revoked_at_ns: u64,
    ) -> Result<Self, WorkDelegationError> {
        self.withdrawn(key, revoked_at_ns, Vec::new())
    }

    /// The same delegation, withdrawn at `revoked_at_ns` and signed again by
    /// the member's device `key`, carrying its cut: `held`, what that device
    /// holds of the server's work, one entry per document. The cut is part of
    /// what the device signs, so only a device of the member's says what a
    /// withdrawal keeps.
    ///
    /// # Errors
    /// As [`Self::sign`], and [`WorkDelegationError::Invalid`] when `key` is
    /// not the device that signed it or `held` names a document twice.
    pub fn withdrawn(
        &self,
        key: &dyn DeviceSigner,
        revoked_at_ns: u64,
        mut held: Vec<HeldWork>,
    ) -> Result<Self, WorkDelegationError> {
        if key.device_public_key() != self.device {
            return Err(WorkDelegationError::Invalid(
                "only the device that signed it revokes it",
            ));
        }
        held.sort_by(|one, other| one.route.cmp(&other.route));
        let mut revoked = self.clone();
        revoked.revoked_at_ns = Some(revoked_at_ns);
        revoked.held = held;
        Self::sealed(key, revoked)
    }

    /// The same giving of the same work, signed again by the member's device
    /// `key` to live from `valid`'s first instant until its second: how a
    /// delegation outlives [`MAX_LIFETIME_NS`] without its member being asked
    /// again. It is no new giving: a withdrawal of this giving stands over it
    /// ([`Self::replaces`]), so a device that renews before a withdrawal
    /// reaches it puts nothing back.
    ///
    /// # Errors
    /// As [`Self::sign`], and [`WorkDelegationError::Invalid`] for a withdrawn
    /// delegation: what its member withdrew is given again, never renewed.
    pub fn renewed(
        &self,
        key: &dyn DeviceSigner,
        (issued_at_ns, expires_at_ns): (u64, u64),
    ) -> Result<Self, WorkDelegationError> {
        if self.revoked_at_ns.is_some() {
            return Err(WorkDelegationError::Invalid(
                "a withdrawn delegation is given again, not renewed",
            ));
        }
        let mut renewed = self.clone();
        renewed.device = key.device_public_key();
        renewed.issued_at_ns = issued_at_ns;
        renewed.expires_at_ns = expires_at_ns;
        Self::sealed(key, renewed)
    }

    /// This work as the member's device `key` gives it where its row holds
    /// `held`: the giving after `held`'s once `held` is a withdrawal, and
    /// `held`'s own giving while it is not. So only a device that holds a
    /// withdrawal signs work that stands over it ([`Self::replaces`]): work
    /// given on a device the withdrawal has not reached stays withdrawn,
    /// whatever either device's clock read.
    ///
    /// # Errors
    /// As [`Self::sign`], and [`WorkDelegationError::Invalid`] when this is a
    /// withdrawal, `held` is of another row or workspace, or the row's
    /// givings are used up.
    pub fn given_after(
        &self,
        key: &dyn DeviceSigner,
        held: &Self,
    ) -> Result<Self, WorkDelegationError> {
        if self.revoked_at_ns.is_some() {
            return Err(WorkDelegationError::Invalid(
                "a withdrawal is of the giving it withdraws",
            ));
        }
        if self.row_key() != held.row_key() || self.workspace != held.workspace {
            return Err(WorkDelegationError::Invalid("another delegation"));
        }
        let mut given = self.clone();
        given.device = key.device_public_key();
        given.giving = if held.revoked_at_ns.is_some() {
            held.giving
                .checked_add(1)
                .ok_or(WorkDelegationError::Invalid("giving"))?
        } else {
            held.giving
        };
        Self::sealed(key, given)
    }

    /// Whether this form stands in its row where `held` stood. The member's
    /// own signed statements decide, never who wrote the row, when a form
    /// arrived or what a clock read: a later giving stands over an earlier
    /// one; of one giving, its withdrawal stands over every other form, and
    /// otherwise the form issued later. So the form a delegation was first
    /// signed in never stands again once it was withdrawn, nor does a renewal
    /// of it, and every holder of the same forms reads the same one.
    #[must_use]
    pub fn replaces(&self, held: &Self) -> bool {
        self.signed_order() > held.signed_order()
    }

    /// Where this form stands among the forms of its row, largest last.
    fn signed_order(&self) -> (u64, bool, u64, &[u8; 64]) {
        (
            self.giving,
            self.revoked_at_ns.is_some(),
            self.issued_at_ns,
            &self.signature,
        )
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let unsigned = Unsigned {
            person: &self.person,
            device: &self.device,
            server: &self.server,
            workspace: &self.workspace,
            agent: &self.agent,
            grants: &self.grants,
            secrets: &self.secrets,
            giving: self.giving,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
            revoked_at_ns: &self.revoked_at_ns,
            held: &self.held,
        };
        let mut bytes = WORK_DELEGATION_DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&unsigned).unwrap_or_default());
        bytes
    }

    /// The member, or the agent of theirs, the server acts as: the actor
    /// history names the work by.
    #[must_use]
    pub fn actor(&self) -> tilo_action_policy::MemberId {
        match &self.agent {
            Some(agent) => tilo_action_policy::MemberId::agent(agent.clone()),
            None => tilo_action_policy::MemberId::Person(tilo_action_policy::PersonId::from_root(
                self.person,
            )),
        }
    }

    fn validate(&self) -> Result<(), WorkDelegationError> {
        if self.workspace.is_empty() {
            return Err(WorkDelegationError::Invalid("workspace"));
        }
        if self.grants.is_empty() {
            return Err(WorkDelegationError::Invalid("work"));
        }
        let actor = tilo_action_policy::Grantee::Member(self.actor());
        let person = tilo_action_policy::Grantee::person(hex::encode(self.person));
        if self.grants.len() > MAX_DELEGATED_WORK
            || self.grants.iter().any(|grant| {
                grant.answer != Answer::Allow
                    || grant.grantee != actor
                    || grant.granted_by != person
            })
        {
            return Err(WorkDelegationError::Invalid("grants"));
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| agent.is_empty() || agent.len() > MAX_DELEGATED_ACTOR_BYTES)
        {
            return Err(WorkDelegationError::Invalid("agent"));
        }
        if self.secrets.len() > MAX_DELEGATED_SECRETS
            || self.secrets.iter().any(|item| !is_item_id(item))
        {
            return Err(WorkDelegationError::Invalid("secrets"));
        }
        if self.giving == 0 {
            return Err(WorkDelegationError::Invalid("giving"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(WorkDelegationError::Invalid("validity window"));
        }
        if self.expires_at_ns.saturating_sub(self.issued_at_ns) > MAX_LIFETIME_NS {
            return Err(WorkDelegationError::Invalid("lifetime"));
        }
        if self
            .revoked_at_ns
            .is_some_and(|revoked| revoked < self.issued_at_ns)
        {
            return Err(WorkDelegationError::Invalid("revocation"));
        }
        // Only a withdrawal carries a cut, and a cut names each document once,
        // in one order: every device then signs and reads the same statement.
        if (self.revoked_at_ns.is_none() && !self.held.is_empty())
            || !self.held.is_sorted_by(|one, other| one.route < other.route)
        {
            return Err(WorkDelegationError::Invalid("cut"));
        }
        Ok(())
    }

    /// Whether the delegation is signed by its device and names `workspace`, at
    /// no particular time: what a holder checks before it lists one.
    ///
    /// # Errors
    /// [`WorkDelegationError`] when it does not hold.
    pub fn verify_signature(&self, workspace: &str) -> Result<(), WorkDelegationError> {
        if self.workspace != workspace {
            return Err(WorkDelegationError::OtherWorkspace);
        }
        self.validate()?;
        let key =
            VerifyingKey::from_bytes(&self.device).map_err(|_| WorkDelegationError::Signature)?;
        key.verify_strict(
            &self.signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| WorkDelegationError::Signature)
    }

    /// [`Self::verify_signature`], and that it is live at `now_ns`: issued,
    /// not expired, and not withdrawn. A withdrawal is live at no time, the
    /// time before the stamp it carries included: that stamp is its signer's
    /// clock, and a time asked for may be whatever a write's author stamped.
    /// What a withdrawal keeps of the server's work is its cut
    /// ([`Self::keeps`]).
    ///
    /// # Errors
    /// [`WorkDelegationError`] when it does not hold or is not current.
    pub fn verify(&self, workspace: &str, now_ns: u64) -> Result<(), WorkDelegationError> {
        self.verify_signature(workspace)?;
        // One that was issued after the asking device's clock is not yet
        // anyone's to take: every holder then reads the same live set.
        if self.revoked_at_ns.is_some()
            || now_ns < self.issued_at_ns
            || now_ns >= self.expires_at_ns
        {
            return Err(WorkDelegationError::NotCurrent);
        }
        Ok(())
    }

    /// Whether this withdrawal keeps the server's write number `sequence` to
    /// the document `route`: the member's device held it when it took the
    /// work back. `compaction` is the write's id when it is a compaction:
    /// then that id decides, never the number, which a compaction shares with
    /// a write it folds. A document the cut does not name keeps nothing, and
    /// a form that is no withdrawal keeps nothing: the work under it is
    /// decided while it is live ([`Self::verify`]).
    #[must_use]
    pub fn keeps(&self, route: &str, sequence: u64, compaction: Option<&[u8; 32]>) -> bool {
        if self.revoked_at_ns.is_none() {
            return false;
        }
        self.held
            .iter()
            .find(|held| held.route == route)
            .is_some_and(|held| match compaction {
                Some(id) => held.compactions.contains(id),
                None => (1..=held.last).contains(&sequence),
            })
    }

    /// Whether the server may take work at `node` of `kind` under it.
    #[must_use]
    pub fn allows(&self, node: &TreePath, kind: ActionKind) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.kinds.contains(kind) && grant.node.covers(node))
    }

    /// Whether the delegation lets the server use the vault item `item`: only
    /// the items the member named are theirs to use there, and an item they
    /// named none of stays on their own devices.
    #[must_use]
    pub fn allows_secret(&self, item: &str) -> bool {
        self.secrets.iter().any(|named| named == item)
    }

    /// The row it is kept under: the server's device key and the member's
    /// root, hex. A server may hold one delegation per member, and a member
    /// one per server, so two holders' rows never replace each other.
    #[must_use]
    pub fn row_key(&self) -> String {
        row_key(&self.server, &self.person)
    }

    /// The server and member [`Self::row_key`] names, when `key` is one.
    #[must_use]
    pub fn row_parts(key: &str) -> Option<([u8; 32], [u8; 32])> {
        let (server, person) = key.split_once('/')?;
        Some((key_of(server)?, key_of(person)?))
    }
}

fn row_key(server: &[u8; 32], person: &[u8; 32]) -> String {
    format!("{}/{}", hex::encode(server), hex::encode(person))
}

fn key_of(text: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(text).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Whether `text` is a vault item's id: 32 lowercase hexadecimal digits, as
/// the vault makes them.
fn is_item_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The one vault item the test's delegation names.
    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    /// One day, ns.
    const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

    fn delegation(key: &SigningKey, server: [u8; 32], expires_at_ns: u64) -> WorkDelegation {
        signed(key, server, (1, expires_at_ns))
    }

    /// The test's work as `key`'s device first gives it to `server` for `valid`.
    fn signed(key: &SigningKey, server: [u8; 32], valid: (u64, u64)) -> WorkDelegation {
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let grant = Grant::new(
            "send-mail",
            tilo_action_policy::Grantee::person(hex::encode(person)),
            TreePath::workspace("w").app("so.tilo.mail"),
            tilo_action_policy::KindSet::READ_UNDOABLE,
            Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(person)),
        );
        WorkDelegation::sign(
            key,
            person,
            server,
            "w",
            None,
            vec![grant],
            vec![SECRET.to_owned()],
            valid,
        )
        .unwrap()
    }

    /// Signed work for a native assistant keeps the same bounded id as its workspace membership.
    #[test]
    fn work_can_be_given_to_a_canonical_personal_assistant() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let assistant = format!("assistant-{}", hex::encode(person));
        let given = |agent: String| {
            let grant = Grant::new(
                "read",
                tilo_action_policy::Grantee::agent(agent.clone()),
                TreePath::workspace("w").app("so.tilo.chat"),
                tilo_action_policy::KindSet::READ,
                Answer::Allow,
                tilo_action_policy::Grantee::person(hex::encode(person)),
            );
            WorkDelegation::sign(
                &key,
                person,
                [8; 32],
                "w",
                Some(agent),
                vec![grant],
                Vec::new(),
                (1, 1_000),
            )
        };
        for id in [
            assistant.clone(),
            format!("{assistant}-2"),
            "a".repeat(MAX_DELEGATED_ACTOR_BYTES),
        ] {
            let signed = given(id.clone()).expect("a workspace agent can receive work");
            signed.verify("w", 500).expect("its signed work verifies");
            assert_eq!(signed.agent.as_deref(), Some(id.as_str()));
        }
        assert!(matches!(
            given("a".repeat(MAX_DELEGATED_ACTOR_BYTES + 1)),
            Err(WorkDelegationError::Invalid("agent"))
        ));
    }

    /// The member's device signs a delegation, every reader checks it, a
    /// revocation is the member's device's and stops it, and another device's
    /// signature names another row.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_delegation_is_the_members_devices_and_stops_when_it_says_so() -> TestResult {
        let key = SigningKey::from_bytes(&[7; 32]);
        let delegation = delegation(&key, [8; 32], 1_000);
        delegation.verify("w", 500)?;
        assert_eq!(
            delegation.verify("w", 1_000),
            Err(WorkDelegationError::NotCurrent),
            "an expired delegation takes no work"
        );
        assert_eq!(
            delegation.verify("other", 500),
            Err(WorkDelegationError::OtherWorkspace)
        );

        let mut forged = delegation.clone();
        forged.server = [3; 32];
        assert_eq!(
            forged.verify("w", 500),
            Err(WorkDelegationError::Signature),
            "the signed bytes bind the server it names"
        );

        let revoked = delegation.revoked(&key, 600)?;
        for at in [1, 599, 600, 999] {
            assert_eq!(
                revoked.verify("w", at),
                Err(WorkDelegationError::NotCurrent),
                "a withdrawal is read live at no time, the time before its stamp included: {at}"
            );
        }
        assert_eq!(
            delegation.revoked(&SigningKey::from_bytes(&[6; 32]), 600),
            Err(WorkDelegationError::Invalid(
                "only the device that signed it revokes it"
            )),
            "no other device revokes it"
        );

        let node = TreePath::workspace("w")
            .app("so.tilo.mail")
            .kind("message")
            .action("send");
        assert!(delegation.allows(&node, ActionKind::Undoable));
        assert!(!delegation.allows(&node, ActionKind::Outside));
        let elsewhere = TreePath::workspace("w")
            .app("so.tilo.chat")
            .kind("message")
            .action("send");
        assert!(!delegation.allows(&elsewhere, ActionKind::Undoable));

        // Only the vault items the member named are the server's to use there.
        assert!(delegation.allows_secret(SECRET));
        assert!(!delegation.allows_secret("fedcba9876543210fedcba9876543210"));

        // What names no item of theirs lets it use none: a delegation names
        // what it means to, and a shape no item has is refused where signed.
        let mut unnamed = delegation.clone();
        unnamed.secrets.clear();
        assert!(!unnamed.allows_secret(SECRET));
        unnamed.secrets = vec!["not-an-item".to_owned()];
        assert_eq!(
            unnamed.validate(),
            Err(WorkDelegationError::Invalid("secrets"))
        );
        unnamed.secrets = vec![SECRET.to_owned(); MAX_DELEGATED_SECRETS + 1];
        assert_eq!(
            unnamed.validate(),
            Err(WorkDelegationError::Invalid("secrets"))
        );

        let row = delegation.row_key();
        assert_eq!(
            WorkDelegation::row_parts(&row),
            Some(([8; 32], delegation.person))
        );
        assert_eq!(WorkDelegation::row_parts("nonsense"), None);
        Ok(())
    }

    /// A delegation lives seven days at most: nothing signs a longer one, and
    /// one made longer after it was signed is refused before its signature is
    /// read, live or withdrawn.
    #[test]
    fn a_delegation_lives_seven_days_at_most_and_nothing_signs_past_it() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let whole = delegation(&key, [8; 32], 1 + MAX_LIFETIME_NS);
        assert_eq!(whole.verify("w", MAX_LIFETIME_NS), Ok(()));
        assert_eq!(MAX_LIFETIME_NS, 7 * DAY);

        let longer = WorkDelegation::sign(
            &key,
            person,
            [8; 32],
            "w",
            None,
            whole.grants.clone(),
            Vec::new(),
            (1, 2 + MAX_LIFETIME_NS),
        );
        assert_eq!(
            longer,
            Err(WorkDelegationError::Invalid("lifetime")),
            "nothing signs past seven days"
        );
        assert_eq!(
            whole.renewed(&key, (5, 6 + MAX_LIFETIME_NS)),
            Err(WorkDelegationError::Invalid("lifetime")),
            "nor does a renewal"
        );

        let mut stretched = whole;
        stretched.expires_at_ns += 1;
        assert_eq!(
            stretched.verify_signature("w"),
            Err(WorkDelegationError::Invalid("lifetime")),
            "a longer one is refused whoever holds it"
        );
        assert_eq!(
            stretched.verify("w", 500),
            Err(WorkDelegationError::Invalid("lifetime"))
        );
    }

    /// The forms of one row stand by what their member signed. A withdrawal
    /// stands over the form it withdrew, over a renewal signed after it by a
    /// device that had not seen it and over work such a device gives; only
    /// work given by a device that holds the withdrawal stands over it; a
    /// renewal stands over what it renews.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_withdrawal_stands_over_everything_given_before_it() -> TestResult {
        let key = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[6; 32]);
        let first = delegation(&key, [8; 32], 1_000);
        assert_eq!(first.giving, 1, "work is first given as its first giving");
        let withdrawn = first.revoked(&key, 600)?;
        assert_eq!(
            withdrawn.giving, 1,
            "a withdrawal is of the giving it withdraws"
        );
        assert!(
            withdrawn.replaces(&first),
            "a withdrawal stands over its delegation"
        );
        assert!(
            !first.replaces(&withdrawn),
            "the first form never stands again once it was withdrawn"
        );
        assert!(!first.replaces(&first) && !withdrawn.replaces(&withdrawn));

        // The member's other device signs the work again to live on.
        let renewed = first.renewed(&other, (500, 2_000))?;
        renewed.verify("w", 1_500)?;
        assert_eq!(renewed.device, other.verifying_key().to_bytes());
        assert_eq!(renewed.giving, first.giving, "a renewal is no new giving");
        assert!(renewed.replaces(&first) && !first.replaces(&renewed));
        assert!(
            withdrawn.replaces(&renewed) && !renewed.replaces(&withdrawn),
            "a withdrawal stands over a renewal of what it withdrew"
        );

        // A device that had not seen the withdrawal renews after it, and its
        // member gives the work there again: neither puts anything back.
        let late = first.renewed(&other, (700, 2_000))?;
        assert!(
            withdrawn.replaces(&late) && !late.replaces(&withdrawn),
            "a renewal signed after a withdrawal puts nothing back"
        );
        let unseen = signed(&other, [8; 32], (800, 2_000)).given_after(&other, &late)?;
        assert_eq!(
            unseen.giving, 1,
            "work given over a live form is of its giving"
        );
        assert!(
            withdrawn.replaces(&unseen) && !unseen.replaces(&withdrawn),
            "work given without the withdrawal in hand stays withdrawn"
        );
        assert_eq!(
            withdrawn.renewed(&key, (700, 2_000)),
            Err(WorkDelegationError::Invalid(
                "a withdrawn delegation is given again, not renewed"
            ))
        );

        // Only work given by a device that holds the withdrawal stands over it.
        let again = signed(&key, [8; 32], (601, 2_000)).given_after(&key, &withdrawn)?;
        assert_eq!(again.giving, 2);
        again.verify("w", 700)?;
        assert!(again.replaces(&withdrawn) && !withdrawn.replaces(&again));
        assert!(again.replaces(&unseen) && again.replaces(&first));
        let later_withdrawal = again.revoked(&key, 900)?;
        assert!(later_withdrawal.replaces(&again) && later_withdrawal.replaces(&withdrawn));
        assert_eq!(
            signed(&key, [3; 32], (601, 2_000)).given_after(&key, &withdrawn),
            Err(WorkDelegationError::Invalid("another delegation")),
            "work follows a form of its own row"
        );

        // A form whose number is moved is another statement: it does not
        // verify, and one that names no giving is none.
        let mut moved = late;
        moved.giving = 2;
        assert_eq!(
            moved.verify_signature("w"),
            Err(WorkDelegationError::Signature),
            "the signed bytes bind the number of the giving"
        );
        moved.giving = 0;
        assert_eq!(
            moved.verify_signature("w"),
            Err(WorkDelegationError::Invalid("giving"))
        );
        Ok(())
    }

    /// The order of a row's forms rests on the number of the giving and on no
    /// clock. A member's device whose clock is a day ahead gives the work;
    /// the member withdraws it on a device whose clock is right, so the
    /// withdrawal is stamped a day before the giving it withdraws. It stands
    /// all the same: over that giving, over the renewal the first device signs
    /// unasked, and over work given there again without the withdrawal.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_giving_stamped_ahead_of_a_later_withdrawal_stays_withdrawn() -> TestResult {
        let ahead = SigningKey::from_bytes(&[7; 32]);
        let right = SigningKey::from_bytes(&[6; 32]);
        let given = signed(&ahead, [8; 32], (DAY, 2 * DAY));
        assert_eq!(
            given.revoked(&ahead, 1_000),
            Err(WorkDelegationError::Invalid("revocation")),
            "no withdrawal is stamped before the form it is signed over"
        );
        // So the device with the right clock signs the giving as its own form,
        // by its own clock, and withdraws that.
        let withdrawn = given.renewed(&right, (999, DAY))?.revoked(&right, 1_000)?;
        assert_eq!(withdrawn.giving, given.giving);
        assert!(
            withdrawn.revoked_at_ns < Some(given.issued_at_ns),
            "the withdrawal is stamped before the giving"
        );
        assert!(
            withdrawn.replaces(&given) && !given.replaces(&withdrawn),
            "a withdrawal stamped before its giving stands over it"
        );
        assert_eq!(
            withdrawn.verify("w", 1_000),
            Err(WorkDelegationError::NotCurrent)
        );

        let renewal = given.renewed(&ahead, (DAY + 5, 3 * DAY))?;
        assert!(
            withdrawn.replaces(&renewal) && !renewal.replaces(&withdrawn),
            "a renewal stamped a day after the withdrawal puts nothing back"
        );
        let unseen = signed(&ahead, [8; 32], (DAY + 6, 3 * DAY)).given_after(&ahead, &renewal)?;
        assert!(
            withdrawn.replaces(&unseen) && !unseen.replaces(&withdrawn),
            "nor does work given there without the withdrawal in hand"
        );
        Ok(())
    }

    /// A member's device whose clock is a day behind holds a withdrawal,
    /// and its member gives the work again there: stamped before the
    /// withdrawal, the new giving stands over it by its number, and its own
    /// withdrawal stands over everything before in turn. What a clock still
    /// decides is each form's own window, and that fails closed.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_giving_stamped_behind_an_earlier_withdrawal_stands_by_its_number() -> TestResult {
        let right = SigningKey::from_bytes(&[7; 32]);
        let behind = SigningKey::from_bytes(&[6; 32]);
        let first = signed(&right, [8; 32], (10 * DAY, 11 * DAY));
        let withdrawn = first.revoked(&right, 10 * DAY + 500)?;

        // The device behind reads nine days where the others read ten.
        let again =
            signed(&behind, [8; 32], (9 * DAY + 600, 12 * DAY)).given_after(&behind, &withdrawn)?;
        assert!(
            Some(again.issued_at_ns) < withdrawn.revoked_at_ns,
            "the new giving is stamped before the withdrawal"
        );
        assert_eq!(again.giving, 2);
        assert!(
            again.replaces(&withdrawn) && !withdrawn.replaces(&again),
            "work given after a withdrawal stands over it, whatever its stamp"
        );
        again.verify("w", 10 * DAY + 600)?;

        let short = signed(&behind, [8; 32], (9 * DAY + 600, 9 * DAY + 700))
            .given_after(&behind, &withdrawn)?;
        assert!(short.replaces(&withdrawn));
        assert_eq!(
            short.verify("w", 10 * DAY + 600),
            Err(WorkDelegationError::NotCurrent),
            "a window that is past by the reader's clock takes no work"
        );

        let withdrawn_again = again.revoked(&behind, 9 * DAY + 800)?;
        assert!(
            withdrawn_again.replaces(&again)
                && withdrawn_again.replaces(&withdrawn)
                && withdrawn_again.replaces(&first),
            "its withdrawal stands over the giving and everything before it"
        );
        assert_eq!(
            withdrawn_again.verify("w", 10 * DAY + 600),
            Err(WorkDelegationError::NotCurrent)
        );
        Ok(())
    }

    /// A withdrawal carries its cut under its member's signature. It keeps the
    /// server's writes up to the number it names for a document and the very
    /// compactions it names, and nothing else: no other document, no other
    /// compaction, no number above. A cut that is raised, added to or put on a
    /// live form is no statement of the member's.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_withdrawal_keeps_what_its_cut_names_and_nothing_else() -> TestResult {
        let key = SigningKey::from_bytes(&[7; 32]);
        let live = delegation(&key, [8; 32], 1_000);
        let (taken, other) = ([1; 32], [2; 32]);
        let cut = vec![
            HeldWork {
                route: "folded".into(),
                last: 0,
                compactions: vec![taken],
            },
            HeldWork {
                route: "edited".into(),
                last: 3,
                compactions: Vec::new(),
            },
        ];
        let withdrawn = live.withdrawn(&key, 600, cut)?;
        withdrawn.verify_signature("w")?;
        let routes: Vec<&str> = withdrawn
            .held
            .iter()
            .map(|held| held.route.as_str())
            .collect();
        assert_eq!(routes, ["edited", "folded"], "one order on every device");

        for (sequence, kept) in [(0, false), (1, true), (3, true), (4, false)] {
            assert_eq!(
                withdrawn.keeps("edited", sequence, None),
                kept,
                "{sequence}"
            );
        }
        // A compaction is kept as the very one the cut names, whatever number
        // it carries, and names its own document alone.
        assert!(withdrawn.keeps("folded", 9, Some(&taken)));
        assert!(!withdrawn.keeps("folded", 9, Some(&other)));
        assert!(!withdrawn.keeps("edited", 3, Some(&taken)));
        assert!(!withdrawn.keeps("folded", 1, None));
        // A document the cut does not name keeps nothing, and neither does a
        // withdrawal that names none or a form that is no withdrawal.
        assert!(!withdrawn.keeps("another", 1, None));
        assert!(!live.revoked(&key, 600)?.keeps("edited", 1, None));
        assert!(!live.keeps("edited", 1, None));

        // The signed bytes bind the cut: raised or added to, it is not the
        // member's statement.
        let mut raised = withdrawn.clone();
        raised.held[0].last = 4;
        assert_eq!(
            raised.verify_signature("w"),
            Err(WorkDelegationError::Signature),
            "a raised cut does not verify"
        );
        let mut added = withdrawn.clone();
        added.held.push(HeldWork {
            route: "zz-another".into(),
            last: 1,
            compactions: Vec::new(),
        });
        assert_eq!(
            added.verify_signature("w"),
            Err(WorkDelegationError::Signature),
            "a cut with a document added does not verify"
        );
        let mut emptied = withdrawn.clone();
        emptied.held.clear();
        assert_eq!(
            emptied.verify_signature("w"),
            Err(WorkDelegationError::Signature)
        );
        // Only a withdrawal carries a cut, each document once, and only the
        // device that signed the form withdraws it.
        let mut carrying = live.clone();
        carrying.held.clone_from(&withdrawn.held);
        assert_eq!(
            carrying.verify_signature("w"),
            Err(WorkDelegationError::Invalid("cut"))
        );
        let twice = vec![withdrawn.held[0].clone(), withdrawn.held[0].clone()];
        assert_eq!(
            live.withdrawn(&key, 600, twice),
            Err(WorkDelegationError::Invalid("cut"))
        );
        assert_eq!(
            live.withdrawn(&SigningKey::from_bytes(&[6; 32]), 600, Vec::new()),
            Err(WorkDelegationError::Invalid(
                "only the device that signed it revokes it"
            ))
        );
        Ok(())
    }
}
