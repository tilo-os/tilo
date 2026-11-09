//! Where a person's workspaces are, and how to reach their agent endpoints.
//!
//! The door owns no workspace. A workspace record names the host that serves
//! it, and making a workspace is the host's action, which the host may refuse
//! (quota, payment, policy). The door asks, always as the grant it acts for
//! ([`GrantKey`]); it decides nothing about membership itself, and it routes
//! only to a workspace the person's host lists to that grant.

use crate::grant_key::GrantKey;
use std::{future::Future, pin::Pin};

pub use crate::endpoint::Route;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One of a person's workspaces, as an agent is shown it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Workspace {
    /// The workspace's id (a UUID): what every action names it by.
    pub id: String,
    pub name: String,
    /// The host serving it, named as hosts are (`eu1.tilo.sh`).
    pub host: String,
}

/// A host a person may make a workspace on.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Host {
    pub name: String,
    /// Whether making a workspace there is the default.
    pub default: bool,
}

/// Why the directory answered nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryError {
    /// Not one of the person's workspaces, or no such workspace: the same
    /// answer, so a guess learns nothing.
    NotFound,
    /// The host refused, with its reason (quota, payment, policy).
    Refused(String),
    /// The host or the directory could not be reached.
    Unavailable(String),
}

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such workspace"),
            Self::Refused(reason) => write!(f, "the host refused: {reason}"),
            Self::Unavailable(reason) => write!(f, "unavailable: {reason}"),
        }
    }
}

/// The person's workspaces and the hosts serving them, as a grant of theirs
/// is answered.
pub trait Directory: Send + Sync + 'static {
    /// The workspaces `grant` reaches.
    fn workspaces<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>>;

    /// The hosts the grant's person may make a workspace on.
    fn hosts<'a>(&'a self, grant: &'a GrantKey)
    -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>>;

    /// Make a workspace called `name` on `host` (the default host when
    /// `None`) as `grant`: the host's create-workspace action, which it may
    /// refuse. The person's runtime has joined it when this answers, so the
    /// next call can act in it.
    fn create<'a>(
        &'a self,
        grant: &'a GrantKey,
        name: &'a str,
        host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>>;

    /// The grant's person's revocation list, as their host enforces it: the
    /// devices they revoked, which may sign nobody in.
    fn revocations<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<tilo_user_identity::RevocationList, DirectoryError>>;

    /// Where the runtime serving `workspace` answers.
    /// [`DirectoryError::NotFound`] unless `grant` reaches it.
    fn route<'a>(
        &'a self,
        grant: &'a GrantKey,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>>;
}
