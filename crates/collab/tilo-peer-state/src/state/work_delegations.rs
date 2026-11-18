//! A member's signed delegation of their own work to a server device.
//!
//! Work in a workspace is the workspace's or a member's. A key holder takes the
//! workspace's work itself; a member's work — an agent using their mail, a
//! Browser they are signed in to, an outbox entry that needs their password —
//! is taken by a server of another person only because that member delegated
//! it. The delegation is a resource of the workspace's own state
//! ([`collab_types::work_delegation::WorkDelegation`]): the member's device
//! signs it, names the server, this workspace, the work by the grant paths the
//! workspace decides actions by and until when, and every holder replays the
//! same rule. A revocation is the same delegation signed again with the revocation
//! set, so it stops the work on every holder at once and needs no one to
//! delete a row.
//!
//! A row is whatever its writers put there, and both forms of a delegation
//! verify. So what a row holds is decided by what the member's own devices
//! signed, never by who wrote the row or when
//! ([`collab_types::work_delegation::WorkDelegation::replaces`]): of every form
//! a row carries, the one signed by a device of the member it names that
//! stands over the others. No one moves a row back to an earlier form or
//! erases it: the member's own verbs refuse to, and every receiver refuses a
//! batch or a base that does ([`rows_kept`]), the owner's devices' too. A
//! device that took a withdrawal in therefore keeps it in its own copy of the
//! workspace, and a device that never saw one is held by the delegation's
//! end, seven days at most, which the member's devices move on by signing the
//! delegation again ([`PeerWorkspaceMut::renew_work_delegations`]).
//!
//! The workspace's document starts over from a base whenever someone is
//! admitted, and a base may be made before a withdrawal reached its maker. A
//! copy that knows its device ([`crate::StatePeer::write_as_device`]) adopts
//! such a base and writes the form it holds back in the same step
//! ([`carried`]); a copy that knows none refuses it until a base holds the
//! form.
//!
//! A key holder that takes a member's work under a live delegation signs what
//! it writes with its own device key; every receiver admits the change as the
//! member's, bounded by the delegation's paths
//! ([`crate::StatePeer::authorize_document`]). A key holder without one is
//! refused, as any device holding no member's authority is.
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{ROOT, SystemTime, now_ns};
use crate::cached_document::PolicyData;
use crate::state::copies::CopyRole;
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ReadDoc as _, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use collab_types::work_delegation::{MAX_LIFETIME_NS, WORK_DELEGATIONS_KEY, WorkDelegation};
use std::collections::BTreeMap;

/// What a batch that takes a delegation out of its row is refused with.
const ERASED: &str = "a member's work delegation cannot be erased";
/// What a batch that puts an earlier form of a delegation back is refused with.
const MOVED_BACK: &str = "a member's work delegation cannot be moved back to an earlier form";
/// What a member's device is told that writes a form the row is already past.
const NOT_THE_LATEST: &str =
    "a later form of this delegation is held: it was withdrawn or signed again since";
/// A member's device signs a delegation of its person's again once half its
/// longest life or less is left of it.
const RENEWED_WITHIN_NS: u64 = MAX_LIFETIME_NS / 2;

/// The workspace document's id, as its founder wrote it.
fn workspace_of(doc: &AutoCommit) -> Option<String> {
    match doc.get(ROOT, "workspace").ok()??.0 {
        Value::Scalar(value) => value.as_str().map(str::to_owned),
        Value::Object(_) => None,
    }
}

/// The map of delegation rows `doc` was founded with.
fn rows_of(doc: &AutoCommit) -> Option<automerge::ObjId> {
    match doc.get(ROOT, WORK_DELEGATIONS_KEY) {
        Ok(Some((Value::Object(ObjType::Map), rows))) => Some(rows),
        _ => None,
    }
}

/// Every text each delegation row of `doc` holds, in one order on every
/// device. Two devices that write one row at once leave two values in it, and
/// a merge shows one of them as the row: the other may be the form that
/// stands, so all of them are read.
fn texts_of(doc: &AutoCommit) -> BTreeMap<String, Vec<String>> {
    let Some(rows) = rows_of(doc) else {
        return BTreeMap::new();
    };
    doc.keys(&rows)
        .map(|row| {
            let Ok(values) = doc.get_all(&rows, row.as_str()) else {
                return (row, Vec::new());
            };
            let mut texts: Vec<String> = values
                .into_iter()
                .filter_map(|(value, _)| match value {
                    Value::Scalar(value) => match value.as_ref() {
                        ScalarValue::Str(text) => Some(text.to_string()),
                        _ => None,
                    },
                    Value::Object(_) => None,
                })
                .collect();
            texts.sort();
            (row, texts)
        })
        .collect()
}

