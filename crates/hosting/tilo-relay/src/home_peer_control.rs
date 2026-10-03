//! Polkit-facing, narrowly scoped home-peer configuration broker.

use getrandom::{SysRng, rand_core::UnwrapErr};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tilo_relay::{DeviceRole, RelayConfig, ReplicationIntent, WorkspaceBinding};

const RELAY_CONFIG: &str = "/etc/tilo/relay.toml";
const HOME_PEER_CONFIG: &str = "/etc/tilo/home-peer.toml";
const MAX_REQUEST: u64 = 64 * 1024;
const MAX_HOME_PEER_EXPORT: u64 = 8 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "tilo-home-peer-control")]
struct Cli {
    #[command(subcommand)]
    command: BrokerCommand,
}

#[derive(Subcommand)]
enum BrokerCommand {
    /// Apply the bounded JSON request read from stdin and restart affected services.
    Apply,
    /// Open a one-minute LAN enrollment approval window.
    ApproveEnrollment {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long = "workspace-id")]
        workspace_ids: Vec<u64>,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
    },
    /// Receive an existing identity on this new personal device.
    PairDevice {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long)]
        label: String,
        #[arg(long)]
        status_file: PathBuf,
        #[arg(long)]
        requester_pid: u32,
        #[arg(long)]
        requester_start_time: u64,
    },
    /// Approve an ordinary nearby personal device.
    ApproveDevice {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        identity_dir: PathBuf,
        #[arg(long)]
        expected_device: String,
        #[arg(long)]
        expected_service: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each boolean is an independently authorized relay capability"
)]
struct ApplyRequest {
    identity_root: PathBuf,
    role: String,
    workspace_ids: Vec<u64>,
    replication: String,
    inbound_relay: bool,
    remote_control_relay: bool,
    app_serving: bool,
    compute: bool,
}

fn require_root() -> anyhow::Result<()> {
    if std::fs::metadata("/proc/self")?.uid() != 0 {
        bail!("tilo-home-peer-control must be started through polkit");
    }
    Ok(())
}

fn read_request() -> anyhow::Result<ApplyRequest> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_REQUEST {
        bail!("configuration request is too large");
    }
    serde_json::from_slice(&bytes).context("invalid configuration request")
}

fn desired_config(request: &ApplyRequest) -> anyhow::Result<RelayConfig> {
    if request.workspace_ids.len() > 4096 {
        bail!("too many workspaces");
    }
    let mut workspace_ids = request.workspace_ids.clone();
    workspace_ids.sort_unstable();
    workspace_ids.dedup();
    let mut config = if Path::new(RELAY_CONFIG).exists() {
        RelayConfig::load(Path::new(RELAY_CONFIG))?
    } else {
        RelayConfig::default()
    };
    config.device_role = match request.role.as_str() {
        "interactive" => DeviceRole::Interactive,
        "hybrid" => DeviceRole::Hybrid,
        "always-on" => DeviceRole::AlwaysOnServer,
        _ => bail!("unknown device role"),
    };
    let serving = config.device_role.can_serve();
    let full = config.device_role == DeviceRole::AlwaysOnServer;
    let requested_replication = match request.replication.as_str() {
        "workspace-only" => ReplicationIntent::WorkspaceOnly,
        "on-demand" => ReplicationIntent::OnDemand,
        "full" => ReplicationIntent::FullWorkspace,
        _ => bail!("unknown replication intent"),
    };
    let replication = if full {
        ReplicationIntent::FullWorkspace
    } else {
        requested_replication
    };
    config.services.workspace_replication = serving;
    config.services.full_content_replication = full;
    config.services.inbound_relay = serving && request.inbound_relay;
    config.services.remote_control_relay = serving && request.remote_control_relay;
    config.services.app_serving = serving && request.app_serving;
    config.services.compute = serving && request.compute;
    config.workspaces = workspace_ids
        .into_iter()
        .map(|id| WorkspaceBinding {
            id,
            pin: serving,
            replication: replication.clone(),
            admitted_users: Vec::new(),
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
        })
        .collect();
    Ok(config)
}

fn service(action: &str, unit: &str) -> anyhow::Result<()> {
    let output = bounded_command::run(
        "systemctl",
        &[action, unit],
        None,
        64 * 1024,
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        bail!("systemctl {action} {unit} failed");
    }
    Ok(())
}

