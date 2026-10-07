//! Existing ush guest confinement, wrapped by an owned aggregate cgroup.
use super::{ApprovedCheck, CheckLimits, ProjectBinding};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize)]
pub enum CheckOutcome {
    Exited,
    Cancelled,
    TimedOut,
    OutputLimit,
    Failed,
}

/// Observed supervision evidence, never a coding score or grader verdict.
#[derive(Debug, Serialize)]
pub struct CheckReceipt {
    pub binding: ProjectBinding,
    pub check_id: String,
    pub unit: String,
    pub config_sha256: String,
    pub runtime_binary_sha256: String,
    pub supervisor_source_sha256: String,
    pub limits: CheckLimits,
    pub cgroup: Option<CgroupObservation>,
    pub outcome: CheckOutcome,
    pub exit_code: Option<i32>,
    pub check_signal: Option<i32>,
    pub wrapper_exit_code: Option<i32>,
    pub service_result: Option<String>,
    pub service_state_before_cleanup: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub stdout_sha256: Option<String>,
    pub stderr_sha256: Option<String>,
    pub elapsed_ms: u128,
    pub failure: Option<String>,
    pub cleanup_observed: bool,
}

#[derive(Debug, Serialize)]
pub struct CgroupObservation {
    pub main_pid: u32,
    pub control_group: String,
    pub cpu_max: String,
    pub memory_max: String,
    pub memory_swap_max: String,
    pub pids_max: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    check: ApprovedCheck,
    token: String,
    runtime_binary_sha256: String,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn binary_hash(path: &Path) -> Result<String, String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 * 1024 {
        return Err("check runtime binary is not bounded".into());
    }
    let mut digest = Sha256::new();
    let mut chunk = vec![0_u8; 65536];
    let mut count = 0_u64;
    loop {
        let got = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if got == 0 {
            break;
        }
        count = count
            .checked_add(u64::try_from(got).map_err(|e| e.to_string())?)
            .ok_or("binary size overflow")?;
        if count > 1024 * 1024 * 1024 {
            return Err("check runtime binary grew beyond its bound".into());
        }
        digest.update(chunk.get(..got).ok_or("binary read length invalid")?);
    }
    Ok(hex::encode(digest.finalize()))
}

fn private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}

fn publish_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("private publication has no parent")?;
    let temporary = format!("publication-{}", uuid::Uuid::new_v4());
    let temporary_path = parent.join(&temporary);
    private_file(&temporary_path, bytes)?;
    let directory = tilo_files::Dir::open(tilo_files::Location::Native(parent.into()))
        .map_err(|e| e.to_string())?;
    let from = tilo_files::VPath::new(&temporary).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid publication name")?;
    let to = tilo_files::VPath::new(name).map_err(|e| e.to_string())?;
    let result =
        tilo_files::blocking::rename_noreplace(&directory, &from, &to).map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&temporary_path);
    result
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    token: String,
    config_sha256: String,
    runtime_binary_sha256: String,
    exit_code: Option<i32>,
    signal: Option<i32>,
}

fn completion(prepared: &Prepared, bytes: &[u8]) -> Result<Option<Completion>, String> {
    let path = prepared
        .check
        .scratch
        .join(format!("completion-{}", prepared.token));
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let value: Completion =
        serde_json::from_slice(&read_private(&path)?).map_err(|e| e.to_string())?;
    if value.token != prepared.token
        || value.config_sha256 != hash(bytes)
        || value.runtime_binary_sha256 != prepared.runtime_binary_sha256
        || !(value.exit_code.is_some() ^ value.signal.is_some())
        || value
            .exit_code
            .is_some_and(|code| !(0..=255).contains(&code))
        || value
            .signal
            .is_some_and(|signal| !(1..=64).contains(&signal))
    {
        return Err("approved check completion identity or exit status mismatched".into());
    }
    Ok(Some(value))
}

