//! The records a connection reads and changes through the session's API: the ones this session's
//! own services keep for the packages it runs (the shell), which answer on the loop's pass, and the
//! workspace's, which the device's daemon answers.

use std::sync::Arc;

use tilo_actions::remote::{ActionCallError, Caller};

use super::SessionServices;

/// The records a connection reads and changes as any app's: those the session's services keep for
/// packages it runs itself (the shell), on the loop's pass, and the workspace's through the device's
/// dispatch.
pub(crate) struct ServiceRecords {
    pub(crate) state: Option<std::sync::Weak<super::Dispatch>>,
    pub(crate) session: Arc<SessionServices>,
}

impl ServiceRecords {
    /// The device's reader of the workspaces' records, which decides each read as the caller.
    fn workspace_reads(
        &self,
    ) -> Result<Arc<dyn tilo_actions::remote::ResourceReads>, ActionCallError> {
        self.state
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .map(|dispatch| dispatch as Arc<dyn tilo_actions::remote::ResourceReads>)
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.to_owned(),
            })
    }
}

impl tilo_actions::remote::ResourceReads for ServiceRecords {
    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer
    where
        Self: 'static,
    {
        Box::pin(tilo_action_calls::answer_read(
            self,
            caller.clone(),
            correlation,
            read,
        ))
    }

    /// Every app's resources are in the workspace's one store, whichever service publishes
    /// them: the broker answers.
    fn store_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
        read: serde_json::Value,
    ) -> tilo_actions::remote::ReadsAnswer<'a, serde_json::Value> {
        tilo_actions::remote::reads_answer(async move {
            self.workspace_reads()?
                .store_read(caller, address, read)
                .await
        })
    }

    /// The resources the session's services publish themselves (the device's sound, its windows,
    /// its styles, Settings' sections) are read here ([`published_read`]); every other address is
    /// an app's record, answered by the workspace.
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, tilo_actions::remote::ResourceCopy> {
        tilo_actions::remote::reads_answer(async move {
            if let Some(published) = self.session.published()
                && let Some(copy) = published_read(&**published, caller, address).await
            {
                return copy;
            }
            self.workspace_reads()?.read(caller, address).await
        })
    }

    /// A workspace's records change through the broker, which wakes its readers; what the session's
    /// services publish is woken by the pass that moves it ([`SessionServices::watch_reading`]).
    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a tilo_actions::remote::ResourceAddress,
        wake: &'a tilo_actions::remote::ResourceWake,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ()> {
        tilo_actions::remote::reads_answer(async move {
            if published_here(&address.app, &address.record).is_some() {
                self.session.watch_reading(wake);
                return Ok(());
            }
            self.workspace_reads()?.watch(caller, address, wake).await
        })
    }
}

/// Where this device's session publishes the resource `resource` of the app `app`, when the
/// platform publishes it ([`tilolang_tilo::platform::platform_roots`]): a root a view names with
/// the device's key in it (`devices/{device}/settings/power`) stands at this device's.
pub(crate) fn published_here(app: &str, resource: &str) -> Option<String> {
    let device = tilo_app::framework::settings_device();
    tilolang_tilo::platform::platform_roots()
        .into_iter()
        .find_map(|(root, ty)| {
            let here = tilolang_tilo::platform::on_device(&root, &device);
            (tilolang_tilo::platform::published_as(&ty) == app
                && (root == resource || here == resource))
                .then_some(here)
        })
}

/// The reading of `address` the session's services publish, as the person here reads it: read
/// through the app wire's link, as the seat's snapshot reads it for the device's daemon. A root
/// no service of this device serves (no battery) reads empty, as the shell's views are handed it. `None` for a resource the session does not
/// publish, and for a caller that is not the person: the workspace decides those.
pub(crate) async fn published_read(
    published: &dyn tilo_app_host::services::PublishedResources,
    caller: &Caller,
    address: &tilo_actions::remote::ResourceAddress,
) -> Option<Result<tilo_actions::remote::ResourceCopy, ActionCallError>> {
    let resource = published_here(&address.app, &address.record)?;
    if !caller.acting().is_person() {
        return None;
    }
    let app = tilo_app_host::services::Caller {
        app_id: address.app.clone(),
        workspace: address.workspace.clone(),
        opened_for: None,
    };
    let unavailable = |reason: String| Some(Err(ActionCallError::Unavailable { reason }));
    let reading = match published.snapshot(&app, &resource).await {
        Err(_) => return unavailable("the session dropped the reading".to_owned()),
        Ok(Err(reason)) => return Some(Err(ActionCallError::Refused { reason })),
        Ok(Ok((_, None))) => serde_json::json!({}),
        Ok(Ok((_, Some(reading)))) => match serde_json::from_slice(&reading) {
            Ok(reading) => reading,
            Err(error) => return unavailable(format!("the reading does not read: {error}")),
        },
    };
    Some(Ok(tilo_actions::remote::ResourceCopy::reading(
        reading, None,
    )))
}