fn export_local_home_peer(identity_root: &Path, workspace_ids: &[u64]) -> anyhow::Result<Vec<u8>> {
    let mut random = [0_u8; 16];
    getrandom::rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), &mut random);
    let temp_dir = PathBuf::from(format!(
        "/etc/tilo/.home-peer-export-{}-{}",
        std::process::id(),
        hex::encode(random)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&temp_dir)
        .context("create private full-sync export directory")?;
    let output = temp_dir.join("bundle.toml");
    let mut command = Command::new("/usr/bin/fs");
    command
        .env("TILO_IDENTITY_ROOT", identity_root)
        .args([
            "export-home-peer",
            "--cache-dir",
            "/var/cache/quick-fs",
            "--acl",
            "/var/lib/tilo/fs-capabilities.json",
            "--output",
        ])
        .arg(&output);
    for id in workspace_ids {
        command.args(["--relay-workspace-id", &id.to_string()]);
    }
    let result = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(60));
    let bytes = match result {
        Ok(result) if result.status.success() => (|| -> std::io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            std::fs::File::open(&output)?
                .take(MAX_HOME_PEER_EXPORT.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_HOME_PEER_EXPORT {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "full workspace sync export is too large",
                ))
            } else {
                Ok(bytes)
            }
        })(),
        Ok(_) => Err(std::io::Error::other(
            "could not prepare full workspace sync",
        )),
        Err(error) => Err(std::io::Error::other(error)),
    };
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_dir(&temp_dir);
    let bytes = bytes.context("prepare local full-sync export")?;
    let _: fs::full_replica::HomePeerSet = toml::from_str(std::str::from_utf8(&bytes)?)?;
    Ok(bytes)
}

fn needs_local_home_peer_export(config: &RelayConfig, home_peer_exists: bool) -> bool {
    config.device_role == DeviceRole::AlwaysOnServer && !home_peer_exists
}

