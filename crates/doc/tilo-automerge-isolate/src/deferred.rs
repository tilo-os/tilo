//! A copy of a document that admits changes at once and applies them only
//! where it is read.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, Change, ChangeHash};

use crate::{Applied, Dependencies, Isolate, Rejected, engine};

/// A document kept beside the one that writes it, such as a server's copy of
/// what an app's worker holds. Each change handed to it is admitted as
/// [`Isolate::apply`] admits one, applied in a worker process with its
/// limits, but the copy itself applies it only once something reads the
/// document ([`Self::settled`]): a writer whose copy is read less often than
/// written pays each change's admission, not its application here as well.
///
/// What it answers without applying anything is what the changes' own
/// headers say: the heads it stands at ([`Self::heads`]) and whether it holds
/// a change ([`Self::holds`]).
///
/// A writer that made its changes on this very state, such as an app's worker
/// whose copy this is, hands them over unapplied ([`Self::admit_unapplied`]):
/// a worker process applies them, as any untrusted change is admitted, only
/// once something reads the document, so a writer the copy is never read
/// between pays neither application of its changes.
#[derive(Clone, Default)]
pub struct Deferred {
    doc: AutoCommit,
    /// Changes admitted and not yet applied to `doc`, in the order admitted.
    pending: Vec<u8>,
    /// Some of `pending` was admitted unapplied ([`Self::admit_unapplied`]): no worker process
    /// applied it yet.
    unchecked: bool,
    /// The changes of `pending` by hash, as their admission parsed them: what reads a change
    /// by its own operations takes them from here rather than parsing them again
    /// ([`Self::admitted`]).
    parsed: HashMap<ChangeHash, Change>,
    /// The heads with `pending` applied, while any wait.
    heads: Vec<ChangeHash>,
}

impl From<AutoCommit> for Deferred {
    fn from(doc: AutoCommit) -> Self {
        Self {
            doc,
            ..Self::default()
        }
    }
}

impl Deferred {
    /// The document with every change admitted to it applied.
    ///
    /// # Errors
    ///
    /// The admitted changes did not apply here as they did in the worker
    /// that admitted them; the document is then as it was before them.
    pub fn settled(&mut self) -> Result<&mut AutoCommit, Rejected> {
        self.settle_in(Isolate::shared())
    }

