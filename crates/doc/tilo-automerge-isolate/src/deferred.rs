//! A copy of a document that admits changes at once and applies them only
//! where it is read.

use std::collections::HashSet;

use automerge::{AutoCommit, ChangeHash};

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
    /// The hash of each change in `pending`.
    hashes: HashSet<ChangeHash>,
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
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.hashes.clear();
            self.heads.clear();
            if std::mem::take(&mut self.unchecked) {
                // What waits unapplied is applied in a worker process first, as any untrusted
                // change is, and only then here.
                let applied = Isolate::shared().apply(
                    &mut self.doc,
                    &pending,
                    Dependencies::RequireComplete,
                )?;
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
        self.hashes.contains(hash) || self.doc.get_change_meta_by_hash(hash).is_some()
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
    /// # Errors
    ///
    /// A [`Rejected`] reason; the document is then unchanged.
    pub fn admit_unapplied(&mut self, incoming: &[u8]) -> Result<Applied, Rejected> {
        let Some((after, carried)) = self.heads_after(incoming) else {
            return self.admit(incoming, Dependencies::RequireComplete);
        };
        crate::precheck::check(incoming, Isolate::shared().limits())?;
        self.pending.extend_from_slice(incoming);
        self.hashes.extend(carried);
        self.heads = after;
        self.unchecked = true;
        Ok(Applied::Changed(true))
    }

    /// The heads after `incoming` and the hash of each change it carries, when it is a sequence of new changes that build
    /// only on what the document holds and the document waits on nothing:
    /// the changes that stand at its heads now or arrive, less every change
    /// one of them builds on.
    fn heads_after(&mut self, incoming: &[u8]) -> Option<(Vec<ChangeHash>, Vec<ChangeHash>)> {
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
        let carried: Vec<ChangeHash> = changes.iter().map(automerge::Change::hash).collect();
        let mut heads = self.heads();
        heads.extend(carried.iter().copied());
        heads.retain(|head| !below.contains(head));
        Some((heads, carried))
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
        #[cfg(target_os = "linux")]
        if let Some((after, carried)) = deferred.heads_after(incoming) {
            crate::precheck::check(incoming, self.limits())?;
            let before = engine::Key::complete(&deferred.heads());
            let (applied, worker_at) =
                self.pool.check(&before, deferred, incoming, dependencies)?;
            if let Applied::Changed(changed) = applied {
                deferred.pending.extend_from_slice(incoming);
                deferred.hashes.extend(carried);
                deferred.heads = after;
                self.pool
                    .remember(engine::Key::complete(&deferred.heads), worker_at);
                return Ok(Applied::Changed(changed));
            }
            return Ok(applied);
        }
        self.apply(deferred.settled()?, incoming, dependencies)
    }
}

#[cfg(target_os = "linux")]
impl crate::pool::Base for Deferred {
    fn base(&mut self) -> Result<&mut AutoCommit, Rejected> {
        self.settled()
    }
}