fn project_home_peer_workspaces(bytes: &[u8], selected_ids: &[u64]) -> anyhow::Result<Vec<u8>> {
    let mut home: fs::full_replica::HomePeerSet = toml::from_str(std::str::from_utf8(bytes)?)?;
    fs::full_replica::validate_home_peer_set(&home)?;
    let workspace = home
        .workspaces
        .first_mut()
        .context("home-peer bundle has no global Files workspace")?;
    let governor = workspace
        .authority
        .as_ref()
        .context("home-peer workspace is missing signed authority")?
        .governor;
    let mut projected = selected_ids.to_vec();
    projected.push(tilo_relay::personal_continuity_workspace_id(&governor));
    projected.sort_unstable();
    projected.dedup();
    if projected.len() > 4096 {
        bail!("too many workspace mappings");
    }
    workspace.relay_workspace_ids = projected;
    toml::to_string_pretty(&home)
        .map(String::into_bytes)
        .map_err(Into::into)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServiceState {
    enabled: bool,
    active: bool,
}

fn service_state(unit: &str) -> anyhow::Result<ServiceState> {
    let query = |action| {
        bounded_command::run(
            "systemctl",
            &[action, "--quiet", unit],
            None,
            64 * 1024,
            Duration::from_secs(10),
        )
    };
    let enabled = query("is-enabled")?.status.success();
    let active = query("is-active")?.status.success();
    Ok(ServiceState { enabled, active })
}

fn restore_service_state(unit: &str, state: ServiceState) -> anyhow::Result<()> {
    let (enable_action, active_action) = service_state_actions(state);
    service(enable_action, unit)?;
    service(active_action, unit)
}

const fn service_state_actions(state: ServiceState) -> (&'static str, &'static str) {
    (
        if state.enabled { "enable" } else { "disable" },
        // `restart`, not `start`: the unit may still be active with the rejected
        // configuration after a later unit failed. Restoring bytes alone does
        // not restore runtime state.
        if state.active { "restart" } else { "stop" },
    )
}

fn apply_services(config: &RelayConfig) -> anyhow::Result<()> {
    if config.device_role.can_serve() {
        service("enable", "tilo-relay.service")?;
        service("restart", "tilo-relay.service")?;
    } else {
        service("disable", "tilo-relay.service")?;
        service("stop", "tilo-relay.service")?;
    }
    if config.device_role == DeviceRole::AlwaysOnServer {
        service("enable", "tilo-fs-replica.service")?;
        service("restart", "tilo-fs-replica.service")?;
    } else {
        service("disable", "tilo-fs-replica.service")?;
        service("stop", "tilo-fs-replica.service")?;
    }
    Ok(())
}

fn restore(path: &Path, previous: Option<&[u8]>) -> anyhow::Result<()> {
    match previous {
        Some(bytes) => tilo_files::native::atomic::write(path, bytes).map_err(Into::into),
        None if path.exists() => std::fs::remove_file(path).map_err(Into::into),
        None => Ok(()),
    }
}

fn commit_transaction(
    relay_path: &Path,
    relay_bytes: &[u8],
    home_path: &Path,
    home_bytes: Option<&[u8]>,
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let previous = std::fs::read(relay_path).ok();
    let previous_home = std::fs::read(home_path).ok();
    tilo_files::native::atomic::write(relay_path, relay_bytes)?;
    if let Some(bytes) = home_bytes
        && let Err(error) = tilo_files::native::atomic::write(home_path, bytes)
    {
        restore(relay_path, previous.as_deref())?;
        return Err(error.into());
    }
    if let Err(error) = restart() {
        restore(relay_path, previous.as_deref())?;
        restore(home_path, previous_home.as_deref())?;
        return Err(error.context("configuration was rolled back"));
    }
    Ok(())
}

fn apply(request: &ApplyRequest) -> anyhow::Result<()> {
    std::fs::create_dir_all("/etc/tilo")?;
    let uid = invoking_uid()?;
    let identity_root = validate_identity_root(&request.identity_root, uid, &home_for_uid(uid)?)?;
    let previous_relay_state = service_state("tilo-relay.service")?;
    let previous_fs_state = service_state("tilo-fs-replica.service")?;
    let config = desired_config(request)?;
    let bytes = toml::to_string_pretty(&config)?;
    let home_bytes = if Path::new(HOME_PEER_CONFIG).is_file() {
        let bytes = std::fs::read(HOME_PEER_CONFIG)?;
        Some(bytes)
    } else if needs_local_home_peer_export(&config, false) {
        Some(export_local_home_peer(
            &identity_root,
            &request.workspace_ids,
        )?)
    } else {
        None
    };
    let home_bytes = home_bytes
        .as_deref()
        .map(|bytes| project_home_peer_workspaces(bytes, &request.workspace_ids))
        .transpose()?;
    let result = commit_transaction(
        Path::new(RELAY_CONFIG),
        bytes.as_bytes(),
        Path::new(HOME_PEER_CONFIG),
        home_bytes.as_deref(),
        || apply_services(&config),
    );
    if result.is_err() {
        let relay_restore = restore_service_state("tilo-relay.service", previous_relay_state);
        let fs_restore = restore_service_state("tilo-fs-replica.service", previous_fs_state);
        if let (Err(relay), Err(filesystem)) = (&relay_restore, &fs_restore) {
            bail!(
                "restore previous relay service state: {relay}; restore previous filesystem service state: {filesystem}"
            );
        }
        relay_restore.context("restore previous relay service state")?;
        fs_restore.context("restore previous filesystem service state")?;
    }
    result
}

fn invoking_uid() -> anyhow::Result<u32> {
    std::env::var("PKEXEC_UID")
        .context("polkit did not identify the requesting user")?
        .parse()
        .context("polkit supplied an invalid requesting user")
}

#[derive(Debug, PartialEq, Eq)]
struct Account {
    gid: u32,
    home: PathBuf,
}

fn account_from_passwd(passwd: &str, uid: u32) -> Option<Account> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let _name = fields.next()?;
        let _password = fields.next()?;
        let record_uid = fields.next()?.parse::<u32>().ok()?;
        let gid = fields.next()?.parse::<u32>().ok()?;
        let _gecos = fields.next()?;
        let home = fields.next()?;
        (record_uid == uid).then(|| Account {
            gid,
            home: PathBuf::from(home),
        })
    })
}

fn account_for_uid(uid: u32) -> anyhow::Result<Account> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    account_from_passwd(&passwd, uid).context("requesting user's home directory was not found")
}

fn home_for_uid(uid: u32) -> anyhow::Result<PathBuf> {
    Ok(account_for_uid(uid)?.home)
}

