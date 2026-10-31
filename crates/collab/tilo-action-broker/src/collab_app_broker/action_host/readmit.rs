//! A launch admitted again after the launch queue's wait: custody and launch
//! policy read afresh, and the launch they admit now, which must be the one
//! asked for.

use tilo_actions::remote::{ActionHostError, Caller, InvokeRequest};

use super::super::AppBrokerState;
use super::LaunchDestination;
use super::refusal;

impl AppBrokerState {
    /// After the launch queue's wait: custody and launch policy read again,
    /// and the launch it admits now, which must be the one asked for.
    pub(super) fn readmit_launch(
        &self,
        caller: &Caller,
        origin: Option<&super::super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        destination: &LaunchDestination,
        native: Option<&super::super::native_task_execution::NativeLaunch>,
    ) -> Result<LaunchDestination, ActionHostError> {
        self.check_launch_origin(caller, origin, &destination.workspace.room().to_string())?;
        let current = match native {
            Some(native) => native.destination(self).map_err(refusal)?,
            None => self.admit_launchable(caller, request)?,
        };
        if current.workspace.room() != destination.workspace.room()
            || current.app_id != destination.app_id
            || current.record != destination.record
        {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(current)
    }
}
