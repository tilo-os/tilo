//! Branching a workspace this host keeps for a person
//! ([`tilo_server_protocol::workspaces::BRANCH_OWN_WORKSPACE`],
//! `docs/architecture/workspace.md`, "Branching a workspace"): a new workspace of
//! theirs that starts with everything the source holds and goes on apart from it.
//!
//! A workspace is no file to copy: its retained frames name it, its sealed records
//! are bound to its identity and its channels' keys, and its governance is signed
//! for it. The host copies what it reads instead, as a device that branches a
//! workspace does, since it is a member of every channel of the workspaces it
//! governs:
//!
//! 1. Every record of the source's store is read whole, but what stays
//!    behind ([`tilo_desktop_model::branch::stays`]: the conversations in Chat,
//!    each person's place in the tilolang apps, Terminal's environments, the
//!    desktop's layout of everyone's), records whose owner is unknown, and
//!    records that belong to a member. A remaining record that does not read
//!    whole stops the branch before anything is made.
//! 2. The branch is made as any workspace of the person's own is: governed here,
//!    its governance new, the person its only member and its administrator, its
//!    channels with keys of their own the first time a record of it is sealed.
//! 3. Each record is founded in the branch's store as it reads now, its history left with the
//!    source ([`Workspaces::found_record`]).
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