fn run_enrollment_as_identity_owner(
    command: &mut Command,
    identity_root: &Path,
    uid: u32,
    gid: u32,
) {
    if identity_root != Path::new("/var/lib/tilo") {
        // `uid` also clears inherited supplementary groups, so the child gets
        // only the requesting account's primary identity rather than any of
        // the privileged helper's ambient root groups.
        command.gid(gid).uid(uid);
    }
}

fn validate_identity_root(identity_root: &Path, uid: u32, home: &Path) -> anyhow::Result<PathBuf> {
    let canonical = identity_root
        .canonicalize()
        .context("identity root does not exist")?;
    if canonical == Path::new("/var/lib/tilo") {
        return Ok(canonical);
    }
    let canonical_home = home.canonicalize().context("user home does not exist")?;
    let metadata = std::fs::metadata(&canonical)?;
    if !canonical.starts_with(&canonical_home)
        || canonical.file_name().and_then(|name| name.to_str()) != Some("tilo")
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        bail!("identity root must be the requesting user's private Tilo data directory");
    }
    Ok(canonical)
}

fn approve_enrollment(
    identity_root: &Path,
    workspace_ids: Vec<u64>,
    expected_device: Option<String>,
    expected_service: Option<String>,
) -> anyhow::Result<()> {
    if workspace_ids.len() > 4096 {
        bail!("too many workspaces");
    }
    let uid = invoking_uid()?;
    let account = account_for_uid(uid)?;
    let identity_root = validate_identity_root(identity_root, uid, &account.home)?;
    let mut command = Command::new("/usr/bin/tilo-enroll");
    command
        .args(["approve", "--timeout", "60", "--identity-root"])
        .arg(&identity_root);
    for id in workspace_ids {
        command.args(["--relay-workspace-id", &id.to_string()]);
    }
    match (expected_device, expected_service) {
        (Some(device), Some(service)) => {
            if device.len() != 64
                || service.len() != 64
                || hex::decode(&device).is_err()
                || hex::decode(&service).is_err()
            {
                bail!("invalid nearby home-server identity");
            }
            command.args(["--expected-device", &device]);
            command.args(["--expected-service", &service]);
        }
        (None, None) => {}
        _ => bail!("nearby device and service identities must be supplied together"),
    }
    run_enrollment_as_identity_owner(&mut command, &identity_root, uid, account.gid);
    let output =
        bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(90))?;
    enrollment_command_result(&output, "home-server enrollment was not completed")
}

fn validate_identity_dir(path: &Path, uid: u32, home: &Path) -> anyhow::Result<PathBuf> {
    let canonical_home = home.canonicalize().context("user home does not exist")?;
    let canonical = path
        .canonicalize()
        .context("identity directory does not exist")?;
    let metadata = std::fs::metadata(&canonical)?;
    if !canonical.starts_with(canonical_home)
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        bail!("identity directory must be the requesting user's private data directory");
    }
    Ok(canonical)
}

fn validate_peer_ids(device: &str, service: &str) -> anyhow::Result<()> {
    if device.len() != 64
        || service.len() != 64
        || hex::decode(device)
            .ok()
            .is_none_or(|bytes| bytes.len() != 32)
        || hex::decode(service)
            .ok()
            .is_none_or(|bytes| bytes.len() != 32)
    {
        bail!("invalid nearby device identity");
    }
    Ok(())
}

fn enrollment_command_result(
    output: &bounded_command::Output,
    incomplete: &'static str,
) -> anyhow::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        bail!("{incomplete} ({})", output.status);
    }
    bail!("{incomplete}: {detail}")
}

fn personal_device_command(
    operation: &str,
    identity_root: &Path,
    identity_dir: &Path,
    extra: impl FnOnce(&mut Command),
) -> anyhow::Result<()> {
    let uid = invoking_uid()?;
    let account = account_for_uid(uid)?;
    let identity_root = validate_identity_root(identity_root, uid, &account.home)?;
    let identity_dir = validate_identity_dir(identity_dir, uid, &account.home)?;
    let mut command = Command::new("/usr/bin/tilo-enroll");
    command
        .arg(operation)
        .arg("--identity-root")
        .arg(&identity_root)
        .arg("--identity-dir")
        .arg(identity_dir);
    extra(&mut command);
    run_enrollment_as_identity_owner(&mut command, &identity_root, uid, account.gid);
    let output =
        bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(600))?;
    enrollment_command_result(&output, "personal-device enrollment was not completed")
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

