//! A launch admitted again after the launch queue's wait: ownership and launch
//! policy read afresh, and the launch they admit now, which must be the one
//! asked for.

use tilo_actions::remote::{ActionHostError, Caller, InvokeRequest};

use super::super::AppBrokerState;
use super::LaunchDestination;

impl AppBrokerState {
    /// After the launch queue's wait: ownership and launch policy read again,
    /// and the launch it admits now, which must be the one asked for.
    pub(super) fn readmit_launch(
        &self,
        caller: &Caller,
        origin: Option<&super::super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        destination: &LaunchDestination,
    ) -> Result<LaunchDestination, ActionHostError> {
        self.check_launch_origin(caller, origin, &destination.workspace.id().to_string())?;
        let current = self.admit_launchable(caller, request)?;
        if current.workspace.id() != destination.workspace.id()
            || current.app_id != destination.app_id
            || current.record != destination.record
        {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(current)
    }
}
