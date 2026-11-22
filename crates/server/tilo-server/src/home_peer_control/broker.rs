//! Polkit-facing, narrowly scoped home-peer configuration broker.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tilo_enrollment::client::{NearbyApproval, approve_nearby_command};
use tilo_identity::device_admin::ENROLL;
use tilo_server::ServerConfig;
use tilo_server_protocol::WorkSetting;

const SERVER_CONFIG: &str = "/etc/tilo/server.toml";
const MAX_REQUEST: u64 = 64 * 1024;

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
        #[arg(long)]
        identity_dir: Option<PathBuf>,
        #[arg(long = "workspace-id")]
        workspace_ids: Vec<tilo_server_protocol::WorkspaceId>,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
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
    reason = "each boolean is an independently authorized server capability"
)]
struct ApplyRequest {
    identity_root: PathBuf,
    /// The work this device takes from other devices.
    work: WorkSetting,
    workspace_ids: Vec<tilo_server_protocol::WorkspaceId>,
    inbound_server: bool,
    remote_control_server: bool,
    app_serving: bool,
    compute: bool,
}

fn require_root() -> anyhow::Result<()> {
    if std::fs::metadata("/proc/self")?.uid() != 0 {
        bail!("tilo-home-peer-control must be started through polkit or sudo");
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

fn desired_config(request: &ApplyRequest) -> anyhow::Result<ServerConfig> {
    if request.workspace_ids.len() > 4096 {
        bail!("too many workspaces");
    }
    let mut config = if Path::new(SERVER_CONFIG).exists() {
        ServerConfig::load(Path::new(SERVER_CONFIG))?
    } else {
        ServerConfig::default()
    };
    config.work = request.work.clone();
    let serving = !config.work.takes_nothing();
    // Which workspaces this device holds, and how much of each, is the workspaces'
    // own hosting membership, not this file's.
    config.services.workspace_replication = serving;
    config.services.inbound_server = serving && request.inbound_server;
    config.services.remote_control_server = serving && request.remote_control_server;
    config.services.app_serving = serving && request.app_serving;
    config.services.compute = serving && request.compute;
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

fn takes_work(config: &ServerConfig) -> bool {
    !config.work.takes_nothing()
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

fn apply_services(config: &ServerConfig) -> anyhow::Result<()> {
    if takes_work(config) {
        service("enable", "tilo-server.service")?;
        service("restart", "tilo-server.service")?;
    } else {
        service("disable", "tilo-server.service")?;
        service("stop", "tilo-server.service")?;
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
    server_path: &Path,
    relay_bytes: &[u8],
    restart: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let previous = std::fs::read(server_path).ok();
    tilo_files::native::atomic::write(server_path, relay_bytes)?;
    if let Err(error) = restart() {
        restore(server_path, previous.as_deref())?;
        return Err(error.context("configuration was rolled back"));
    }
    Ok(())
}

fn apply(request: &ApplyRequest) -> anyhow::Result<()> {
    std::fs::create_dir_all("/etc/tilo")?;
    let uid = invoking_uid()?;
    validate_identity_root(&request.identity_root, uid, &home_for_uid(uid)?)?;
    let previous_server_state = service_state("tilo-server.service")?;
    let config = desired_config(request)?;
    let bytes = toml::to_string_pretty(&config)?;
    let result = commit_transaction(Path::new(SERVER_CONFIG), bytes.as_bytes(), || {
        apply_services(&config)
    });
    if result.is_err() {
        restore_service_state("tilo-server.service", previous_server_state)
            .context("restore previous server service state")?;
    }
    result
}

/// The account this root process acts for: whoever polkit authorised, or
/// whoever ran it through sudo (an SSH session has no polkit agent). Only
/// root starts this helper, so either variable comes from the launcher that
/// authenticated them.
fn invoking_uid() -> anyhow::Result<u32> {
    invoking_uid_from(
        std::env::var("PKEXEC_UID").ok().as_deref(),
        std::env::var("SUDO_UID").ok().as_deref(),
    )
}

fn invoking_uid_from(pkexec: Option<&str>, sudo: Option<&str>) -> anyhow::Result<u32> {
    let (value, launcher) = match (pkexec, sudo) {
        (Some(value), _) => (value, "polkit"),
        (None, Some(value)) => (value, "sudo"),
        (None, None) => bail!("neither polkit nor sudo identified the requesting user"),
    };
    value
        .parse()
        .with_context(|| format!("{launcher} supplied an invalid requesting user"))
}

#[derive(Debug, PartialEq, Eq)]
struct Account {
    name: String,
    gid: u32,
    home: PathBuf,
}

fn account_from_passwd(passwd: &str, uid: u32) -> Option<Account> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let record_uid = fields.next()?.parse::<u32>().ok()?;
        let gid = fields.next()?.parse::<u32>().ok()?;
        let _gecos = fields.next()?;
        let home = fields.next()?;
        (record_uid == uid).then(|| Account {
            name: name.to_owned(),
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

/// Every group `account` belongs to, the way a login gives them to it.
fn account_groups(account: &Account) -> anyhow::Result<Vec<libc::gid_t>> {
    let name = std::ffi::CString::new(account.name.as_str())?;
    let mut capacity: libc::c_int = 64;
    loop {
        let mut groups = vec![0; usize::try_from(capacity)?];
        let mut count = capacity;
        // SAFETY: `name` is NUL-terminated and `groups` has workspace for `count`
        // entries; getgrouplist writes at most that many and reports the
        // number it needs in `count`.
        let found = unsafe {
            libc::getgrouplist(
                name.as_ptr(),
                account.gid,
                groups.as_mut_ptr(),
                &raw mut count,
            )
        };
        if found >= 0 {
            groups.truncate(usize::try_from(count)?);
            return Ok(groups);
        }
        if count <= capacity || count > 65_536 {
            bail!("requesting user's groups could not be listed");
        }
        capacity = count;
    }
}

/// Run `command` with the invoking person's own credentials: their uid,
/// primary group and supplementary groups, and the session directories their
/// secret store answers on. Approving a device signs with the person's
/// identity record, which only its owner can open.
fn run_as_invoking_person(
    command: &mut Command,
    uid: u32,
    account: &Account,
) -> anyhow::Result<()> {
    let groups = account_groups(account)?;
    let gid = account.gid;
    let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
    command
        .env("HOME", &account.home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", runtime_dir.join("bus").display()),
        );
    // SAFETY: the closure runs in the forked child before exec and calls only
    // async-signal-safe credential syscalls on data prepared before the fork.
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
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
    identity_dir: Option<&Path>,
    workspace_ids: &[tilo_server_protocol::WorkspaceId],
    expected_device: Option<&str>,
    expected_service: Option<&str>,
) -> anyhow::Result<()> {
    if workspace_ids.len() > 4096 {
        bail!("too many workspaces");
    }
    let expected = match (expected_device, expected_service) {
        (Some(device), Some(service)) => {
            validate_peer_ids(device, service).context("invalid nearby home-server identity")?;
            Some((device, service))
        }
        (None, None) => None,
        _ => bail!("nearby device and service identities must be supplied together"),
    };
    let identity_dir = identity_dir.context("approving needs the person's identity directory")?;
    approve_nearby(
        identity_root,
        identity_dir,
        NearbyApproval::HomePeer {
            workspaces: workspace_ids,
            expected,
        },
        Duration::from_secs(90),
        "home-server enrollment was not completed",
    )
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

fn approve_nearby(
    identity_root: &Path,
    identity_dir: &Path,
    approval: NearbyApproval<'_>,
    timeout: Duration,
    incomplete: &'static str,
) -> anyhow::Result<()> {
    let uid = invoking_uid()?;
    let account = account_for_uid(uid)?;
    let identity_root = validate_identity_root(identity_root, uid, &account.home)?;
    let identity_dir = validate_identity_dir(identity_dir, uid, &account.home)?;
    // The directory can be replaced during the window. Sign and write as
    // the invoking person throughout the helper's lifetime.
    let mut command = approve_nearby_command(
        Path::new(ENROLL),
        &identity_root,
        &identity_dir,
        uid,
        approval,
    );
    run_as_invoking_person(&mut command, uid, &account)?;
    let output = bounded_command::run_prepared(command, None, 1024 * 1024, timeout)?;
    enrollment_command_result(&output, incomplete)
}

pub fn main() -> anyhow::Result<std::process::ExitCode> {
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
            identity_dir,
            workspace_ids,
            expected_device,
            expected_service,
        } => approve_enrollment(
            &identity_root,
            identity_dir.as_deref(),
            &workspace_ids,
            expected_device.as_deref(),
            expected_service.as_deref(),
        ),
        BrokerCommand::ApproveDevice {
            identity_root,
            identity_dir,
            expected_device,
            expected_service,
        } => {
            validate_peer_ids(&expected_device, &expected_service)?;
            approve_nearby(
                &identity_root,
                &identity_dir,
                NearbyApproval::PersonalDevice {
                    device: &expected_device,
                    service: &expected_service,
                },
                Duration::from_secs(600),
                "personal-device enrollment was not completed",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    tilo_test_env::scenario! {
        fn the_broker_acts_as_the_account_that_asked;

        phase the_requesting_account_comes_from_polkit_or_sudo;
        {
            assert_eq!(invoking_uid_from(Some("1000"), Some("1001")).unwrap(), 1000);
            assert_eq!(invoking_uid_from(None, Some("1001")).unwrap(), 1001);
            assert!(invoking_uid_from(None, None).is_err());
            assert!(invoking_uid_from(None, Some("person")).is_err());
        }

        phase enrollment_requests_run_as_the_invoking_user_with_bounded_diagnostics;
        {
            {
                let passwd = "root:x:0:0:root:/root:/bin/bash\ntilouser:x:42424:31337::/home/tilouser:/sbin/nologin\n";
                assert_eq!(
                    account_from_passwd(passwd, 42_424),
                    Some(Account {
                        name: "tilouser".into(),
                        gid: 31_337,
                        home: PathBuf::from("/home/tilouser"),
                    })
                );
                assert_eq!(account_from_passwd(passwd, 7), None);
            }
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
            {
                let request = ApplyRequest {
                    identity_root: PathBuf::from("/var/lib/tilo"),
                    work: WorkSetting::own_person(),
                    workspace_ids: vec![
                        tilo_server_protocol::WorkspaceId::from_u128(9),
                        tilo_server_protocol::WorkspaceId::from_u128(2),
                        tilo_server_protocol::WorkspaceId::from_u128(9),
                    ],
                    inbound_server: true,
                    remote_control_server: false,
                    app_serving: false,
                    compute: false,
                };
                let config = desired_config(&request).unwrap();
                assert!(!config.work.takes_nothing());
                assert!(config.services.inbound_server);
            }
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
    }

    #[test]
    fn config_changes_reload_or_roll_back() {
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("server.toml");
            tilo_files::native::atomic::write(&path, b"old").unwrap();
            let result = commit_transaction(&path, b"new", || bail!("reload failed"));
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"old");
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("server.toml");
            commit_transaction(&path, b"new", || Ok(())).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
        }
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
    }
}