fn read_private(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no pointer inputs and cannot fail.
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 64 * 1024
    {
        return Err("approved check private input is not an owned bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("approved check input grew beyond its bound".into());
    }
    Ok(bytes)
}

fn control(args: &[&str]) -> Result<String, String> {
    let output = bounded_command::run(
        "/usr/bin/systemctl",
        args,
        None,
        16384,
        Duration::from_secs(2),
    )
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn property(unit: &str, key: &str) -> Result<String, String> {
    control(&["--user", "show", "--value", "--property", key, "--", unit])
        .map(|s| s.trim().to_owned())
}

fn observe(unit: &str, limits: &CheckLimits) -> Result<CgroupObservation, String> {
    let pid = property(unit, "MainPID")?
        .parse::<u32>()
        .map_err(|e| e.to_string())?;
    if pid == 0 {
        return Err("approved check has no observed main process yet".into());
    }
    let group = property(unit, "ControlGroup")?;
    if !group.starts_with('/')
        || group.split('/').any(|s| matches!(s, "." | ".."))
        || group == "/"
        || !group.ends_with(unit)
    {
        return Err("approved check cgroup is not the exact owned unit".into());
    }
    let membership =
        std::fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e| e.to_string())?;
    if !membership.lines().any(|s| s == format!("0::{group}")) {
        return Err("approved check main process has another cgroup".into());
    }
    let root = Path::new("/sys/fs/cgroup").join(group.trim_start_matches('/'));
    let read = |key: &str| {
        std::fs::read_to_string(root.join(key))
            .map(|s| s.trim().to_owned())
            .map_err(|e| e.to_string())
    };
    let observation = CgroupObservation {
        main_pid: pid,
        control_group: group,
        cpu_max: read("cpu.max")?,
        memory_max: read("memory.max")?,
        memory_swap_max: read("memory.swap.max")?,
        pids_max: read("pids.max")?,
    };
    validate_observation(&observation, limits)?;
    Ok(observation)
}

fn validate_observation(observed: &CgroupObservation, limits: &CheckLimits) -> Result<(), String> {
    let cpu: Vec<u64> = observed
        .cpu_max
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| "CPU quota is not bounded")?;
    let [quota, period] = cpu.as_slice() else {
        return Err("CPU quota has an invalid shape".into());
    };
    if *quota == 0
        || *period == 0
        || u128::from(*quota)
            .checked_mul(100)
            .ok_or("CPU quota overflow")?
            > u128::from(*period)
                .checked_mul(u128::from(limits.cpu_percent))
                .ok_or("CPU period overflow")?
        || observed
            .memory_max
            .parse::<u64>()
            .map_err(|_| "memory limit is not bounded")?
            > limits.memory_bytes
        || observed.memory_max == "0"
        || observed.memory_swap_max != "0"
        || observed
            .pids_max
            .parse::<u32>()
            .map_err(|_| "process limit is not bounded")?
            > limits.processes
        || observed.pids_max == "0"
    {
        return Err("aggregate cgroup bounds do not enforce the approved limits".into());
    }
    Ok(())
}

struct OwnedUnit(String);

#[derive(Debug)]
struct UnitState {
    load: String,
    active: String,
    pid: Option<u32>,
    job: String,
}

fn unit_state(text: &str) -> Result<UnitState, String> {
    let mut values = std::collections::BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=').ok_or("malformed manager unit state")?;
        if values.insert(key, value).is_some() {
            return Err("duplicate manager state property".into());
        }
    }
    let field = |key| {
        values
            .get(key)
            .map(|s| (*s).to_owned())
            .ok_or_else(|| "manager state property missing".to_owned())
    };
    let pid = values
        .get("MainPID")
        .map(|s| s.parse::<u32>().map_err(|e| e.to_string()))
        .transpose()?;
    Ok(UnitState {
        load: field("LoadState")?,
        active: field("ActiveState")?,
        pid,
        job: field("Job")?,
    })
}

fn quiescent(state: &UnitState) -> bool {
    matches!(state.job.as_str(), "" | "0")
        && matches!(state.active.as_str(), "inactive" | "failed")
        && (state.pid == Some(0) || (state.pid.is_none() && state.load == "not-found"))
        && matches!(state.load.as_str(), "loaded" | "not-found")
}

fn stopped_manager_unit(unit: &str) -> bool {
    if control(&["--user", "stop", "--", unit]).is_err() {
        return false;
    }
    let output = bounded_command::run(
        "/usr/bin/systemctl",
        &[
            "--user",
            "show",
            "--property=MainPID",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=Job",
            "--",
            unit,
        ],
        None,
        16384,
        Duration::from_secs(2),
    );
    let Ok(output) = output else {
        return false;
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return false;
    };
    let Ok(state) = unit_state(&text) else {
        return false;
    };
    // systemctl reports not-found with exit4; only the explicit fully observed
    // absent unit state may establish that this exact name has no pending work.
    (output.status.success() || (output.status.code() == Some(4) && state.load == "not-found"))
        && quiescent(&state)
}

