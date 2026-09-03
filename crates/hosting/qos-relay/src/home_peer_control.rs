//! Polkit-facing, narrowly scoped home-peer configuration broker.

use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use qos_relay::{DeviceRole, RelayConfig, ReplicationIntent, WorkspaceBinding};
use serde::Deserialize;

const RELAY_CONFIG: &str = "/etc/qos/relay.toml";
const HOME_PEER_CONFIG: &str = "/etc/qos/home-peer.toml";
const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Parser)]
#[command(name = "qos-home-peer-control")]
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
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    identity_root: PathBuf,
    role: String,
    workspace_ids: Vec<u64>,
    inbound_relay: bool,
    remote_control_relay: bool,
    app_serving: bool,
    compute: bool,
}

fn require_root() -> anyhow::Result<()> {
    if std::fs::metadata("/proc/self")?.uid() != 0 {
        bail!("qos-home-peer-control must be started through polkit");
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
            replication: if full {
                ReplicationIntent::FullWorkspace
            } else {
                ReplicationIntent::OnDemand
            },
            admitted_users: Vec::new(),
        })
        .collect();
    Ok(config)
}

fn service(action: &str, unit: &str) -> anyhow::Result<()> {
    let status = Command::new("systemctl").args([action, unit]).status()?;
    if !status.success() {
        bail!("systemctl {action} {unit} failed");
    }
    Ok(())
}

