//! The governance a write is judged by: the one in its causal past.
//!
//! A write names the heads of the governance its writer held
//! ([`SignedOp::governance`]), and every copy judges it by the governance at
//! those heads, whatever order the write and the governance's own changes
//! reach the copy in. Two copies holding the same changes then take in the
//! same writes.
//!
//! A change since that took the writer's standing away judges the write too,
//! unless its cut kept the write ([`crate::standing`]): a member who was
//! demoted or removed does not write as they were by building on governance
//! from before. A write the cut keeps is judged as it was written.
//!
//! A workspace starts its governance over from a base at each admission
//! ([`crate::workspace::base`]), so a copy may no longer hold the heads an
//! older write names. Such a write is judged by the base, with the standing a
//! cut kept of its writer when the cut went into the base: the governance
//! that is left of when it was written.
//!
//! A write that names heads this copy neither holds nor knows a base replaced
//! comes from governance this copy may never hold: another device's own base.
//! It is judged by the governance this copy holds, as one that names none is,
//! and waits for the governance it names only while that refuses it.
//!
//! A write that names no governance at all comes from a device that does not
//! hold it, such as a mounted folder. Nothing says when it was written, so it
//! is judged by the governance this copy holds, with the standing the earliest
//! cut that keeps it says its writer held then, and refused by every cut that
//! does not.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Mutex, OnceLock};

use automerge::ChangeHash;
use collab_types::SignedOp;
use tilo_lock::MutexRecover as _;

use super::{CachedDocument, StatePeer, backend::PeerCore, workspace};

/// What a write says of the governance it was written under.
#[derive(Clone, Copy)]
enum Written {
    /// Heads this copy holds.
    Named,
    /// Heads of the history the base this copy holds replaced.
    BeforeBase,
    /// Nothing: its writer, a device that does not hold the workspace's
    /// governance (a mounted folder, a relay), cannot say.
    Unnamed,
    /// Heads neither this copy nor the history its base replaced holds: on
    /// their way, or of a base another device made.
    Unresolved,
}

/// The governance a write is judged by.
pub(super) enum Under {
    /// The governance this copy holds now: the write names all of it.
    Now,
    /// An earlier one, or one a change since judges it by: this copy as it
    /// judges by that governance.
    Past(Box<StatePeer>),
    /// The governance it names is not here, and may never be: judged by the
    /// governance this copy holds ([`Under::Now`] when `None`), it waits for
    /// the governance it names while that refuses it.
    Unresolved(Option<Box<StatePeer>>),
    /// This copy holds no governance yet.
    Missing,
}

/// How many governance states the writes a copy takes in were judged by are
/// kept: forking a governance at a write's heads replays its changes, and the
/// writes of one stretch of history name the same few.
const FORKS_KEPT: usize = 16;

/// Governance states, each with the heads it was forked at, oldest first.
type Forks = Mutex<VecDeque<(Vec<ChangeHash>, CachedDocument)>>;

/// The governance of a workspace at the heads it is keyed by. A state is the
/// same whichever copy forked it, so every copy in the process shares them.
fn forks() -> &'static Forks {
    static FORKS: OnceLock<Forks> = OnceLock::new();
    FORKS.get_or_init(Mutex::default)
}

/// `doc` as it was at `heads`.
fn forked(doc: &CachedDocument, heads: &BTreeSet<ChangeHash>) -> Result<CachedDocument, String> {
    let key: Vec<ChangeHash> = heads.iter().copied().collect();
    let kept = forks()
        .lock_recover()
        .iter()
        .find(|(held, _)| *held == key)
        .map(|(_, fork)| fork.clone());
    if let Some(fork) = kept {
        return Ok(fork);
    }
    let fork: CachedDocument = automerge::AutoCommit::from_document(
        doc.committed_document()
            .fork_at(&key)
            .map_err(|error| error.to_string())?,
    )
    .into();
    {
        let mut forks = forks().lock_recover();
        if forks.len() == FORKS_KEPT {
            forks.pop_front();
        }
        forks.push_back((key, fork.clone()));
    }
    Ok(fork)
}

/// How a write that names the governance heads `named` was written, as `doc` holds them, and
/// the changes of `doc` the governance it was written under is the one at.
fn written_at(
    doc: &CachedDocument,
    named: &BTreeSet<[u8; 4]>,
    retained: bool,
) -> (Written, BTreeSet<ChangeHash>) {
    let changes = doc.changes_named();
    let mut heads = BTreeSet::new();
    let mut unknown = Vec::new();
    for head in named {
        match changes.by_head.get(head) {
            Some(found) => heads.extend(found.iter().copied()),
            None => unknown.push(*head),
        }
    }
    let written = if named.is_empty() {
        Written::Unnamed
    } else if unknown.is_empty() {
        Written::Named
    } else {
        // Heads this copy does not hold: of the history a base replaced,
        // or on their way.
        let replaced = workspace::base::replaced(doc);
        let based = workspace::base::generation(doc) > 0
            && (retained || unknown.iter().all(|head| replaced.contains(head)));
        if based {
            heads.extend(changes.roots.iter().copied());
            Written::BeforeBase
        } else {
            Written::Unresolved
        }
    };
    (written, heads)
}

