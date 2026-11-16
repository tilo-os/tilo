//! What a request must have before the service's thread reads its workspace:
//! the workspace's subscription, and the keys of what it reads. The thread
//! holds a request that lacks either and answers the other workspaces'
//! requests until the session hands it the change that makes this one
//! runnable ([`Worker::admit`](super::Worker)), so the first answer for one
//! workspace does not wait behind every other workspace joining.

use tilo_actions::remote::{CatalogueQuery, InvokeRequest};

use super::{Needs, record_of};

/// What a job must have before it can read its workspace: the workspace's
/// subscription, and the keys of what it reads. The service's thread holds a
/// job that lacks either, and answers the other workspaces' requests until
/// the session hands it the change that makes this one runnable
/// ([`Worker::admit`]), so the first answer for one workspace does not wait
/// behind every other workspace joining.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Keys {
    /// The workspace alone: a new record of it, or content it stores.
    Joined,
    /// Every record of the workspace whose keys are on their way.
    Workspace,
    /// One record: acted on (`act`), or listed by name.
    Record {
        app_id: String,
        record: String,
        act: bool,
    },
}

impl Keys {
    pub(super) fn needs(&self) -> Needs<'_> {
        match self {
            Self::Joined => Needs::Joined,
            Self::Workspace => Needs::Workspace,
            Self::Record { act: true, .. } => self.act(),
            Self::Record { act: false, .. } => self.read(),
        }
    }

    pub(super) fn act(&self) -> Needs<'_> {
        match self {
            Self::Record { app_id, record, .. } => Needs::Act { app_id, record },
            other => other.needs(),
        }
    }

    pub(super) fn read(&self) -> Needs<'_> {
        match self {
            Self::Record { app_id, record, .. } => Needs::Read { app_id, record },
            other => other.needs(),
        }
    }
}

/// What a listing of `query` waits for: the record it names by provider, or
/// the workspace's own records. A view lookup reads through the host's
/// records link, which holds the keys itself.
pub(super) fn catalogue_keys(query: &CatalogueQuery) -> Keys {
    if query.view_resource.is_some() {
        return Keys::Joined;
    }
    match query
        .provider
        .as_deref()
        .and_then(|provider| record_of(provider, ""))
    {
        Some(route) => Keys::Record {
            app_id: route.app_id,
            record: route.record,
            act: false,
        },
        None => Keys::Workspace,
    }
}

/// What running `request` waits for: the record it acts on, or, when it names
/// none, the workspace alone.
pub(super) fn invoke_keys(request: &InvokeRequest) -> Keys {
    match record_of(&request.target.provider, "") {
        Some(route) => Keys::Record {
            app_id: route.app_id,
            record: route.record,
            act: true,
        },
        None => Keys::Joined,
    }
}
