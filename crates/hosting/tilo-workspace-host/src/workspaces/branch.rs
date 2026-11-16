//! Branching a workspace this host keeps for a person
//! ([`tilo_relay_session::workspaces::BRANCH_OWN_WORKSPACE`],
//! `docs/architecture/workspace.md`, "Branching a workspace"): a new workspace of
//! theirs that starts with everything the source holds and goes on apart from it.
//!
//! A workspace is no file to copy: its retained frames name it, its sealed records
//! are bound to its identity and its channels' keys, and its governance is signed
//! for it. The host copies what it reads instead, as a device that branches a
//! workspace does, since it is a member of every channel of the workspaces it
//! governs:
//!
//! 1. Every record of the source is read whole, with its history, but what stays
//!    behind ([`tilo_desktop_model::branch::stays`]: the conversations in Chat,
//!    each person's place in the tilolang apps, Terminal's environments, the
//!    desktop's layout of everyone's) and a person's own records sealed for their
//!    devices alone, which are not the host's to read. A record that does not
//!    read whole stops the branch before anything is made.
//! 2. The branch is made as any workspace of the person's own is: governed here,
//!    its governance new, the person its only member and its administrator, its
//!    channels with keys of their own the first time a record of it is sealed.
//! 3. Each record is forked into it (`ui::record::transfer::branch`: its whole
//!    history under a writer of its own, one change naming the branch) and founded
//!    there as the governor founds a record again (`sealed_records::refound`):
//!    sealed under the branch's own keys where its records are sealed.
//! 4. The content those records name is copied into the branch's content by what
//!    keeps it ([`BranchContent`]).
//! 5. The source's Linux machine is copied into the branch's by what runs the
//!    workspaces' machines ([`WorkspaceMachines`](super::WorkspaceMachines)): a
//!    source without one branches without one.
//!
//! All or nothing: a branch that could not take a record, its content or its
//! machine is deleted again, and the refusal names what did not come. It is the person's, and listed
//! among their workspaces, only once it holds everything. Whoever keeps more of a
//! workspace than its records hears of each branch made ([`Workspaces::branches`]).

use std::sync::Arc;

use super::Workspaces;
use tilo_lock::RwLockRecover as _;

/// A branch this host made: its source, the branch, and whose both are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branched {
    /// The workspace branched, by its id.
    pub source: String,
    /// The branch, by its id.
    pub branch: String,
    /// The person whose own both are (their root key).
    pub person: [u8; 32],
}

/// What keeps the content of the workspaces this host governs, as it copies a
/// source's content into a branch of it.
pub trait BranchContent: Send + Sync {
    /// Copy into `branch` every item the workspace `source` keeps that `records`
    /// (the branch's, by app and id, each saved with its whole history) name,
    /// with what each is made of. An item `source` does not keep is not copied:
    /// the source could not answer it either.
    ///
    /// # Errors
    ///
    /// An item `source` keeps could not be copied, named with why.
    fn copy(
        &self,
        source: &str,
        branch: &str,
        records: &[((String, String), Vec<u8>)],
    ) -> Result<(), String>;

    /// Let go of what was copied into `branch`, a branch that did not take
    /// everything and is discarded: nothing of it is kept.
    ///
    /// # Errors
    ///
    /// What was copied stays, named with why.
    fn discard(&self, branch: &str) -> Result<(), String> {
        let _ = branch;
        Ok(())
    }
}

impl Workspaces {
    /// Each branch made here, as it is made: what keeps more of a workspace than
    /// its records (the machine a workspace runs on) reacts to it.
    #[must_use]
    pub fn branches(&self) -> tokio::sync::broadcast::Receiver<Branched> {
        self.inner.branches.subscribe()
    }

    /// Have `content` copy a source's content into each branch made here.
    pub fn branch_content(&self, content: Arc<dyn BranchContent>) {
        *self.inner.branch_content.write_recover() = Some(content);
    }
}

#[cfg(feature = "agents")]
mod copy {
    use std::collections::BTreeSet;

    use automerge::AutoCommit;