impl StatePeer {
    /// The governance `write`, a write to `document` of `app`, is judged by:
    /// the one at the heads it names, with every change since that took its
    /// writer's standing away and does not keep it. `retained` for a write a
    /// holder hands on from what it kept: one that names heads this copy does
    /// not hold is then of the history a base replaced, never one still on
    /// its way.
    ///
    /// # Errors
    ///
    /// The cuts the workspace keeps do not read.
    pub(super) fn judged_under(
        &self,
        app: &str,
        document: &str,
        write: &SignedOp,
        retained: bool,
    ) -> Result<Under, String> {
        let Some(doc) = self.core.workspace_doc.as_ref() else {
            return Ok(Under::Missing);
        };
        let named: BTreeSet<[u8; 4]> = write.governance.iter().copied().collect();
        let current: BTreeSet<[u8; 4]> = automerge::transaction::Transactable::base_heads(&**doc)
            .iter()
            .map(|head| collab_types::governance_head(&head.0))
            .collect();
        if !named.is_empty() && named == current {
            return Ok(Under::Now);
        }
        let changes = doc.changes_named();
        let (written, mut heads) = written_at(doc, &named, retained);
        let author = write.author.as_bytes();
        let person = self
            .core
            .certificates
            .get(author)
            .map(|cert| cert.user_public_key);
        let compaction = (write.kind == collab_types::OpKind::Compaction)
            .then(|| self.write_id(app, document, write))
            .flatten();
        let mut judged = match written {
            Written::Unnamed | Written::Unresolved => doc.clone(),
            Written::Named | Written::BeforeBase => forked(doc, &heads)?,
        };
        let mut since = BTreeSet::new();
        // The standing the earliest cut that keeps the write says its writer
        // held, when the governance before that cut is not here to say it.
        let mut kept: Option<(u64, crate::standing::Standing)> = None;
        for subject in std::iter::once(*author).chain(person) {
            for cut in crate::standing::cuts(doc, &subject)? {
                let keeps = crate::standing::keeps(
                    doc,
                    &cut,
                    author,
                    (app, document),
                    write.seq,
                    compaction.as_ref(),
                )?;
                let known = cut
                    .written_in
                    .iter()
                    .any(|change| judged.has_change(change));
                let before_it = match written {
                    // Made beside the write or after it: it judges the write
                    // unless it kept it.
                    _ if !known => {
                        if !keeps {
                            since.extend(cut.written_in.iter().copied());
                        }
                        false
                    }
                    // Written knowing of the cut.
                    Written::Named => false,
                    // The cut went into the base the write is judged by.
                    Written::BeforeBase => cut
                        .written_in
                        .iter()
                        .all(|change| changes.roots.contains(change)),
                    Written::Unnamed | Written::Unresolved => true,
                };
                if before_it
                    && keeps
                    && Some(subject) == person
                    && kept.as_ref().is_none_or(|(order, _)| cut.order < *order)
                {
                    kept = Some((cut.order, cut.prior));
                }
            }
        }
        if !since.is_empty() {
            heads.extend(since);
            judged = forked(doc, &heads)?;
        }
        match (kept, person) {
            (Some((_, prior)), Some(person)) => {
                let policy = prior.held_in(
                    judged.workspace_policy(&self.core.workspace).clone(),
                    &hex::encode(person),
                );
                judged = judged.governing_by(policy);
            }
            _ if matches!(written, Written::Unnamed) => return Ok(Under::Now),
            _ if matches!(written, Written::Unresolved) => return Ok(Under::Unresolved(None)),
            _ => {}
        }
        let past = Box::new(self.judging_by(judged));
        Ok(match written {
            Written::Unresolved => Under::Unresolved(Some(past)),
            _ => Under::Past(past),
        })
    }

    /// This copy as it judges a write by the governance `judged`: everything
    /// it holds but the governance, which is `judged`, and what it has yet to
    /// take in, which a judgement does not read.
    fn judging_by(&self, judged: CachedDocument) -> Self {
        let workspace = self.core.workspace.clone();
        let owner =
            super::receiving::governed_owner(&judged, &workspace).unwrap_or(self.core.owner);
        Self {
            core: PeerCore {
                workspace,
                owner,
                origin: self.core.origin,
                certificates: self.core.certificates.clone(),
                revocations: self.core.revocations.clone(),
                receipts: self.core.receipts.clone(),
                workspace_doc: Some(judged),
                documents: self.core.documents.clone(),
                pending: VecDeque::new(),
                pending_bytes: 0,
                exact_admissions: crate::admission::ExactAdmissions::default(),
                exact_selected: false,
                native_cut_publisher: None,
            },
            surfaces: self.surfaces.clone(),
            app_surfaces: self.app_surfaces.clone(),
            missed: super::held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: self.workspace_host,
            own_device: self.own_device,
            governance_now: self
                .governance_now
                .clone()
                .or_else(|| self.core.workspace_doc.clone().map(Box::new)),
        }
    }

    /// The governance this copy holds now, whether this is the copy itself or
    /// the copy as it judges a write by an earlier governance.
    pub(super) fn governance_held_now(&self) -> Option<&CachedDocument> {
        self.governance_now
            .as_deref()
            .or(self.core.workspace_doc.as_ref())
    }
}
