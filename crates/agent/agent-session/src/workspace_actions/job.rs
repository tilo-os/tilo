//! What the workspace actions worker is asked to do: one job per request or
//! event, served one at a time on its thread.

#[allow(
    clippy::wildcard_imports,
    reason = "a job names the worker's own types"
)]
use super::*;

/// How soon a queued job is taken ([`Job::urgency`]): the least first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Urgency {
    Read,
    Write,
    Background,
}

/// What the worker thread is asked to do, one at a time.
pub(super) enum Job {
    Feedback {
        workspace_id: WorkspaceId,
        query: Box<tilo_actions::remote::feedback::FeedbackQuery>,
        caller: Caller,
        done: Completer<Result<tilo_actions::remote::feedback::FeedbackAnswer, ActionCallError>>,
    },
    Catalogue {
        workspace_id: WorkspaceId,
        query: Box<CatalogueQuery>,
        caller: Caller,
        done: Completer<Result<CataloguePage, ActionCallError>>,
    },
    Invoke {
        workspace_id: WorkspaceId,
        invocation: InvocationId,
        done: Completer<InvocationReceipt>,
    },
    /// A Terminal session's command that ended in the server's shell, to keep.
    Ran(Box<terminal::Ended>),
    /// A Terminal session's open that the server's shell answered, to settle.
    Opened(Box<terminal::OpenEnded>),
    /// A command line typed into a terminal a Terminal session's open started, to record.
    Typed(Box<terminal::Ended>),
    /// A step of an upload into a workspace whose store the service has not
    /// found yet.
    Content {
        workspace_id: WorkspaceId,
        caller: Caller,
        step: tilo_actions::remote::ContentPutStep,
        done: Completer<Result<tilo_actions::remote::ContentPutAnswer, ActionCallError>>,
    },
    /// A piece of content asked of a workspace whose store the service has
    /// not found yet.
    ContentGet {
        workspace_id: WorkspaceId,
        caller: Caller,
        content: String,
        offset: u64,
        length: u64,
        done: Completer<Result<tilo_actions::remote::ContentGetAnswer, ActionCallError>>,
    },
    /// A link to content of a workspace whose store the service has not found
    /// yet, for a caller who may read it.
    ContentLink {
        workspace_id: WorkspaceId,
        caller: Caller,
        link: tilo_actions::remote::ContentLinkAsk,
        done: Completer<Result<tilo_actions::remote::ContentLink, ActionCallError>>,
    },
    /// One document of a workspace, read as its caller may read it
    /// ([`tilo_actions::remote::ResourceReads`]).
    ReadDocument {
        workspace_id: WorkspaceId,
        caller: Caller,
        address: tilo_actions::remote::ResourceAddress,
        done: Completer<Result<tilo_actions::remote::ResourceCopy, ActionCallError>>,
    },
    /// A search of what a workspace holds.
    Search {
        workspace_id: WorkspaceId,
        caller: Caller,
        needle: String,
        within: tilo_actions::remote::Within,
        done: Completer<Result<Vec<tilo_actions::remote::Found>, ActionCallError>>,
    },
    /// The changes to a workspace's documents after a cursor.
    Changes {
        workspace_id: WorkspaceId,
        caller: Caller,
        after: Option<String>,
        done: Completer<Result<tilo_actions::remote::ChangePage, ActionCallError>>,
    },
    /// A caller waits for what it follows in a workspace to change
    /// ([`tilo_actions::remote::ActionProvider::changed`]): a write landing in the workspace's
    /// documents, or who is in it moving.
    Changed {
        workspace_id: WorkspaceId,
        caller: Caller,
        done: Completer<Result<(), ActionCallError>>,
    },
    /// A page of a workspace's invocations.
    Invocations {
        workspace_id: WorkspaceId,
        caller: Caller,
        query: tilo_actions::remote::workspace_invocations::InvocationsQuery,
        done: Completer<
            Result<tilo_actions::remote::workspace_invocations::Invocations, ActionCallError>,
        >,
    },
    /// An agent session connected to a workspace and has asked nothing yet:
    /// it is present there, waiting ([`session_voice`]).
    Arrived {
        workspace_id: WorkspaceId,
        caller: Caller,
    },
    /// A workspace requests wait to be joined to changed: those it joined
    /// go on.
    Joined(WorkspaceId),
    /// An app's worker answered the operation a request of the workspace handed off this
    /// thread: the request goes on ([`call_wait`]).
    Called(WorkspaceId),
    /// A replica's parts were taken in off the thread ([`part_load`]): the request that
    /// waited for them goes on.
    Loaded(WorkspaceId),
    /// A workspace was deleted: the service lets go of all it holds of it.
    Forget(WorkspaceId),
    /// A document written since its name was last read: its name in its app's index is
    /// read again ([`publish`]).
    Rename(WorkspaceId, DocumentRoute),
    /// Another's write landed in a record of the store the service reads, as its host read it
    /// there: what holds the record merges it before the next request
    /// ([`super::WorkspaceActions::landed`]).
    Landed(WorkspaceId, DocumentRoute),
    /// A web page of a workspace's app: opened, acted on or closed ([`pages`]).
    Page(Box<pages::PageJob>),
}