fn run() -> anyhow::Result<()> {
    require_root()?;
    match Cli::parse().command {
        BrokerCommand::Apply => apply(&read_request()?),
        BrokerCommand::ApproveEnrollment {
            identity_root,
            workspace_ids,
            expected_device,
            expected_service,
        } => approve_enrollment(
            &identity_root,
            workspace_ids,
            expected_device,
            expected_service,
        ),
        BrokerCommand::PairDevice {
            identity_root,
            identity_dir,
            label,
            status_file,
            requester_pid,
            requester_start_time,
        } => {
            let status_parent = status_file.parent().context("status file has no parent")?;
            if status_parent != identity_dir {
                bail!("pairing status must be stored in the identity directory");
            }
            let requester =
                tilo_relay::ProcessLease::from_parts(requester_pid, requester_start_time);
            requester
                .validate_owner(invoking_uid()?)
                .context("pairing requester is no longer the invoking user process")?;
            personal_device_command("pair-device", &identity_root, &identity_dir, |command| {
                command
                    .args(["--label", &label, "--status-file"])
                    .arg(status_file)
                    .arg("--requester-pid")
                    .arg(requester_pid.to_string())
                    .arg("--requester-start-time")
                    .arg(requester_start_time.to_string());
            })
        }
        BrokerCommand::ApproveDevice {
            identity_root,
            identity_dir,
            expected_device,
            expected_service,
        } => {
            validate_peer_ids(&expected_device, &expected_service)?;
            personal_device_command("approve-device", &identity_root, &identity_dir, |command| {
                command
                    .args(["--expected-device", &expected_device])
                    .args(["--expected-service", &expected_service]);
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn enrollment_requests_run_as_the_invoking_user_with_bounded_diagnostics() {
        // Phase `passwd_lookup_keeps_the_requesting_primary_group_and_home`.
        {
            let passwd = "root:x:0:0:root:/root:/bin/bash\ntilouser:x:42424:31337::/home/tilouser:/sbin/nologin\n";
            assert_eq!(
                account_from_passwd(passwd, 42_424),
                Some(Account {
                    gid: 31_337,
                    home: PathBuf::from("/home/tilouser"),
                })
            );
            assert_eq!(account_from_passwd(passwd, 7), None);
        }
        // Phase `enrollment_failure_preserves_the_bounded_child_diagnostic`.
        {
            let error = enrollment_command_result(
                &bounded_command::Output {
                    status: std::process::ExitStatus::from_raw(1 << 8),
                    stdout: Vec::new(),
                    stderr: b"requester lease expired\n".to_vec(),
                },
                "personal-device enrollment was not completed",
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "personal-device enrollment was not completed: requester lease expired"
            );
        }
        // Phase `enrollment_failure_without_stderr_still_reports_exit_status`.
        {
            let error = enrollment_command_result(
                &bounded_command::Output {
                    status: std::process::ExitStatus::from_raw(2 << 8),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
                "home-server enrollment was not completed",
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "home-server enrollment was not completed (exit status: 2)"
            );
        }
        // Phase `personal_device_ids_are_exact_ed25519_width_hex`.
        {
            assert!(validate_peer_ids(&"11".repeat(32), &"22".repeat(32)).is_ok());
            assert!(validate_peer_ids("11", &"22".repeat(32)).is_err());
            assert!(validate_peer_ids(&"zz".repeat(32), &"22".repeat(32)).is_err());
        }
        // Phase `request_projects_only_bounded_role_services_and_workspace_ids`.
        {
            let request = ApplyRequest {
                identity_root: PathBuf::from("/var/lib/tilo"),
                role: "always-on".into(),
                workspace_ids: vec![9, 2, 9],
                replication: "full".into(),
                inbound_relay: true,
                remote_control_relay: false,
                app_serving: false,
                compute: false,
            };
            let config = desired_config(&request).unwrap();
            assert_eq!(config.device_role, DeviceRole::AlwaysOnServer);
            assert_eq!(
                config
                    .workspaces
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                vec![2, 9]
            );
            assert!(
                config
                    .workspaces
                    .iter()
                    .all(|entry| entry.pin && entry.replication == ReplicationIntent::FullWorkspace)
            );
        }
        // Phase `approval_identity_root_is_owned_and_confined_to_the_invoking_home`.
        {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let identity = home.join(".local/share/tilo");
            std::fs::create_dir_all(&identity).unwrap();
            let uid = std::fs::metadata("/proc/self").unwrap().uid();
            assert_eq!(
                validate_identity_root(&identity, uid, &home).unwrap(),
                identity.canonicalize().unwrap()
            );

            let outside = dir.path().join("outside/tilo");
            std::fs::create_dir_all(&outside).unwrap();
            assert!(validate_identity_root(&outside, uid, &home).is_err());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn config_and_workspace_changes_reload_or_roll_back() {
        // Phase `failed_reload_rolls_back_config`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            tilo_files::native::atomic::write(&path, b"old").unwrap();
            let home = dir.path().join("home.toml");
            tilo_files::native::atomic::write(&home, b"old-home").unwrap();
            let result = commit_transaction(&path, b"new", &home, Some(b"new-home"), || {
                bail!("reload failed")
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"old");
            assert_eq!(std::fs::read(&home).unwrap(), b"old-home");
        }
        // Phase `successful_reload_keeps_new_config`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("relay.toml");
            let home = dir.path().join("home.toml");
            commit_transaction(&path, b"new", &home, Some(b"new-home"), || Ok(())).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
            assert_eq!(std::fs::read(&home).unwrap(), b"new-home");
        }
        // Phase `service_rollback_restores_enabled_and_active_independently`.
        {
            assert_eq!(
                service_state_actions(ServiceState {
                    enabled: true,
                    active: false,
                }),
                ("enable", "stop")
            );
            assert_eq!(
                service_state_actions(ServiceState {
                    enabled: false,
                    active: true,
                }),
                ("disable", "restart")
            );
        }
        // Phase `stock_desktop_always_on_selection_self_provisions_full_sync`.
        {
            let config = desired_config(&ApplyRequest {
                identity_root: PathBuf::from("/var/lib/tilo"),
                role: "always-on".into(),
                workspace_ids: vec![12],
                replication: "on-demand".into(),
                inbound_relay: true,
                remote_control_relay: false,
                app_serving: false,
                compute: false,
            })
            .unwrap();
            assert!(needs_local_home_peer_export(&config, false));
            assert!(!needs_local_home_peer_export(&config, true));
        }
        // Phase `workspace_add_remove_reloads_mapping_and_keeps_continuity`.
        {
            let dir = tempfile::tempdir().unwrap();
            let relay = dir.path().join("relay.toml");
            let home = dir.path().join("home.toml");
            let governor = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
            let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
                uuid::Uuid::from_bytes([3; 16]),
                1,
                Vec::new(),
                false,
                &governor,
            )
            .unwrap();
            let bundle = fs::full_replica::HomePeerSet {
                status_file: dir.path().join("status.json"),
                workspaces: vec![fs::full_replica::HomePeerBundle {
                    ticket: "test-ticket".into(),
                    cache_dir: dir.path().join("cache"),
                    capability_json: "[]".into(),
                    authority: Some(authority),
                    principals: Vec::new(),
                    relay_workspace_ids: vec![7, 8],
                }],
            };
            let original = toml::to_string_pretty(&bundle).unwrap();
            tilo_files::native::atomic::write(&home, original.as_bytes()).unwrap();
            let continuity =
                tilo_relay::personal_continuity_workspace_id(&governor.verifying_key().to_bytes());

            let added = project_home_peer_workspaces(original.as_bytes(), &[8, 9]).unwrap();
            commit_transaction(&relay, b"relay-add", &home, Some(&added), || Ok(())).unwrap();
            let after_add: fs::full_replica::HomePeerSet =
                toml::from_str(&std::fs::read_to_string(&home).unwrap()).unwrap();
            assert_eq!(after_add.workspaces[0].relay_workspace_ids, {
                let mut expected = vec![8, 9, continuity];
                expected.sort_unstable();
                expected.dedup();
                expected
            });

            let removed = project_home_peer_workspaces(&added, &[9]).unwrap();
            commit_transaction(&relay, b"relay-remove", &home, Some(&removed), || Ok(())).unwrap();
            let after_remove: fs::full_replica::HomePeerSet =
                toml::from_str(&std::fs::read_to_string(&home).unwrap()).unwrap();
            assert_eq!(after_remove.workspaces[0].relay_workspace_ids, {
                let mut expected = vec![9, continuity];
                expected.sort_unstable();
                expected.dedup();
                expected
            });
        }
    }
}