/// The person the workspace's own device rows certify `device` for, whether
/// or not the device is still one of theirs: a withdrawal stays its member's
/// when the device that signed it is removed. A device the workspace holds no
/// verified certificate of is nobody's.
fn certified_person(policy: &PolicyData, device: &[u8; 32]) -> Option<[u8; 32]> {
    let bytes = policy.identities.get(&hex::encode(device))?;
    let identity = tilo_failure::ignored!(
        "a device row that does not decode certifies its device for nobody",
        crate::state::GovernedIdentity::decode(bytes).ok()
    )?;
    tilo_failure::ignored!(
        "a certificate that does not verify certifies its device for nobody",
        tilo_user_identity::verify_device_cert(&identity.device_cert).ok()
    )?;
    Some(identity.device_cert.user_public_key)
}

/// The form each row holds: of the forms in `texts` that are signed for
/// `workspace`, sit in the row their server and member name and are signed by
/// a device `person_of` certifies for that very member, the one that stands
/// over the others. Whoever wrote a row, only the member's own devices say
/// what it holds: a form another person's device signed in their name is no
/// form of theirs.
fn forms_held(
    texts: &BTreeMap<String, Vec<String>>,
    workspace: &str,
    person_of: &dyn Fn(&[u8; 32]) -> Option<[u8; 32]>,
) -> BTreeMap<String, WorkDelegation> {
    texts
        .iter()
        .filter_map(|(row, texts)| {
            let held = texts
                .iter()
                .filter_map(|text| {
                    tilo_failure::ignored!(
                        "a row another build or another writer filled is no delegation",
                        serde_json::from_str::<WorkDelegation>(text).ok()
                    )
                })
                .filter(|form| {
                    form.row_key() == *row
                        && form.verify_signature(workspace).is_ok()
                        && person_of(&form.device) == Some(form.person)
                })
                .reduce(|held, form| if form.replaces(&held) { form } else { held })?;
            Some((row.clone(), held))
        })
        .collect()
}

/// The work delegations `doc` holds, one per row, ordered by row key: each
/// row's form as [`forms_held`] reads it. A row that carries no form of its
/// member's own devices is left out.
#[must_use]
pub fn work_delegations_of(doc: &AutoCommit) -> Vec<WorkDelegation> {
    let texts = texts_of(doc);
    // Most workspaces hold no delegation: nothing to read.
    if texts.values().all(Vec::is_empty) {
        return Vec::new();
    }
    let Some(workspace) = workspace_of(doc) else {
        return Vec::new();
    };
    let policy = crate::workspace::policy::read(doc);
    forms_held(&texts, &workspace, &|device: &[u8; 32]| {
        certified_person(&policy, device)
    })
    .into_values()
    .collect()
}

/// Whether `after` keeps every delegation row of `before`: at the form it
/// held, or at one that stands over it. A row erased, emptied of its member's
/// forms, or put back to an earlier form is refused, whoever wrote the batch
/// and whether it is a batch or a base: a withdrawal a device took in stays in
/// its copy. Whose device signed a form is read from the device rows of either
/// document, so removing a member does not read as erasing their rows.
///
/// # Errors
///
/// What the batch is refused with.
pub(crate) fn rows_kept(before: Option<&AutoCommit>, after: &AutoCommit) -> Result<(), String> {
    let Some(before) = before else {
        return Ok(());
    };
    let held = texts_of(before);
    // Most workspaces hold no delegation, and most batches change none.
    if held.values().all(Vec::is_empty) {
        return Ok(());
    }
    let next = texts_of(after);
    if held == next {
        return Ok(());
    }
    let Some(workspace) = workspace_of(before) else {
        return Ok(());
    };
    let (was, is) = (
        crate::workspace::policy::read(before),
        crate::workspace::policy::read(after),
    );
    let person_of = |device: &[u8; 32]| {
        certified_person(&was, device).or_else(|| certified_person(&is, device))
    };
    let next = forms_held(&next, &workspace, &person_of);
    for (row, held) in forms_held(&held, &workspace, &person_of) {
        match next.get(&row) {
            None => return Err(ERASED.into()),
            Some(form) if held.replaces(form) => return Err(MOVED_BACK.into()),
            Some(_) => {}
        }
    }
    Ok(())
}

