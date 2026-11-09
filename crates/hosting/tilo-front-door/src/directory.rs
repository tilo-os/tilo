//! Where a person's workspaces are, and how to reach their agent endpoints.
//!
//! The door owns no workspace. A workspace record names the host that serves
//! it, and making a workspace is the host's action, which the host may refuse
//! (quota, payment, policy). The door asks; it decides nothing about
//! membership itself, and it routes only to a workspace the directory says
//! the person has.

use std::{future::Future, pin::Pin};

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

/// How the door reaches a workspace's agent endpoint: the `tilo-runtime`
/// streamable HTTP MCP endpoint serving that workspace, and the bearer token
/// that proves the caller is the door.
#[derive(Clone, PartialEq, Eq)]
pub struct Route {
    pub url: String,
    pub token: String,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("url", &self.url)
            .field("token", &"[redacted]")
            .finish()
    }
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

/// The person's workspaces and the hosts serving them.
pub trait Directory: Send + Sync + 'static {
    /// `person`'s workspaces (identity root key, hex).
    fn workspaces<'a>(
        &'a self,
        person: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>>;

    /// The hosts `person` may make a workspace on.
    fn hosts<'a>(&'a self, person: &'a str) -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>>;

    /// Make a workspace called `name` for `person` on `host` (the default
    /// host when `None`): the host's create-workspace action, which it may
    /// refuse.
    fn create<'a>(
        &'a self,
        person: &'a str,
        name: &'a str,
        host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>>;

    /// `person`'s own revocation list (identity root key, raw): the devices
    /// they revoked, which may sign nobody in.
    fn revocations<'a>(
        &'a self,
        person: &'a [u8; 32],
    ) -> BoxFuture<'a, Result<tilo_user_identity::RevocationList, DirectoryError>>;

    /// The agent endpoint of `person`'s workspace `workspace`.
    /// [`DirectoryError::NotFound`] unless the person has it.
    fn route<'a>(
        &'a self,
        person: &'a str,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>>;
}