impl Job {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Feedback { .. } => "feedback",
            Self::Catalogue { .. } => "catalogue",
            Self::Invoke { .. } => "invoke",
            Self::Ran(_) => "ran",
            Self::Opened(_) => "opened",
            Self::Typed(_) => "typed",
            Self::Arrived { .. } => "arrived",
            Self::Content { .. } => "content",
            Self::ContentGet { .. } => "content-get",
            Self::ContentLink { .. } => "content-link",
            Self::ReadDocument { .. } => "read-document",
            Self::Search { .. } => "search",
            Self::Changes { .. } => "changes",
            Self::Invocations { .. } => "invocations",
            Self::Changed { .. } => "changed",
            Self::Joined(_) => "joined",
            Self::Called(_) => "called",
            Self::Loaded(_) => "loaded",
            Self::Forget(_) => "forget",
            Self::Rename(..) => "rename",
            Self::Landed(..) => "landed",
            Self::Page(_) => "page",
        }
    }

    /// Whether the request waits off the workspace's thread for the parts of what it reads or
    /// changes: a write, a listing, or a page's act, which is a person's click and waits like
    /// one.
    pub(super) const fn holds_for_parts(&self) -> bool {
        match self {
            Self::Invoke { .. } | Self::Catalogue { .. } => true,
            Self::Page(job) => matches!(**job, pages::PageJob::Act { .. }),
            _ => false,
        }
    }

    /// Which step of a content put the job is, for the log: a slow one is
    /// told apart from the others by what it was doing.
    pub(super) const fn step_kind(&self) -> Option<&'static str> {
        match self {
            Self::Content { step, .. } => Some(match step {
                tilo_actions::remote::ContentPutStep::Begin { .. } => "begin",
                tilo_actions::remote::ContentPutStep::Append { .. } => "append",
                tilo_actions::remote::ContentPutStep::Finish { .. } => "finish",
                tilo_actions::remote::ContentPutStep::Many { .. } => "many",
                tilo_actions::remote::ContentPutStep::Have { .. } => "have",
                tilo_actions::remote::ContentPutStep::Progress { .. } => "progress",
            }),
            _ => None,
        }
    }

    /// How soon the worker takes the job among those queued: what a caller waits on to read
    /// first, then writes in the order they came, then what no caller waits on. A bulk import
    /// queues a write after every answer, and each write it lands queues a name to read again:
    /// served in arrival order, a listing asked meanwhile waited behind all of them, past the
    /// wait its caller gives up after. A reader asked after a write it waited for still reads
    /// what that write did: a caller is answered a write once it ran.
    pub(super) const fn urgency(&self) -> Urgency {
        match self {
            Self::Catalogue { .. }
            | Self::ReadDocument { .. }
            | Self::Search { .. }
            | Self::Changes { .. }
            | Self::Invocations { .. }
            | Self::Changed { .. }
            | Self::Feedback { .. }
            | Self::Content { .. }
            | Self::ContentGet { .. }
            | Self::ContentLink { .. }
            | Self::Joined(_)
            | Self::Called(_)
            | Self::Loaded(_)
            | Self::Landed(..)
            | Self::Page(_) => Urgency::Read,
            Self::Invoke { .. }
            | Self::Ran(_)
            | Self::Opened(_)
            | Self::Typed(_)
            | Self::Arrived { .. }
            | Self::Forget(_) => Urgency::Write,
            Self::Rename(..) => Urgency::Background,
        }
    }

    /// Who asked for the job, where it names them itself: an invocation is named by its id
    /// alone, and a page acted on by its number ([`Worker::region_for`]).
    pub(super) const fn caller(&self) -> Option<&Caller> {
        match self {
            Self::Feedback { caller, .. }
            | Self::Catalogue { caller, .. }
            | Self::Content { caller, .. }
            | Self::ContentGet { caller, .. }
            | Self::ContentLink { caller, .. }
            | Self::ReadDocument { caller, .. }
            | Self::Search { caller, .. }
            | Self::Changes { caller, .. }
            | Self::Changed { caller, .. }
            | Self::Invocations { caller, .. }
            | Self::Arrived { caller, .. } => Some(caller),
            Self::Page(job) => job.caller(),
            Self::Invoke { .. }
            | Self::Ran(_)
            | Self::Opened(_)
            | Self::Typed(_)
            | Self::Joined(_)
            | Self::Called(_)
            | Self::Loaded(_)
            | Self::Forget(_)
            | Self::Rename(..)
            | Self::Landed(..) => None,
        }
    }

    /// The workspace whose documents the job reads or changes.
    pub(super) const fn workspace(&self) -> Option<WorkspaceId> {
        match self {
            Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. }
            | Self::ContentLink { workspace_id, .. }
            | Self::ReadDocument { workspace_id, .. }
            | Self::Search { workspace_id, .. }
            | Self::Changes { workspace_id, .. }
            | Self::Invocations { workspace_id, .. }
            | Self::Changed { workspace_id, .. }
            | Self::Arrived { workspace_id, .. }
            | Self::Loaded(workspace_id)
            | Self::Rename(workspace_id, _)
            | Self::Landed(workspace_id, _) => Some(*workspace_id),
            Self::Page(job) => Some(job.workspace()),
            Self::Feedback { .. }
            | Self::Ran(_)
            | Self::Opened(_)
            | Self::Typed(_)
            | Self::Joined(_)
            | Self::Called(_)
            | Self::Forget(_) => None,
        }
    }

    /// What the job must have before it reads its workspace: what it is about
    /// to read names what it waits for. An invocation is named here by its id
    /// alone: the worker reads its request from the log
    /// ([`Worker::keys`]).
    pub(super) fn keys(&self) -> Keys {
        match self {
            Self::Catalogue { query, .. } => catalogue_keys(query),
            // Every document the workspace holds is searched and followed.
            Self::Search { .. }
            | Self::Changes { .. }
            | Self::Invocations { .. }
            | Self::ReadDocument { .. }
            | Self::Changed { .. } => Keys::Workspace,
            Self::Feedback { .. }
            | Self::Invoke { .. }
            | Self::Ran(_)
            | Self::Opened(_)
            | Self::Typed(_)
            | Self::Content { .. }
            | Self::ContentGet { .. }
            | Self::ContentLink { .. }
            | Self::Arrived { .. }
            | Self::Joined(_)
            | Self::Called(_)
            | Self::Loaded(_)
            | Self::Forget(_)
            | Self::Page(_)
            | Self::Rename(..)
            | Self::Landed(..) => Keys::Joined,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(act: bool) -> Job {
        let workspace_id = WorkspaceId::parse("6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b").unwrap();
        let job = if act {
            let (done, _answer) = tilo_actions::remote::pending();
            pages::PageJob::Act {
                workspace_id,
                page: 1,
                act: None,
                signed: None,
                invoker: None,
                done,
            }
        } else {
            pages::PageJob::Close {
                workspace_id,
                page: 1,
            }
        };
        Job::Page(Box::new(job))
    }

    #[test]
    fn a_pages_act_holds_for_parts_as_a_write_does() {
        assert!(page(true).holds_for_parts());
        assert!(!page(false).holds_for_parts());
    }
}