/// Put, as `actor`, the form each row holds into every row of `doc` where two
/// writers left two values. A base keeps one value of a row, the one a merge
/// shows, and a base that left the form that stands behind would move the row
/// back for everyone who adopts it.
///
/// # Errors
///
/// The row cannot be written.
pub(super) fn settle(
    doc: &mut AutoCommit,
    actor: &automerge::ActorId,
) -> Result<(), automerge::AutomergeError> {
    let texts = texts_of(doc);
    if texts.values().all(|texts| texts.len() < 2) {
        return Ok(());
    }
    let (Some(rows), Some(workspace)) = (rows_of(doc), workspace_of(doc)) else {
        return Ok(());
    };
    let policy = crate::workspace::policy::read(doc);
    let held = forms_held(&texts, &workspace, &|device: &[u8; 32]| {
        certified_person(&policy, device)
    });
    doc.set_actor(actor.clone());
    for (row, form) in held {
        if texts.get(&row).is_some_and(|texts| texts.len() > 1)
            && let Ok(text) = serde_json::to_string(&form)
        {
            doc.put(&rows, row, text)?;
        }
    }
    doc.commit();
    Ok(())
}

/// Write, as `actor`, into `after` every form `before` holds that `after`
/// lacks or holds further back, each into its row: how a copy that adopts a
/// base made before a withdrawal reached its maker keeps what it saw. A form
/// is its member's own signed statement, so the device that hands it on
/// gives it no authority it had not. Returns how many rows it wrote.
///
/// # Errors
///
/// `after` was founded without the rows, or a row cannot be written.
pub(super) fn carried(
    before: &AutoCommit,
    after: &mut AutoCommit,
    actor: &automerge::ActorId,
) -> Result<usize, String> {
    let held = texts_of(before);
    // Most workspaces hold no delegation: nothing to keep.
    if held.values().all(Vec::is_empty) {
        return Ok(0);
    }
    let Some(workspace) = workspace_of(before) else {
        return Ok(0);
    };
    let (was, is) = (
        crate::workspace::policy::read(before),
        crate::workspace::policy::read(after),
    );
    let person_of = |device: &[u8; 32]| {
        certified_person(&was, device).or_else(|| certified_person(&is, device))
    };
    let next = forms_held(&texts_of(after), &workspace, &person_of);
    let lacking: Vec<(String, WorkDelegation)> = forms_held(&held, &workspace, &person_of)
        .into_iter()
        .filter(|(row, held)| next.get(row).is_none_or(|form| held.replaces(form)))
        .collect();
    if lacking.is_empty() {
        return Ok(0);
    }
    let rows = rows_of(after).ok_or("the base was founded without the workspace's delegations")?;
    after.set_actor(actor.clone());
    for (row, form) in &lacking {
        let text = serde_json::to_string(form).map_err(|error| error.to_string())?;
        after
            .put(&rows, row.as_str(), text)
            .map_err(|error| error.to_string())?;
    }
    after.commit();
    Ok(lacking.len())
}

/// What a member's work on a server is at one instant: what a turn that
/// cannot take the work tells its person, so that an assistant never stops
/// without a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DelegatedWork {
    /// A delegation is live: the server takes the work under it.
    Live(Box<WorkDelegation>),
    /// The delegation came to its end at `at_ns`, and no device of the
    /// member's has signed it again since.
    Ended {
        /// When it ended, ns since the epoch.
        at_ns: u64,
    },
    /// The member withdrew it at `at_ns`.
    Withdrawn {
        /// When they withdrew it, ns since the epoch.
        at_ns: u64,
    },
    /// A delegation is held, neither withdrawn nor at its end, and the server
    /// takes nothing under it: the device that signed it is no longer the
    /// member's, or the server no longer keeps the workspace's keys.
    Stopped,
    /// The member gave this server no work.
    NotGiven,
}

