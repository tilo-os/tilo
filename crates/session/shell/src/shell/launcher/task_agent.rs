//! Launching the agent made for one task, under the key the shell minted for
//! it ([`crate::shell::task_agents`]).

use super::{
    InstalledApp, LaunchError, RoomId, WORKSPACE_AGENT_PACKAGE_ID, Workspace, installed_apps,
    is_headless_collab_service,
};
#[cfg(target_os = "linux")]
use super::{
    build_workspace_agent_command, selected_execution_program, spawn_detached,
    start_agent_companions, workspace_agent_provider,
};

/// Start the agent made for one task in `workspace`, under the key the shell
/// minted for it ([`crate::shell::task_agents`]). Unlike a collaborator it
/// needs no admission and no administrator: its authority is the task
/// delegation this device signed, which the broker verifies when it joins
/// and on every revalidation. Its credentials are left in its own app
/// directory, which only it and this desktop reach, for it to read once.
///
/// # Errors
///
/// Returns an error when the fixed agent service is missing or malformed,
/// its app directory cannot be prepared or written, or the normal launcher
/// setup fails.
pub fn launch_task_agent(
    workspace: &Workspace,
    agent_id: &str,
    credentials: &runtime_collab::TaskAgentCredentials,
) -> Result<InstalledApp, LaunchError> {
    let app = installed_apps()?
        .into_iter()
        .find(|app| app.package_id == WORKSPACE_AGENT_PACKAGE_ID)
        .ok_or_else(|| LaunchError::NotInstalled {
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
        })?;
    if !is_headless_collab_service(&app)? {
        return Err(LaunchError::NotAManagedService {
            package_id: app.package_id,
        });
    }
    leave_task_credentials(&workspace.room(), agent_id, credentials)?;
    execute_task_agent(app, agent_id, workspace)
}

/// Whether a task agent can be launched here: the fixed agent service is
/// installed as a headless broker service.
#[must_use]
pub fn task_agent_runtime_installed() -> bool {
    installed_apps().is_ok_and(|apps| {
        apps.iter().any(|app| {
            app.package_id == WORKSPACE_AGENT_PACKAGE_ID
                && is_headless_collab_service(app).unwrap_or(false)
        })
    })
}

/// Write `credentials` where the task agent `agent_id` launched in `room`
/// reads them: its app directory's [`runtime_collab::TASK_AGENT_CREDENTIALS_DIR`],
/// owner-only, a new file.
fn leave_task_credentials(
    room: &RoomId,
    agent_id: &str,
    credentials: &runtime_collab::TaskAgentCredentials,
) -> Result<(), LaunchError> {
    use std::io::Write as _;
    let failed = |detail: String| LaunchError::IdentityUnavailable { detail };
    let data_home = launcher::appdir::host_data_home()
        .ok_or_else(|| failed("no data directory for the task agent".into()))?;
    let room = room.to_string();
    let dir =
        launcher::appdir::prepare_host_app_dir(&data_home, Some(&room), WORKSPACE_AGENT_PACKAGE_ID)
            .map_err(|error| failed(format!("preparing the task agent's directory: {error}")))?
            .join(runtime_collab::TASK_AGENT_CREDENTIALS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|error| failed(format!("preparing the task agent's credentials: {error}")))?;
    let bytes = credentials
        .encode()
        .map_err(|error| failed(format!("encoding the task agent's credentials: {error}")))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let path = dir.join(runtime_collab::TaskAgentCredentials::file_name(agent_id));
    options
        .open(&path)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|error| failed(format!("writing the task agent's credentials: {error}")))
}

#[cfg(not(target_os = "linux"))]
fn execute_task_agent(
    _app: InstalledApp,
    _agent_id: &str,
    _workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    Err(LaunchError::ProviderUnavailable {
        provider: "workspace process service",
    })
}

/// [`execute_workspace_agent`] for a task agent: the same sandboxed service
/// and companions, told to join under its task's credentials.
#[cfg(target_os = "linux")]
fn execute_task_agent(
    app: InstalledApp,
    agent_id: &str,
    workspace: &Workspace,
) -> Result<InstalledApp, LaunchError> {
    let launcher_bin = selected_execution_program()?;
    let identity_path = crate::identity_registry::selected_identity_path(
        &workspace.room().to_string(),
    )
    .map_err(|error| LaunchError::IdentityUnavailable {
        detail: error.to_string(),
    })?;
    let companions =
        start_agent_companions(&launcher_bin, workspace.room(), identity_path.as_deref());
    let mut command = build_workspace_agent_command(
        &app,
        &launcher_bin,
        workspace.room(),
        identity_path.as_deref(),
        agent_id,
    );
    command
        .args
        .extend(["--task-identity".into(), agent_id.to_owned()]);
    for companion in companions {
        command
            .args
            .extend(["--companion".into(), companion.into()]);
    }
    if let Some(provider) = workspace_agent_provider(agent_id) {
        command.args.extend(["--provider".into(), provider]);
    }
    if let Err(detail) = spawn_detached(app.clone(), command, None) {
        return Err(LaunchError::AgentLaunchWorkerUnavailable { detail });
    }
    Ok(app)
}
