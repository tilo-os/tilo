//! The resources of the apps the session's services run over, kept in the workspace the person
//! is in once the device's daemon holds it: the store the app's windows open, so the services,
//! the windows, an agent's programs and the person's other devices read and change one copy.

impl crate::Shared {
    /// Keep the apps' resources the services run over in the workspace the person is in, once the
    /// session holds it: the store the app's windows open, so the service, the windows, an agent's
    /// programs and the person's other devices read and change one copy.
    pub(crate) fn keep_app_stores(&self, runtime: &tokio::runtime::Handle) {
        if *self.app_stores.borrow() == AppStores::Kept {
            return;
        }
        let waits = |on: &'static str| *self.app_stores.borrow_mut() = AppStores::Waiting(on);
        let Some(workspace) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        }) else {
            return waits("the session shows no workspace");
        };
        // A workspace the daemon does not hold yet (one still being founded) would refuse every
        // record moved there.
        let Some(standing) = self
            .view
            .as_ref()
            .and_then(|view| view.holds(workspace).then(|| view.revision()))
        else {
            return waits("the device's daemon does not hold the workspace the session shows yet");
        };
        // A move the workspace refused waits for how the workspace stands to move on, rather
        // than being asked again on every pass.
        if matches!(&*self.app_stores.borrow(), AppStores::Refused(held, _) if *held == standing) {
            return;
        }
        let workspace = workspace.to_string();
        if !self.document_reads.workspaces().contains(&workspace) {
            return waits("the device's daemon does not read the workspace yet");
        }
        let Some(store) = (self.held_stores)(workspace, runtime) else {
            return waits("this device's workspace service holds no store of the workspace");
        };
        let kept = match self.services.borrow_mut().keep_in_workspace(&store) {
            Ok(_) => AppStores::Kept,
            Err(error) => {
                tracing::warn!(%error, "the apps' resources are not kept in the workspace");
                AppStores::Refused(standing, error)
            }
        };
        *self.app_stores.borrow_mut() = kept;
    }
}

/// Where moving the apps' resources into the workspace the person is in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppStores {
    /// Not moved yet, and what the move waits on.
    Waiting(&'static str),
    /// The workspace refused them, saying why, while it stood here on the device's daemon.
    Refused(u64, String),
    /// Every app's resources are kept there.
    Kept,
}

impl Default for AppStores {
    fn default() -> Self {
        Self::Waiting("the session has not yet offered them")
    }
}

impl std::fmt::Display for AppStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waiting(on) => write!(f, "waiting: {on}"),
            Self::Refused(_, why) => write!(f, "refused: {why}"),
            Self::Kept => f.write_str("kept"),
        }
    }
}

/// The store of a workspace this device's own service holds, none while the service is not open.
pub(crate) type HeldStores = std::rc::Rc<
    dyn Fn(
        String,
        &tokio::runtime::Handle,
    ) -> Option<std::rc::Rc<dyn tilo_service::WorkspaceStore>>,
>;

/// The stores of the workspaces `service` holds, once it is open.
pub(crate) fn held_stores(service: tilo_workspace_service::service::ServiceSlot) -> HeldStores {
    std::rc::Rc::new(move |workspace, runtime| {
        service.service()?;
        Some(std::rc::Rc::new(HeldStore {
            service: service.clone(),
            workspace,
            runtime: runtime.clone(),
        }))
    })
}

/// The store of one workspace this device's own service holds, as the apps the session runs
/// itself write and read their resources in it.
struct HeldStore {
    service: tilo_workspace_service::service::ServiceSlot,
    workspace: String,
    runtime: tokio::runtime::Handle,
}

impl tilo_service::WorkspaceStore for HeldStore {
    fn command(&self, app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: tilo_resource_store::wire::Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        // A first write can wait for this device's group keys on the reactor.
        let reply = self.runtime.block_on(
            self.service
                .service()
                .ok_or("this device's workspace service is not open")?
                .workspaces()
                .own_resource_command(&self.workspace, app, command),
        )?;
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }
}