impl PeerWorkspace<'_> {
    /// The work delegations the workspace holds, live or not, each as the
    /// member's device signed it: what every member reads of the work they
    /// gave a server, beside who holds the workspace.
    #[must_use]
    pub fn work_delegations(self) -> Vec<WorkDelegation> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map_or_else(Vec::new, work_delegations_of)
    }

    /// The delegation under which `server` takes `person`'s work at `now`, if
    /// one is live: the form their row holds, signed for this workspace,
    /// inside its window, not revoked, signed by a current certified device
    /// of that member, and for a server the workspace names among its
    /// key-holding holders. A server named by a delegation but no longer
    /// holding the keys takes nothing under it.
    #[must_use]
    pub fn work_delegation(
        self,
        server: &[u8; 32],
        person: &[u8; 32],
        now: SystemTime,
    ) -> Option<WorkDelegation> {
        let now_ns = now_ns(now);
        let workspace = self.peer.core.workspace.clone();
        let delegation = self
            .work_delegations()
            .into_iter()
            .find(|delegation| &delegation.server == server && &delegation.person == person)
            .filter(|delegation| delegation.verify(&workspace, now_ns).is_ok())?;
        // The signing device must be the member's own current device: the
        // delegation says whose it is, and the workspace's roster says whether
        // that is true.
        let cert = self.peer.certificate(&delegation.device, now).ok()?;
        if cert.user_public_key != delegation.person {
            return None;
        }
        self.peer.membership(delegation.device, now).ok()?;
        (self.peer.governance().holder_role(server) == Some(CopyRole::KeyHolder))
            .then_some(delegation)
    }

    /// What `person`'s work on `server` is at `now`: live, at its end,
    /// withdrawn, stopped or never given. A turn that cannot take the work
    /// says which, so its person can act on it.
    #[must_use]
    pub fn delegated_work(
        self,
        server: &[u8; 32],
        person: &[u8; 32],
        now: SystemTime,
    ) -> DelegatedWork {
        if let Some(live) = self.work_delegation(server, person, now) {
            return DelegatedWork::Live(Box::new(live));
        }
        let Some(held) = self
            .work_delegations()
            .into_iter()
            .find(|held| &held.server == server && &held.person == person)
        else {
            return DelegatedWork::NotGiven;
        };
        if let Some(at_ns) = held.revoked_at_ns {
            return DelegatedWork::Withdrawn { at_ns };
        }
        if now_ns(now) >= held.expires_at_ns {
            return DelegatedWork::Ended {
                at_ns: held.expires_at_ns,
            };
        }
        DelegatedWork::Stopped
    }

    /// The delegation under which `server` takes a member's work at `now`: the
    /// one with the latest issue time among the live ones.
    #[must_use]
    pub fn delegation_for_server(
        self,
        server: &[u8; 32],
        now: SystemTime,
    ) -> Option<WorkDelegation> {
        let now_ns = now_ns(now);
        let workspace = self.peer.core.workspace.clone();
        self.work_delegations()
            .into_iter()
            .filter(|delegation| &delegation.server == server)
            .filter(|delegation| delegation.verify(&workspace, now_ns).is_ok())
            .max_by_key(|delegation| delegation.issued_at_ns)
            .filter(|delegation| {
                self.work_delegation(server, &delegation.person, now)
                    .is_some()
            })
    }
}

