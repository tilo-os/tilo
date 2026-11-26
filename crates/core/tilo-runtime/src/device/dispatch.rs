//! The content this daemon keeps for its workspaces' devices, as a turn it may
//! take asks it before claiming the turn.
//!
//! A turn of an offloadable action is the action itself, dispatched through this
//! device's own dispatch to the app or engine that performs it; the device only
//! takes it when the content the action reads is already here, so nothing moves
//! to run it over content that is elsewhere.

use std::sync::Arc;

use tilo_app_host::route::CrdtRoute;
use tilo_workspace_service::content::RouteContent;
use tilo_workspace_service::work::{Job, KeepsContent};

/// The content of its workspaces' apps this daemon keeps.
#[derive(Clone)]
pub(crate) struct Kept(Arc<RouteContent>);

impl Kept {
    /// What the daemon keeps of the app content it routes.
    pub(crate) fn new(content: Arc<RouteContent>) -> Self {
        Self(content)
    }
}

impl KeepsContent for Kept {
    fn holds(&self, workspace: &str, job: &Job) -> bool {
        let route = CrdtRoute::app(&job.app, workspace);
        job.inputs
            .iter()
            .all(|digest| self.0.kept(&route, &hex::encode(digest)).is_some())
    }
}