impl OwnedUnit {
    fn cleanup(&self, observed: Option<&CgroupObservation>) -> bool {
        if !stopped_manager_unit(&self.0) {
            return false;
        }
        if let Some(observed) = observed {
            let root =
                Path::new("/sys/fs/cgroup").join(observed.control_group.trim_start_matches('/'));
            match std::fs::read_to_string(root.join("cgroup.events")) {
                Ok(events) => events.lines().any(|s| s == "populated 0"),
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            }
        } else {
            true
        }
    }
}
impl Drop for OwnedUnit {
    fn drop(&mut self) {
        let _ = self.cleanup(None);
    }
}

struct RunningCheck {
    unit: OwnedUnit,
    prepared: Prepared,
    config_bytes: Vec<u8>,
    child: bounded_command::AsyncCommand,
}

fn start(
    check: &ApprovedCheck,
    authority: &impl Fn() -> Result<(), String>,
) -> Result<RunningCheck, String> {
    let token = uuid::Uuid::new_v4().to_string();
    let unit = OwnedUnit(format!("tilo-approved-check-{token}.service"));
    let config = check.scratch.join(format!("check-{token}.json"));
    let prepared = Prepared {
        check: check.clone(),
        token,
        runtime_binary_sha256: binary_hash(&check.runtime_binary)?,
    };
    let bytes = serde_json::to_vec(&prepared).map_err(|e| e.to_string())?;
    private_file(&config, &bytes)?;
    let mut command = Command::new("/usr/bin/systemd-run");
    command.args([
        "--user",
        "--quiet",
        "--wait",
        "--pipe",
        "--service-type=exec",
        "--unit",
        &unit.0,
    ]);
    for property in [
        format!("CPUQuota={}%", check.limits.cpu_percent),
        format!("MemoryMax={}", check.limits.memory_bytes),
        "MemorySwapMax=0".into(),
        format!("TasksMax={}", check.limits.processes),
        "KillMode=control-group".into(),
        format!("RuntimeMaxSec={}", check.limits.wall_seconds),
        "TimeoutStopSec=2".into(),
    ] {
        command.arg("--property").arg(property);
    }
    command
        .arg("--")
        .arg(&check.runtime_binary)
        .arg("__approved-check-supervise")
        .arg(&config);
    authority()?;
    let child = bounded_command::spawn_prepared(
        command,
        None,
        check.limits.output_bytes,
        Duration::from_secs(u64::from(check.limits.wall_seconds).saturating_add(20)),
    )
    .map_err(|e| e.to_string())?;
    Ok(RunningCheck {
        unit,
        prepared,
        config_bytes: bytes,
        child,
    })
}