    use super::{Branched, Workspaces};
    use crate::workspaces::{CollaborationError, WorkspaceSummary, sealed_records};
    use tilo_lock::RwLockRecover as _;

    /// A record by its app and id.
    type Route = (String, String);

    /// What a branch could not take: the part of its source, in words the
    /// person reads, and why, which the host's log keeps.
    pub(in crate::workspaces) struct NotMade {
        pub(in crate::workspaces) part: String,
        pub(in crate::workspaces) why: String,
    }

    impl NotMade {
        fn of(part: impl Into<String>) -> impl FnOnce(String) -> Self {
            let part = part.into();
            move |why| Self { part, why }
        }

        /// The record `record` of `app`.
        fn record(app: &str, record: &str) -> impl FnOnce(String) -> Self {
            Self::of(format!("the record {record} of {app}"))
        }

        /// The refusal the host answers with.
        pub(in crate::workspaces) fn refusal(&self) -> String {
            tilo_relay_session::workspaces::branch_not_made(&self.part, &self.why)
        }
    }

    impl Workspaces {
        /// Make `branch` (a UUID), a branch of `source` named `name`, the person
        /// `person`'s own: every record of `source` but what stays behind, forked
        /// with its history, and the content they name. Whose `source` is, and
        /// whether `branch` is made already, the caller decided.
        ///
        /// # Errors
        ///
        /// What did not come, named: a record of `source` that does not read
        /// whole, one that does not fork or found, its content, or the branch
        /// itself. Nothing of the branch stays then.
        pub(in crate::workspaces) async fn branch_own(
            &self,
            source: &str,
            branch: &str,
            name: &str,
            person: [u8; 32],
        ) -> Result<WorkspaceSummary, NotMade> {
            // What an earlier ask under this identity left unfinished (a fill
            // a stop cut short, or a discard that did not end) was never
            // anyone's: it goes before the branch is made again.
            if self.unfinished_branch(branch) {
                self.discard_unfinished(branch)
                    .await
                    .map_err(NotMade::of("the new workspace"))?;
            }
            let forked = self.forked(source, branch).await?;
            // Marked before anything of it is made, and unmarked only once it
            // is its person's: until then a host that stops opens none of it.
            self.begin_branch(branch)
                .await
                .map_err(NotMade::of("the new workspace"))?;
            let made = match self.create_workspace(name, Some(branch)).await {
                Ok(summary) => self
                    .fill(source, branch, forked, person)
                    .await
                    .map(|()| summary),
                Err(why) => Err(NotMade::of("the new workspace")(why)),
            };
            let made = match made {
                Ok(summary) => self
                    .finish_branch(branch)
                    .await
                    .map(|()| summary)
                    .map_err(NotMade::of("its ownership")),
                Err(not_made) => Err(not_made),
            };
            match made {
                Ok(summary) => {
                    tracing::info!(%source, %branch, "a workspace was branched");
                    let _ = self.inner.branches.send(Branched {
                        source: source.to_owned(),
                        branch: branch.to_owned(),
                        person,
                    });
                    Ok(summary)
                }
                Err(why) => {
                    tracing::warn!(%source, %branch, part = %why.part, why = %why.why, "a branch did not take everything; it is discarded");
                    // Never answered to anyone, so discarded rather than
                    // deleted: the same ask makes it again once it can. What a
                    // discard leaves, the next ask under this identity takes.
                    if let Err(error) = self.discard_unfinished(branch).await {
                        tracing::error!(%branch, %error, "a branch that did not take everything is not discarded yet");
                    }
                    Err(why)
                }
            }
        }