impl PeerWorkspaceMut<'_> {
    /// Write `delegation` into the workspace, authored by the member's device
    /// `key` that signed it. Writing it as it is written changes nothing.
    ///
    /// # Errors
    /// It is not `key`'s device's of a current member, the delegation
    /// is not current, `server` is no key-holding holder of this workspace, a
    /// later form of it is held (it was withdrawn or signed again since), or
    /// the workspace cannot be written.
    pub fn author_work_delegation(
        mut self,
        delegation: &WorkDelegation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = self.peer.core.workspace.clone();
        // A revocation is the same delegation signed again, so what it must be
        // is its signature: one revoked now still names the delegation it
        // stops, and a member withdraws one that is past its end too, which a
        // device of theirs would otherwise sign again.
        delegation
            .verify_signature(&workspace)
            .map_err(|error| error.to_string())?;
        let at = now_ns(now);
        if delegation.revoked_at_ns.is_none()
            && (at < delegation.issued_at_ns || at >= delegation.expires_at_ns)
        {
            return Err("the delegation is not current".into());
        }
        if delegation.device != key.device_public_key() {
            return Err("a delegation is signed by the member's own device".into());
        }
        let cert = self
            .peer
            .certificate(&delegation.device, now)
            .map_err(|error| error.to_string())?;
        if cert.user_public_key != delegation.person {
            return Err("the signing device is not the member's".into());
        }
        self.peer.membership(delegation.device, now)?;
        if self.peer.governance().holder_role(&delegation.server) != Some(CopyRole::KeyHolder) {
            return Err("the delegation names no key holder of this workspace".into());
        }
        let held = self
            .peer
            .workspace()
            .work_delegations()
            .into_iter()
            .find(|held| held.row_key() == delegation.row_key());
        if held.as_ref() == Some(delegation) {
            return Ok(false);
        }
        // The member's own later statement stays: the form a delegation was
        // first signed in is not written over its withdrawal, by anyone.
        if held.is_some_and(|held| !delegation.replaces(&held)) {
            return Err(NOT_THE_LATEST.into());
        }
        let json = serde_json::to_string(delegation).map_err(|error| error.to_string())?;
        self.write_row(
            WORK_DELEGATIONS_KEY,
            &delegation.row_key(),
            &delegation.device,
            Some(&json),
        )?;
        Ok(true)
    }

    /// Revoke `server`'s delegation of `person`'s work at `now`, as that
    /// member's device `key`: from then on every holder refuses the work the
    /// server takes for them. Any device of the member's revokes it: one that
    /// did not sign the form held signs the withdrawal as its own. Returns
    /// whether a delegation was revoked; `false` when the member delegated
    /// nothing to it, or already revoked it.
    ///
    /// # Errors
    /// `key` is no device of the member's, or the workspace cannot be written.
    pub fn revoke_work_delegation(
        self,
        server: &[u8; 32],
        person: &[u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(delegation) = self
            .peer
            .workspace()
            .work_delegations()
            .into_iter()
            .find(|delegation| &delegation.server == server && &delegation.person == person)
        else {
            return Ok(false);
        };
        if delegation.revoked_at_ns.is_some() {
            return Ok(false);
        }
        let at = now_ns(now);
        let own = if delegation.device == key.device_public_key() && delegation.issued_at_ns <= at {
            delegation
        } else {
            // This device signs the giving as its own form, by its own clock,
            // and withdraws that: no withdrawal is stamped before the form it
            // is signed over, and the form held may be stamped ahead of this
            // device's clock. The number of the giving decides that the
            // withdrawal stands, not either stamp.
            let issued = delegation.issued_at_ns.min(at);
            let expires = delegation
                .expires_at_ns
                .min(issued.saturating_add(MAX_LIFETIME_NS));
            delegation
                .renewed(key, (issued, expires))
                .map_err(|error| error.to_string())?
        };
        let revoked = own.revoked(key, at).map_err(|error| error.to_string())?;
        self.author_work_delegation(&revoked, key, now)
    }

    /// Sign again, as the member's device `key`, every delegation of its
    /// person's that is not withdrawn and has half its longest life or less
    /// left, one past its end included: the same giving of the same work, to
    /// live seven days from `now`. A member's device does this unasked
    /// whenever it is there, so a delegation lasts while its member's devices
    /// come by and ends within seven days once they withdrew it somewhere this
    /// device has not seen. Returns how many it signed.
    ///
    /// # Errors
    /// `key` is no certified device of a member, or the workspace cannot be
    /// written.
    pub fn renew_work_delegations(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let device = key.device_public_key();
        let person = self
            .peer
            .certificate(&device, now)
            .map_err(|error| error.to_string())?
            .user_public_key;
        let at = now_ns(now);
        let due: Vec<WorkDelegation> = self
            .peer
            .workspace()
            .work_delegations()
            .into_iter()
            .filter(|held| held.person == person && held.revoked_at_ns.is_none())
            .filter(|held| {
                held.issued_at_ns <= at
                    && held.expires_at_ns.saturating_sub(at) <= RENEWED_WITHIN_NS
            })
            .filter(|held| {
                self.peer.governance().holder_role(&held.server) == Some(CopyRole::KeyHolder)
            })
            .collect();
        let mut signed = 0_usize;
        for held in due {
            let renewed = held
                .renewed(key, (at, at.saturating_add(MAX_LIFETIME_NS)))
                .map_err(|error| error.to_string())?;
            if self
                .peer
                .workspace_mut()
                .author_work_delegation(&renewed, key, now)?
            {
                signed = signed.saturating_add(1);
            }
        }
        Ok(signed)
    }
}