fn admit(
    check: &ApprovedCheck,
    token: &str,
    unit: &str,
    authority: &impl Fn() -> Result<(), String>,
) -> Result<CgroupObservation, (CheckOutcome, String)> {
    let began = Instant::now();
    let mut failure = "approved cgroup could not be observed".to_owned();
    while began.elapsed() < Duration::from_secs(15) {
        authority().map_err(|e| (CheckOutcome::Cancelled, e))?;
        match observe(unit, &check.limits) {
            Ok(found) => {
                publish_private(
                    &check.scratch.join(format!("permit-{token}")),
                    token.as_bytes(),
                )
                .map_err(|e| (CheckOutcome::Failed, e))?;
                return Ok(found);
            }
            Err(e) => failure = e,
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err((CheckOutcome::Failed, failure))
}

fn wait(
    prepared: &Prepared,
    config_bytes: &[u8],
    child: &mut bounded_command::AsyncCommand,
    authority: &impl Fn() -> Result<(), String>,
) -> (
    CheckOutcome,
    Option<bounded_command::Output>,
    Option<Completion>,
    Option<String>,
) {
    let check = &prepared.check;
    let began = Instant::now();
    loop {
        if let Err(e) = authority() {
            return (CheckOutcome::Cancelled, None, None, Some(e));
        }
        if began.elapsed() >= Duration::from_secs(u64::from(check.limits.wall_seconds)) {
            return (
                CheckOutcome::TimedOut,
                None,
                None,
                Some("approved check exceeded its wall deadline".into()),
            );
        }
        match completion(prepared, config_bytes) {
            Ok(Some(status)) => return (CheckOutcome::Exited, None, Some(status), None),
            Ok(None) => (),
            Err(e) => return (CheckOutcome::Failed, None, None, Some(e)),
        }
        if let Some(result) = child.try_wait() {
            return match result {
                Ok(output) => (
                    CheckOutcome::Failed,
                    Some(output),
                    None,
                    Some("owned wrapper exited without an observed check completion".into()),
                ),
                Err(e) => {
                    let reason = e.to_string();
                    let cause = match e {
                        bounded_command::Error::OutputTooLarge { .. } => CheckOutcome::OutputLimit,
                        bounded_command::Error::Timeout(_) => CheckOutcome::TimedOut,
                        _ => CheckOutcome::Failed,
                    };
                    (cause, None, None, Some(reason))
                }
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn run(
    check: &ApprovedCheck,
    authority: impl Fn() -> Result<(), String>,
) -> Result<CheckReceipt, String> {
    let started = Instant::now();
    let RunningCheck {
        unit,
        prepared,
        config_bytes: bytes,
        mut child,
    } = start(check, &authority)?;
    // No build script executes until the cgroup was observed and permitted.
    let (observation, mut outcome, mut output, completed, mut error) =
        match admit(check, &prepared.token, &unit.0, &authority) {
            Ok(observed) => {
                let (outcome, output, completed, failure) =
                    wait(&prepared, &bytes, &mut child, &authority);
                (Some(observed), outcome, output, completed, failure)
            }
            Err((cause, failure)) => (None, cause, None, None, Some(failure)),
        };
    let service_result = property(&unit.0, "Result").ok();
    let service_state_before_cleanup = property(&unit.0, "ActiveState").ok();
    if service_result.as_deref() == Some("timeout") {
        outcome = CheckOutcome::TimedOut;
        error = Some("owned service observed its runtime deadline".into());
    }
    let cleanup_observed = unit.cleanup(observation.as_ref());
    if cleanup_observed {
        let _ = control(&["--user", "reset-failed", "--", &unit.0]);
    }
    if output.is_none() && completed.is_some() && cleanup_observed {
        let capture_started = Instant::now();
        while capture_started.elapsed() < Duration::from_secs(3) {
            if let Some(result) = child.try_wait() {
                match result {
                    Ok(captured) => output = Some(captured),
                    Err(e) => {
                        error = Some(e.to_string());
                        outcome = match e {
                            bounded_command::Error::OutputTooLarge { .. } => {
                                CheckOutcome::OutputLimit
                            }
                            bounded_command::Error::Timeout(_) => CheckOutcome::TimedOut,
                            _ => CheckOutcome::Failed,
                        };
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if output.is_none() && error.is_none() {
            outcome = CheckOutcome::Failed;
            error.get_or_insert_with(|| "stopped check streams could not be collected".into());
        }
    }
    drop(child);
    if !cleanup_observed {
        outcome = CheckOutcome::Failed;
        let cleanup_failure = "owned check process-tree cleanup could not be observed";
        error = Some(error.map_or_else(
            || cleanup_failure.to_owned(),
            |previous| format!("{previous}; {cleanup_failure}"),
        ));
    }
    let captured = output.is_some();
    let (wrapper_exit_code, stdout, stderr) = output.map_or((None, Vec::new(), Vec::new()), |o| {
        (o.status.code(), o.stdout, o.stderr)
    });
    if captured {
        private_file(
            &check.scratch.join(format!("stdout-{}", prepared.token)),
            &stdout,
        )?;
        private_file(
            &check.scratch.join(format!("stderr-{}", prepared.token)),
            &stderr,
        )?;
    }
    Ok(CheckReceipt {
        binding: check.binding.clone(),
        check_id: check.id.clone(),
        unit: unit.0.clone(),
        config_sha256: hash(&bytes),
        runtime_binary_sha256: prepared.runtime_binary_sha256,
        supervisor_source_sha256: hash(include_bytes!("execution.rs")),
        limits: check.limits.clone(),
        cgroup: observation,
        outcome,
        exit_code: completed.as_ref().and_then(|c| c.exit_code),
        check_signal: completed.as_ref().and_then(|c| c.signal),
        wrapper_exit_code,
        service_result,
        service_state_before_cleanup,
        stdout_sha256: captured.then(|| hash(&stdout)),
        stderr_sha256: captured.then(|| hash(&stderr)),
        stdout: captured.then(|| String::from_utf8_lossy(&stdout).into_owned()),
        stderr: captured.then(|| String::from_utf8_lossy(&stderr).into_owned()),
        elapsed_ms: started.elapsed().as_millis(),
        failure: error,
        cleanup_observed,
    })
}

/// Explicit developer qualification entry, not a model operation or production
/// host registration route. Private input and a new private receipt are required.
///
/// # Errors
/// Returns unsafe input, invalid registration, supervision or receipt failures.
pub fn developer_probe(
    config: &Path,
    receipt: &Path,
    cancel_after_ms: Option<u64>,
) -> Result<CheckReceipt, String> {
    let check: ApprovedCheck =
        serde_json::from_slice(&read_private(config)?).map_err(|e| e.to_string())?;
    let binding = check.binding.clone();
    let id = check.id.clone();
    let registry = super::ApprovedChecks::new(binding.clone(), vec![check])?;
    let began = Instant::now();
    let result = registry.run(&binding, &id, || {
        if cancel_after_ms.is_some_and(|ms| began.elapsed() >= Duration::from_millis(ms)) {
            Err("developer explicitly cancelled this owned qualification job".into())
        } else {
            Ok(())
        }
    })?;
    let bytes = serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?;
    private_file(receipt, &bytes)?;
    Ok(result)
}

/// Internal trusted host entry: waits for the supervisor's cgroup gate, then
/// enters the existing ush sandbox. No model-facing request accepts this path.
///
/// # Errors
/// Returns unsafe configuration, missing gate, changed helper or sandbox setup.
pub fn run_guest(config: &Path) -> Result<i32, String> {
    let config_bytes = read_private(config)?;
    let prepared: Prepared = serde_json::from_slice(&config_bytes).map_err(|e| e.to_string())?;
    prepared.check.validate()?;
    if binary_hash(&std::env::current_exe().map_err(|e| e.to_string())?)?
        != prepared.runtime_binary_sha256
    {
        return Err("approved check helper binary identity changed".into());
    }
    if uuid::Uuid::parse_str(&prepared.token).is_err() {
        return Err("invalid approved-check gate token".into());
    }
    let check = &prepared.check;
    let permit = check.scratch.join(format!("permit-{}", prepared.token));
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(16))
        .ok_or("check gate deadline overflow")?;
    loop {
        match read_private(&permit) {
            Ok(bytes) if bytes == prepared.token.as_bytes() => break,
            Ok(_) => return Err("approved-check cgroup gate identity mismatched".into()),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("approved check was never admitted: {e}")),
        }
    }
    let session = check.scratch.join(format!("guest-{}", prepared.token));
    let helper = crate::helper_runtime::HelperRuntime::resolve(&check.runtime_binary)
        .map_err(|e| e.to_string())?;
    let mut command = helper.as_ref().map_or_else(
        || vec![format!("{}/tilo-ush", crate::GUEST_BIN_DIR)],
        crate::helper_runtime::HelperRuntime::entry,
    );
    command.push("__approved-check-init".into());
    command.push("--".into());
    command.extend(check.command.clone());
    let plan = crate::guest::GuestPlan {
        images: check.images.iter().map(|i| i.root.clone()).collect(),
        host_root: "/".into(),
        session_dir: session.clone(),
        var_dir: session.join("var"),
        guest_home: session.join("home"),
        user: crate::guest::GuestUser::current(),
        environment: "Approved check".into(),
        masked: vec![],
        trusted_dirs: vec![],
        readable_dirs: vec![],
        grant_root: None,
        runtime_bin: check.runtime_binary.clone(),
        helper_runtime: helper,
        legacy_commands: vec![],
        broker_socket: session.join("absent-broker"),
        seccomp_blob: session.join("ush.bpf"),
        command,
        shell: "/usr/bin/sh".into(),
        term: "dumb".into(),
        env: check.environment.clone(),
        resolver: Some(String::new()),
    };
    plan.prepare().map_err(|e| e.to_string())?;
    crate::seccomp::write_guest_filter(&session).map_err(|e| e.to_string())?;
    let (_blob, fd) =
        crate::guest::open_seccomp_fd(&plan.seccomp_blob).map_err(|e| e.to_string())?;
    let sandbox_arguments =
        crate::guest::build_check_argv(&plan, fd, &check.checkout, &check.build);
    let (program, parameters) = sandbox_arguments
        .split_first()
        .ok_or("approved guest argv missing")?;
    let result = Command::new(program)
        .args(parameters)
        .status()
        .map_err(|e| e.to_string())?;
    let observed = Completion {
        token: prepared.token.clone(),
        config_sha256: hash(&config_bytes),
        runtime_binary_sha256: prepared.runtime_binary_sha256.clone(),
        exit_code: result.code(),
        signal: result.signal(),
    };
    publish_private(
        &check.scratch.join(format!("completion-{}", prepared.token)),
        &serde_json::to_vec(&observed).map_err(|e| e.to_string())?,
    )?;
    // Keep the exact owned unit observable until the host explicitly stops it.
    // RuntimeMaxSec still bounds this helper if the host disappears.
    loop {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Internal guest entry. All subsequent commands inherit fully enforced
/// Landlock; image startup files and guest management brokers never run.
///
/// # Errors
/// Returns missing fixed argv, incomplete confinement or program exec failure.
pub fn run_init(args: &[String]) -> Result<i32, String> {
    let Some((program, rest)) = args
        .strip_prefix(&["--".into()])
        .and_then(|a| a.split_first())
    else {
        return Err("approved check init requires its fixed program".into());
    };
    let rw = ["/tmp", "/var", "/run", "/dev", "/proc", "/project/target"].map(PathBuf::from);
    let outcome = crate::guard::restrict_self(&rw, &[PathBuf::from("/")]);
    if outcome != crate::guard::LandlockOutcome::FullyEnforced {
        return Err(format!(
            "approved check refused incomplete Landlock: {}",
            crate::guard::describe(outcome)
        ));
    }
    Err(Command::new(program).args(rest).exec().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> CgroupObservation {
        CgroupObservation {
            main_pid: 12,
            control_group: "/owned.service".into(),
            cpu_max: "100000 100000".into(),
            memory_max: "67108864".into(),
            memory_swap_max: "0".into(),
            pids_max: "32".into(),
        }
    }
    fn limits() -> CheckLimits {
        CheckLimits {
            cpu_percent: 100,
            memory_bytes: 67_108_864,
            processes: 32,
            wall_seconds: 5,
            output_bytes: 4096,
        }
    }

    #[test]
    fn completion_requires_matching_private_token_config_binary_and_real_status() {
        let dir = tempfile::tempdir().unwrap();
        let check = ApprovedCheck {
            id: "fixture".into(),
            binding: ProjectBinding {
                workspace: uuid::Uuid::new_v4(),
                host: [1; 32],
                record: "file:/fixture/project.tilo".into(),
                resource: "project:fixture".into(),
            },
            checkout: dir.path().join("source"),
            build: dir.path().join("build"),
            scratch: dir.path().into(),
            runtime_binary: "/usr/bin/true".into(),
            images: vec![],
            command: vec!["/usr/bin/true".into()],
            environment: vec![],
            limits: limits(),
        };
        let mut prepared = Prepared {
            check,
            token: uuid::Uuid::new_v4().to_string(),
            runtime_binary_sha256: "a".repeat(64),
        };
        assert!(completion(&prepared, b"config").unwrap().is_none());
        for (token, config, binary, code, signal) in [
            (
                "foreign".into(),
                hash(b"config"),
                "a".repeat(64),
                Some(0),
                None,
            ),
            (
                prepared.token.clone(),
                hash(b"other"),
                "a".repeat(64),
                Some(0),
                None,
            ),
            (
                prepared.token.clone(),
                hash(b"config"),
                "b".repeat(64),
                Some(0),
                None,
            ),
            (
                prepared.token.clone(),
                hash(b"config"),
                "a".repeat(64),
                None,
                None,
            ),
            (
                prepared.token.clone(),
                hash(b"config"),
                "a".repeat(64),
                Some(0),
                Some(9),
            ),
        ] {
            let value = Completion {
                token,
                config_sha256: config,
                runtime_binary_sha256: binary,
                exit_code: code,
                signal,
            };
            let path = prepared
                .check
                .scratch
                .join(format!("completion-{}", prepared.token));
            publish_private(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(completion(&prepared, b"config").is_err());
            prepared.token = uuid::Uuid::new_v4().to_string();
        }
        let value = Completion {
            token: prepared.token.clone(),
            config_sha256: hash(b"config"),
            runtime_binary_sha256: "a".repeat(64),
            exit_code: Some(23),
            signal: None,
        };
        let path = prepared
            .check
            .scratch
            .join(format!("completion-{}", prepared.token));
        publish_private(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            completion(&prepared, b"config").unwrap().unwrap().exit_code,
            Some(23)
        );
        assert!(publish_private(&path, b"replacement").is_err());
        assert_eq!(
            completion(&prepared, b"config").unwrap().unwrap().exit_code,
            Some(23)
        );
    }

    #[test]
    fn cleanup_requires_observed_quiescent_manager_with_no_pid_or_pending_job() {
        let good = "LoadState=loaded\nActiveState=inactive\nMainPID=0\nJob=0\n";
        assert!(quiescent(&unit_state(good).unwrap()));
        assert!(quiescent(
            &unit_state("LoadState=not-found\nActiveState=inactive\nMainPID=0\nJob=\n").unwrap()
        ));
        for text in [
            "LoadState=loaded\nActiveState=activating\nMainPID=0\nJob=0\n",
            "LoadState=loaded\nActiveState=inactive\nMainPID=0\nJob=41\n",
            "LoadState=loaded\nActiveState=active\nMainPID=0\nJob=0\n",
            "LoadState=loaded\nActiveState=failed\nMainPID=12\nJob=0\n",
            "LoadState=loaded\nActiveState=inactive\nJob=0\n",
            "LoadState=error\nActiveState=inactive\nMainPID=0\nJob=0\n",
        ] {
            assert!(!quiescent(&unit_state(text).unwrap()));
        }
        assert!(quiescent(
            &unit_state("LoadState=not-found\nActiveState=inactive\nJob=0\n").unwrap()
        ));
        assert!(unit_state("LoadState=loaded\nActiveState=inactive\nMainPID=0\n").is_err());
        assert!(
            unit_state("LoadState=loaded\nActiveState=inactive\nMainPID=0\nJob=0\nJob=41\n")
                .is_err()
        );
    }

    #[test]
    fn observed_kernel_values_must_actually_bound_every_aggregate_dimension() {
        assert!(validate_observation(&observation(), &limits()).is_ok());
        for changed in [
            CgroupObservation {
                cpu_max: "max 100000".into(),
                ..observation()
            },
            CgroupObservation {
                cpu_max: "100001 100000".into(),
                ..observation()
            },
            CgroupObservation {
                cpu_max: "1 0".into(),
                ..observation()
            },
            CgroupObservation {
                memory_max: "max".into(),
                ..observation()
            },
            CgroupObservation {
                memory_max: "67108865".into(),
                ..observation()
            },
            CgroupObservation {
                memory_swap_max: "1".into(),
                ..observation()
            },
            CgroupObservation {
                pids_max: "33".into(),
                ..observation()
            },
            CgroupObservation {
                pids_max: "max".into(),
                ..observation()
            },
        ] {
            assert!(validate_observation(&changed, &limits()).is_err());
        }
    }

    #[test]
    fn private_gate_refuses_symlink_hardlink_public_input_and_unbounded_files() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        private_file(&input, b"gate").unwrap();
        assert_eq!(read_private(&input).unwrap(), b"gate");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&input, &link).unwrap();
        assert!(read_private(&link).is_err());
        std::fs::hard_link(&input, dir.path().join("hard")).unwrap();
        assert!(read_private(&input).is_err());
        let large = dir.path().join("large");
        private_file(&large, &vec![0; 65537]).unwrap();
        assert!(read_private(&large).is_err());
        let public = dir.path().join("public");
        std::fs::write(&public, b"unsafe").unwrap();
        assert!(read_private(&public).is_err());
    }
}