    /// [`Self::settled`], what waits unapplied applied in `isolate`'s workers.
    fn settle_in(&mut self, isolate: &Isolate) -> Result<&mut AutoCommit, Rejected> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.parsed.clear();
            self.heads.clear();
            if std::mem::take(&mut self.unchecked) {
                // What waits unapplied is applied in a worker process first, as any untrusted
                // change is, and only then here.
                let applied =
                    isolate.apply(&mut self.doc, &pending, Dependencies::RequireComplete)?;
                if applied == Applied::MissingDependencies {
                    return Err(Rejected::Invalid(
                        "changes admitted unapplied build on what the document lacks".into(),
                    ));
                }
            } else {
                // A worker applied exactly these bytes to exactly this state,
                // which is what admitted them.
                engine::apply_trusted(&mut self.doc, &pending, Dependencies::RequireComplete)?;
            }
        }
        Ok(&mut self.doc)
    }

    /// The document as it stands before the changes still to be applied,
    /// and those changes, in the order admitted: what reads a change by its
    /// own operations needs no more of the document than what it builds on.
    #[must_use]
    pub fn unsettled(&self) -> (&AutoCommit, &[u8]) {
        (&self.doc, &self.pending)
    }

    /// The changes `hashes` name, in that order, as they were parsed when they were admitted:
    /// `None` unless each still waits to be applied. A list of a few hundred changes costs a
    /// tenth of a second to parse, and what reads what they write needs them parsed.
    #[must_use]
    pub fn admitted(&self, hashes: &[ChangeHash]) -> Option<Vec<&Change>> {
        hashes.iter().map(|hash| self.parsed.get(hash)).collect()
    }

    /// Every change admitted and not yet applied, as its admission parsed it: what a reader of
    /// what they write takes when it holds no change's hash to ask by.
    #[must_use]
    pub fn pending_changes(&self) -> Vec<&Change> {
        self.parsed.values().collect()
    }

    /// The heads the document stands at, every admitted change counted.
    pub fn heads(&mut self) -> Vec<ChangeHash> {
        if self.pending.is_empty() {
            self.doc.get_heads()
        } else {
            self.heads.clone()
        }
    }

    /// Whether the document holds the change `hash` names, applied or not.
    pub fn holds(&mut self, hash: &ChangeHash) -> bool {
        self.parsed.contains_key(hash) || self.doc.get_change_meta_by_hash(hash).is_some()
    }

    /// Admit `incoming` as [`Isolate::apply`] does. Changes that are all new
    /// and build only on what the document holds wait here once a worker
    /// applied them; anything else (a document, a change already held, one
    /// whose dependencies have not arrived) is applied at once, after what
    /// waits.
    ///
    /// # Errors
    ///
    /// A [`Rejected`] reason; the document is then unchanged.
    pub fn admit(
        &mut self,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        Isolate::shared().admit(self, incoming, dependencies)
    }

    /// Admit `incoming`, changes made on this document's own state by the writer it is kept
    /// beside, without applying them anywhere yet: their framing is checked and they must all be
    /// new and build only on what the document holds; a worker process applies them, and then
    /// this copy, once something reads the document ([`Self::settled`]). Anything else is
    /// admitted as [`Self::admit`] admits it.
    ///
    /// Whether the writer's changes are taken depends on them alone: another writer's change
    /// waiting here for what it builds on (it arrived first) is no reason to refuse them. They
    /// are [`Applied::MissingDependencies`] only when one of them builds on what the document
    /// lacks, and the document is then as it was.
    ///
    /// # Errors
    ///
    /// A [`Rejected`] reason; the document is then unchanged.
    pub fn admit_unapplied(&mut self, incoming: &[u8]) -> Result<Applied, Rejected> {
        let Some((after, carried)) = self.heads_after(incoming) else {
            return self.admit_own(incoming);
        };
        crate::precheck::check(incoming, Isolate::shared().limits())?;
        self.pending.extend_from_slice(incoming);
        self.keep_parsed(carried);
        self.heads = after;
        self.unchecked = true;
        Ok(Applied::Changed(true))
    }

    /// [`Self::admit_unapplied`] of what cannot wait unapplied: applied at once, keeping any
    /// other writer's change that waits for its base, and taken when every change of `incoming`
    /// is in the document after it. Changes that do not name themselves (a whole document) are
    /// admitted complete or not at all.
    fn admit_own(&mut self, incoming: &[u8]) -> Result<Applied, Rejected> {
        let Some(own) = engine::change_hashes(incoming) else {
            return self.admit(incoming, Dependencies::RequireComplete);
        };
        // Tried on a copy: a change of the writer's that builds on what is not here must not
        // wait in the document, which would then hold what its writer was refused.
        let mut candidate = self.clone();
        match candidate.admit(incoming, Dependencies::RetainOrphans)? {
            Applied::Changed(changed) if own.iter().all(|hash| candidate.holds(hash)) => {
                *self = candidate;
                Ok(Applied::Changed(changed))
            }
            _ => Ok(Applied::MissingDependencies),
        }
    }

    /// Keep `carried`, changes just added to `pending`, as they were parsed.
    fn keep_parsed(&mut self, carried: Vec<Change>) {
        for change in carried {
            self.parsed.insert(change.hash(), change);
        }
    }

    /// The heads after `incoming` and the changes it carries, parsed, when it is a sequence of
    /// new changes that build only on what the document holds and the document waits on
    /// nothing: the changes that stand at its heads now or arrive, less every change one of
    /// them builds on.
    fn heads_after(&mut self, incoming: &[u8]) -> Option<(Vec<ChangeHash>, Vec<Change>)> {
        if self.pending.is_empty() && !self.doc.get_missing_deps(&[]).is_empty() {
            return None;
        }
        let changes = engine::changes(incoming)?;
        let mut arrived: HashSet<ChangeHash> = HashSet::with_capacity(changes.len());
        let mut below: HashSet<ChangeHash> = HashSet::new();
        for change in &changes {
            let hash = change.hash();
            if self.holds(&hash) || !arrived.insert(hash) {
                return None;
            }
            for dependency in change.deps() {
                if !arrived.contains(dependency) && !self.holds(dependency) {
                    return None;
                }
                below.insert(*dependency);
            }
        }
        let mut heads = self.heads();
        heads.extend(changes.iter().map(Change::hash));
        heads.retain(|head| !below.contains(head));
        Some((heads, changes))
    }
}

impl Isolate {
    /// [`Deferred::admit`] under these limits.
    ///
    /// # Errors
    ///
    /// A [`Rejected`] reason; the document is then unchanged.
    pub fn admit(
        &self,
        deferred: &mut Deferred,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        // What waits unapplied is applied first: a worker that does not hold the copy's state
        // is handed it from the document settled ([`crate::pool::Base`]) while the worker is
        // held, and settling then would ask a worker of this pool again, maybe that very one,
        // which waited for itself for good and held every write of the copy behind it.
        if deferred.unchecked {
            deferred.settle_in(self)?;
        }
        #[cfg(target_os = "linux")]
        if let Some((after, carried)) = deferred.heads_after(incoming) {
            crate::precheck::check(incoming, self.limits())?;
            let before = engine::Key::complete(&deferred.heads());
            let (applied, worker_at) =
                self.pool.check(&before, deferred, incoming, dependencies)?;
            if let Applied::Changed(changed) = applied {
                deferred.pending.extend_from_slice(incoming);
                deferred.keep_parsed(carried);
                deferred.heads = after;
                self.pool
                    .remember(engine::Key::complete(&deferred.heads), worker_at);
                return Ok(Applied::Changed(changed));
            }
            return Ok(applied);
        }
        self.apply(deferred.settle_in(self)?, incoming, dependencies)
    }
}

#[cfg(target_os = "linux")]
impl crate::pool::Base for Deferred {
    fn base(&mut self) -> Result<&mut AutoCommit, Rejected> {
        self.settled()
    }
}