        /// Every record of `source` that comes along, read whole and forked into
        /// `branch`, saved.
        async fn forked(
            &self,
            source: &str,
            branch: &str,
        ) -> Result<Vec<(Route, Vec<u8>)>, NotMade> {
            let peer = self.state(source).await.ok_or_else(|| {
                NotMade::of("the workspace")("it is not governed here".to_owned())
            })?;
            let comes: BTreeSet<Route> = peer
                .plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .filter(|(app, record)| !tilo_desktop_model::branch::stays(app, record))
                .collect();
            drop(peer);
            let read = self
                .whole_records(source, Some(&comes))
                .await
                .map_err(|error| NotMade::of("its records")(error.to_string()))?;
            if let Some(((app, record), why)) = read.unread.first() {
                return Err(NotMade::record(app, record)(format!(
                    "it does not read whole here: {why}"
                )));
            }
            let branch = branch.to_owned();
            // Forking a record's whole history is work for a thread that may block.
            tokio::task::spawn_blocking(move || {
                read.documents
                    .into_iter()
                    .map(|((app, record), bytes)| {
                        let source = AutoCommit::load(&bytes)
                            .map_err(|error| NotMade::record(&app, &record)(error.to_string()))?;
                        let mut forked = ui::record::transfer::branch(&source, &branch)
                            .map_err(|error| NotMade::record(&app, &record)(error.to_string()))?;
                        Ok(((app, record), forked.save()))
                    })
                    .collect::<Result<Vec<_>, NotMade>>()
            })
            .await
            .map_err(|error| NotMade::of("its records")(error.to_string()))?
        }

        /// Found each of `forked` in `branch`, copy the content they name from
        /// `source`, and make the branch `person`'s own.
        async fn fill(
            &self,
            source: &str,
            branch: &str,
            forked: Vec<(Route, Vec<u8>)>,
            person: [u8; 32],
        ) -> Result<(), NotMade> {
            for ((app, record), document) in &forked {
                self.found_branched(branch, (app, record), document)
                    .await
                    .map_err(|error| NotMade::record(app, record)(error.to_string()))?;
            }
            let content = self.inner.branch_content.read_recover().clone();
            if let Some(content) = content {
                let (source, branch) = (source.to_owned(), branch.to_owned());
                let files = NotMade::of("the files its records name");
                match tokio::task::spawn_blocking(move || content.copy(&source, &branch, &forked))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(why)) => return Err(files(why)),
                    Err(error) => return Err(files(error.to_string())),
                }
            }
            self.branch_machine(source, branch)
                .await
                .map_err(NotMade::of("its Linux machine"))?;
            let ownership = || NotMade::of("its ownership");
            let own = self.own().ok_or_else(|| {
                ownership()("this host keeps no workspaces of people's own".to_owned())
            })?;
            own.record(&person, branch).map_err(ownership())?;
            self.administer_own(branch, person, tilo_clock::unix_now_ns())
                .await
                .map_err(|error| ownership()(error.to_string()))
        }

        /// Found `record` of `app` in `branch` as `document`, the source's
        /// record forked into it: as the governor founds a record again, the whole
        /// record as the branch's checkpoint of it, sealed under the branch's own
        /// keys where its records are sealed. A record the branch made of its own
        /// meanwhile (one an app makes from its kind alone) is taken in too.
        #[expect(
            clippy::significant_drop_tightening,
            reason = "the branch stays locked through durable signed publication"
        )]
        async fn found_branched(
            &self,
            branch: &str,
            (app, record): (&str, &str),
            document: &[u8],
        ) -> Result<(), CollaborationError> {
            let state = self.workspace(branch).await?;
            let mut governed = state.lock().await;
            let keys = self
                .sealing_keys(&governed, app, record)
                .await
                .map_err(CollaborationError::new)?;
            let mut whole = AutoCommit::load(document)
                .map_err(|error| CollaborationError::new(error.to_string()))?;
            if let Some(made) = governed
                .readable_document(app, record)
                .await
                .map_err(CollaborationError::new)?
            {
                let mut made = AutoCommit::load(&made)
                    .map_err(|error| CollaborationError::new(error.to_string()))?;
                tilo_automerge::ApplyIncoming::merge_incoming(&mut whole, &mut made)
                    .map_err(|error| CollaborationError::new(error.to_string()))?;
            }
            let now = tilo_clock::wall();
            let key = &self.inner.host.key;
            let mut candidate = governed.peer().clone();
            sealed_records::refound(
                &mut candidate,
                keys.as_ref(),
                (app, record),
                whole.save(),
                key,
                now,
            )
            .map_err(CollaborationError::new)?;
            governed.commit(candidate, key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
            self.settle(&mut governed).await?;
            Ok(())
        }
    }
}