/// The Files keys needed to copy retained versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchFileKeys {
    None,
    Destination,
    SourceAndDestination,
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

    /// The key groups needed to copy the versions the source still holds.
    fn file_keys(
        &self,
        _source: &str,
        _documents: &[((String, String), Vec<u8>)],
    ) -> Result<BranchFileKeys, String> {
        Ok(BranchFileKeys::None)
    }

    /// Copy and rekey the Files tree before committing the branch's documents.
    fn copy_rekeyed(
        &self,
        _source: &str,
        _branch: &str,
        _documents: &mut [((String, String), Vec<u8>)],
        _keys: (Option<&key_group::ContentKeys>, &key_group::ContentKeys),
    ) -> Result<(), String> {
        Err("the content store cannot rekey sealed files".into())
    }

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
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use automerge::AutoCommit;

    use super::{BranchFileKeys, Branched, Workspaces};
    use crate::workspaces::WorkspaceSummary;
    use tilo_lock::RwLockRecover as _;

    /// A record by its app and id.
    type Route = (String, String);

    /// The records a branch takes, each saved, with the kind of each.
    type Forked = (Vec<(Route, Vec<u8>)>, BTreeMap<Route, String>);

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

        /// The refusal the host answers with; `why` stays with the host's log.
        pub(in crate::workspaces) fn refusal(&self) -> String {
            tilo_server_protocol::workspaces::AdminRefusal::BranchNotMade {
                part: self.part.clone(),
            }
            .to_reason()
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
            let forked = self.forked(source).await?;
            // Marked before anything of it is made, and unmarked only once it
            // is its person's: until then a host that stops opens none of it.
            self.begin_branch(branch)
                .await
                .map_err(NotMade::of("the new workspace"))?;
            let made = match self.create_workspace_keeping_its_apps(name, branch).await {
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

        /// Every record of `source`'s store that comes along, read whole and saved, with the
        /// kind of each.
        async fn forked(&self, source: &str) -> Result<Forked, NotMade> {
            let state = self
                .workspace(source)
                .await
                .map_err(|error| NotMade::of("the workspace")(error.to_string()))?;
            let mut governed = state.lock().await;
            let stored = self.stored_documents(&mut governed, None, true);
            drop(governed);
            let (mut documents, mut kinds) = (Vec::new(), BTreeMap::new());
            for ((app, record), kind, read) in stored {
                // A record whose app this device knows nothing of stays behind with it.
                if app.is_empty()
                    || tilo_desktop_model::branch::stays(&app, &record)
                    || tilo_action_policy::roles::is_private_record(&app, &record)
                {
                    continue;
                }
                let (_, bytes) = read.map_err(|why| {
                    NotMade::record(&app, &record)(format!("it does not read whole here: {why}"))
                })?;
                kinds.insert((app.clone(), record.clone()), kind);
                documents.push(((app, record), bytes));
            }
            Ok((documents, kinds))
        }

        async fn copy_branch_content(
            &self,
            source: &str,
            branch: &str,
            forked: Vec<(Route, Vec<u8>)>,
        ) -> Result<Vec<(Route, Vec<u8>)>, NotMade> {
            let Some(content) = self.inner.branch_content.read_recover().clone() else {
                return Ok(forked);
            };
            let files = || NotMade::of("the files its documents name");
            let (forked, file_keys) = {
                let content = Arc::clone(&content);
                let source = source.to_owned();
                tokio::task::spawn_blocking(move || {
                    let file_keys = content.file_keys(&source, &forked)?;
                    Ok::<_, String>((forked, file_keys))
                })
                .await
                .map_err(|error| files()(error.to_string()))?
                .map_err(files())?
            };
            let keys = if file_keys == BranchFileKeys::None {
                None
            } else {
                let route = |workspace: &str| tilo_peer_state::document_frame::DocumentRoute {
                    app_id: tilo_app_id::apps::FILE_EXPLORER.to_owned(),
                    cluster_id: workspace.to_owned(),
                    document: collab_types::person_document_id(collab_types::FILES_TREE_KIND),
                };
                let source = if file_keys == BranchFileKeys::SourceAndDestination {
                    Some(
                        self.own_route_keys(&route(source), false)
                            .await
                            .ok_or_else(|| {
                                files()("the source's Files keys are unavailable".to_owned())
                            })?
                            .map_err(files())?,
                    )
                } else {
                    None
                };
                let branch = self
                    .own_route_keys(&route(branch), true)
                    .await
                    .ok_or_else(|| files()("the branch's Files keys are unavailable".to_owned()))?
                    .map_err(files())?;
                Some((source, branch))
            };
            let (source, branch) = (source.to_owned(), branch.to_owned());
            tokio::task::spawn_blocking(move || {
                let mut documents = forked;
                match keys {
                    Some((from, to)) => content.copy_rekeyed(
                        &source,
                        &branch,
                        &mut documents,
                        (from.as_ref(), &to),
                    )?,
                    None => content.copy(&source, &branch, &documents)?,
                }
                Ok::<_, String>(documents)
            })
            .await
            .map_err(|error| files()(error.to_string()))?
            .map_err(files())
        }

        /// Found each of `forked` in `branch`, copy the content they name from
        /// `source`, and make the branch `person`'s own.
        async fn fill(
            &self,
            source: &str,
            branch: &str,
            (forked, kinds): Forked,
            person: [u8; 32],
        ) -> Result<(), NotMade> {
            let mut forked = self.copy_branch_content(source, branch, forked).await?;
            for ((app, record), document) in &mut forked {
                self.reseal_branched_vault((source, branch), (app, record), document)
                    .await
                    .map_err(NotMade::record(app, record))?;
            }
            for ((app, record), document) in &forked {
                let kind = kinds
                    .get(&(app.clone(), record.clone()))
                    .ok_or_else(|| NotMade::record(app, record)("it has no kind".to_owned()))?;
                let held = AutoCommit::load(document)
                    .map_err(|error| NotMade::record(app, record)(error.to_string()))?;
                self.found_record(branch, (app, kind, record), (&held, false))
                    .await
                    .map_err(NotMade::record(app, record))?;
            }
            self.branch_machine(source, branch)
                .await
                .map_err(NotMade::of("its Linux machine"))?;
            let ownership = || NotMade::of("its ownership");
            let own = self.own().ok_or_else(|| {
                ownership()("this host keeps no workspaces of people's own".to_owned())
            })?;
            own.record(&person, branch).map_err(ownership())?;
            if let Ok(summary) = self.summary(branch).await {
                self.list_for(&person, &summary.workspace, &summary.name)
                    .await;
            }
            let held = self
                .workspace(branch)
                .await
                .map_err(|error| ownership()(error.to_string()))?;
            let mut held = held.lock().await;
            self.give_ownership(&mut held, person, None)
                .await
                .map_err(ownership())
        }
    }
}