fn export_local_home_peer(identity_root: &Path, workspace_ids: &[u64]) -> anyhow::Result<Vec<u8>> {
    let mut random = [0_u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let temp_dir = PathBuf::from(format!(
        "/etc/qos/.home-peer-export-{}-{}",
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
        .env("QOS_IDENTITY_ROOT", identity_root)
        .args([
            "export-home-peer",
            "--cache-dir",
            "/var/cache/quick-fs",
            "--acl",
            "/var/lib/qos/fs-capabilities.json",
            "--output",
        ])
        .arg(&output);
    for id in workspace_ids {
        command.args(["--relay-workspace-id", &id.to_string()]);
    }
    let bytes = match command.output() {
        Ok(result) if result.status.success() => std::fs::read(&output),
        Ok(result) => {
            let detail = String::from_utf8_lossy(&result.stderr);
            Err(std::io::Error::other(format!(
                "could not prepare full workspace sync: {}",
                detail.trim()
            )))
        }
        Err(error) => Err(error),
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
    projected.push(qos_relay::personal_continuity_workspace_id(&governor));
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
    let enabled = Command::new("systemctl")
        .args(["is-enabled", "--quiet", unit])
        .status()?
        .success();
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()?
        .success();
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
        if state.active { "start" } else { "stop" },
    )
}

fn apply_services(config: &RelayConfig) -> anyhow::Result<()> {
    if config.device_role.can_serve() {
        service("enable", "qos-relay.service")?;
        service("restart", "qos-relay.service")?;
    } else {
        service("disable", "qos-relay.service")?;
        service("stop", "qos-relay.service")?;
    }
    if config.device_role == DeviceRole::AlwaysOnServer {
        service("enable", "qos-fs-replica.service")?;
        service("restart", "qos-fs-replica.service")?;
    } else {
        service("disable", "qos-fs-replica.service")?;
        service("stop", "qos-fs-replica.service")?;
    }
    Ok(())
}

fn restore(path: &Path, previous: &Option<Vec<u8>>) -> anyhow::Result<()> {
    match previous {
        Some(bytes) => atomic_file::write(path, bytes).map_err(Into::into),
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
    atomic_file::write(relay_path, relay_bytes)?;
    if let Some(bytes) = home_bytes {
        if let Err(error) = atomic_file::write(home_path, bytes) {
            restore(relay_path, &previous)?;
            return Err(error.into());
        }
    }
    if let Err(error) = restart() {
        restore(relay_path, &previous)?;
        restore(home_path, &previous_home)?;
        return Err(error.context("configuration was rolled back"));
    }
    Ok(())
}

fn apply(request: ApplyRequest) -> anyhow::Result<()> {
    std::fs::create_dir_all("/etc/qos")?;
    let uid = invoking_uid()?;
    let identity_root = validate_identity_root(&request.identity_root, uid, &home_for_uid(uid)?)?;
    let previous_relay_state = service_state("qos-relay.service")?;
    let previous_fs_state = service_state("qos-fs-replica.service")?;
    let config = desired_config(&request)?;
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
        let relay_restore = restore_service_state("qos-relay.service", previous_relay_state);
        let fs_restore = restore_service_state("qos-fs-replica.service", previous_fs_state);
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

fn home_for_uid(uid: u32) -> anyhow::Result<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    passwd
        .lines()
        .filter_map(|line| {
            let fields = line.split(':').collect::<Vec<_>>();
            (fields.len() >= 6 && fields[2].parse::<u32>().ok() == Some(uid))
                .then(|| PathBuf::from(fields[5]))
        })
        .next()
        .context("requesting user's home directory was not found")
}

fn validate_identity_root(identity_root: &Path, uid: u32, home: &Path) -> anyhow::Result<PathBuf> {
    let canonical = identity_root
        .canonicalize()
        .context("identity root does not exist")?;
    if canonical == Path::new("/var/lib/qos") {
        return Ok(canonical);
    }
    let canonical_home = home.canonicalize().context("user home does not exist")?;
    let metadata = std::fs::metadata(&canonical)?;
    if !canonical.starts_with(&canonical_home)
        || canonical.file_name().and_then(|name| name.to_str()) != Some("qos")
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        bail!("identity root must be the requesting user's private qOS data directory");
    }
    Ok(canonical)
}

fn approve_enrollment(identity_root: PathBuf, workspace_ids: Vec<u64>) -> anyhow::Result<()> {
    if workspace_ids.len() > 4096 {
        bail!("too many workspaces");
    }
    let uid = invoking_uid()?;
    let identity_root = validate_identity_root(&identity_root, uid, &home_for_uid(uid)?)?;
    let mut command = Command::new("/usr/bin/qos-enroll");
    command
        .args(["approve", "--timeout", "60", "--identity-root"])
        .arg(identity_root);
    for id in workspace_ids {
        command.args(["--relay-workspace-id", &id.to_string()]);
    }
    let status = command.stdin(Stdio::null()).status()?;
    if !status.success() {
        bail!("home-server enrollment was not completed");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    require_root()?;
    match Cli::parse().command {
        BrokerCommand::Apply => apply(read_request()?),
        BrokerCommand::ApproveEnrollment {
            identity_root,
            workspace_ids,
        } => approve_enrollment(identity_root, workspace_ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_projects_only_bounded_role_services_and_workspace_ids() {
        let request = ApplyRequest {
            identity_root: PathBuf::from("/var/lib/qos"),
            role: "always-on".into(),
            workspace_ids: vec![9, 2, 9],
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

    #[test]
    fn failed_reload_rolls_back_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        atomic_file::write(&path, b"old").unwrap();
        let home = dir.path().join("home.toml");
        atomic_file::write(&home, b"old-home").unwrap();
        let result = commit_transaction(&path, b"new", &home, Some(b"new-home"), || {
            bail!("reload failed")
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_eq!(std::fs::read(&home).unwrap(), b"old-home");
    }

    #[test]
    fn successful_reload_keeps_new_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay.toml");
        let home = dir.path().join("home.toml");
        commit_transaction(&path, b"new", &home, Some(b"new-home"), || Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read(&home).unwrap(), b"new-home");
    }

    #[test]
    fn service_rollback_restores_enabled_and_active_independently() {
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
            ("disable", "start")
        );
    }

    #[test]
    fn approval_identity_root_is_owned_and_confined_to_the_invoking_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let identity = home.join(".local/share/qos");
        std::fs::create_dir_all(&identity).unwrap();
        let uid = std::fs::metadata("/proc/self").unwrap().uid();
        assert_eq!(
            validate_identity_root(&identity, uid, &home).unwrap(),
            identity.canonicalize().unwrap()
        );

        let outside = dir.path().join("outside/qos");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(validate_identity_root(&outside, uid, &home).is_err());
    }

    #[test]
    fn stock_desktop_always_on_selection_self_provisions_full_sync() {
        let config = desired_config(&ApplyRequest {
            identity_root: PathBuf::from("/var/lib/qos"),
            role: "always-on".into(),
            workspace_ids: vec![12],
            inbound_relay: true,
            remote_control_relay: false,
            app_serving: false,
            compute: false,
        })
        .unwrap();
        assert!(needs_local_home_peer_export(&config, false));
        assert!(!needs_local_home_peer_export(&config, true));
    }

    #[test]
    fn workspace_add_remove_reloads_mapping_and_keeps_continuity() {
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
        atomic_file::write(&home, original.as_bytes()).unwrap();
        let continuity =
            qos_relay::personal_continuity_workspace_id(&governor.verifying_key().to_bytes());

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
