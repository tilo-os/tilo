//! Linux local runtime lifecycle; exact container custody is recorded before execution.
//! GUI/model callers select signed releases through the service, never raw commands.
use crate::ownership::{ContainerOwnership, arguments_digest, verify_stopped_unit};
use crate::{ProcessHandle, ProcessSupervisor, ServiceError, VllmCommand};
use agent_provider_config::local::{LocalEndpoint, RuntimeKind};
use getrandom::{SysRng, rand_core::TryRng as _};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// User-systemd supervision. The executable and every vLLM argument come from
/// [`VllmCommand`], itself constructed only from a verified signed catalog.
pub struct SystemdSupervisor;
/// Fixed existing placement shared by capacity admission and runtime launch.
pub const MODEL_RUNTIME_SLICE: &str = "app.slice";
#[must_use]
pub fn podman_vllm_argv(command: &VllmCommand) -> Vec<String> {
    let mut argv = vec![
        "create".into(),
        "--pull".into(),
        "never".into(),
        "--network".into(),
        "host".into(),
    ];
    match command.runtime_kind {
        agent_provider_config::local::RuntimeKind::Cpu => {}
        agent_provider_config::local::RuntimeKind::NvidiaCuda => {
            argv.extend(["--device".into(), "nvidia.com/gpu=all".into()]);
        }
        agent_provider_config::local::RuntimeKind::AmdRocm => {
            argv.extend([
                "--device".into(),
                "/dev/kfd".into(),
                "--device".into(),
                "/dev/dri".into(),
            ]);
        }
        // Mesa in the image drives the GPU through its render node.
        agent_provider_config::local::RuntimeKind::Vulkan => {
            argv.extend(["--device".into(), "/dev/dri".into()]);
        }
    }
    // The host object is derived exclusively from its verified SHA-256 path.
    // `label=disable` lets rootless podman read the existing per-user store
    // on SELinux systems without recursively relabeling shared model bytes.
    argv.extend([
        "--mount".into(),
        format!(
            "type=bind,src={},dst={},ro",
            command.model_path.display(),
            command.model_container_path
        ),
        "--security-opt".into(),
        "label=disable".into(),
        command.runtime_image.clone(),
    ]);
    argv.extend(command.args.iter().cloned());
    argv
}
/// A listening socket may only serve "Loading model". Readiness requires a
/// healthy HTTP endpoint and discovery of the exact configured model alias.
#[must_use]
pub fn model_endpoint_ready(client: &reqwest::blocking::Client, endpoint: &LocalEndpoint) -> bool {
    let Some(base) = endpoint.url.strip_suffix("/v1") else {
        return false;
    };
    let Some(port) = base.strip_prefix("http://127.0.0.1:") else {
        return false;
    };
    if !port.parse::<u16>().is_ok_and(|port| port > 0) {
        return false;
    }
    let Ok(health) = client.get(format!("{base}/health")).send() else {
        return false;
    };
    if health.status() != reqwest::StatusCode::OK {
        return false;
    }
    let Ok(models) = client.get(format!("{}/models", endpoint.url)).send() else {
        return false;
    };
    if models.status() != reqwest::StatusCode::OK {
        return false;
    }
    let mut bytes = Vec::new();
    if models.take(65_537).read_to_end(&mut bytes).is_err() || bytes.len() > 65_536 {
        return false;
    }
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    body.get("data")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|models| {
            models.iter().any(|model| {
                model.get("id").and_then(serde_json::Value::as_str)
                    == Some(endpoint.model_id.as_str())
            })
        })
}

impl ProcessSupervisor for SystemdSupervisor {
    fn install_runtime_image(
        &mut self,
        archive: &std::path::Path,
        signed_image: &str,
    ) -> Result<(), ServiceError> {
        // `podman load` receives a verified, content-addressed archive path;
        // it is never given a registry tag or caller-controlled image string.
        let mut load = Command::new("podman");
        load.args(["load", "--input"]).arg(archive);
        let output =
            bounded_command::run_prepared(load, None, 1024 * 1024, Duration::from_secs(1800))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "podman could not import verified runtime archive".into(),
            ));
        }
        let mut inspect = Command::new("podman");
        inspect.args([
            "image",
            "inspect",
            "--format",
            "{{json .RepoDigests}}",
            signed_image,
        ]);
        let output =
            bounded_command::run_prepared(inspect, None, 1024 * 1024, Duration::from_secs(30))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "podman cannot inspect imported signed runtime image".into(),
            ));
        }
        let digests: Vec<String> = serde_json::from_slice(&output.stdout).map_err(|_| {
            ServiceError::Backend("podman returned invalid image digest metadata".into())
        })?;
        if !digests.iter().any(|digest| digest == signed_image) {
            return Err(ServiceError::Backend(
                "imported OCI archive did not provide the exact signed image digest".into(),
            ));
        }
        Ok(())
    }
    fn prepare(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
        prepare_container(command, false)
    }

    fn start(&mut self, command: &VllmCommand, handle: &ProcessHandle) -> Result<(), ServiceError> {
        let owned = owned_container(handle)?;
        if owned.image != command.runtime_image
            || owned.model_id_sha256
                != hex::encode(Sha256::digest(command.endpoint.model_id.as_bytes()))
            || owned.arguments_sha256 != arguments_digest(&command.args)
        {
            return Err(ServiceError::Backend(
                "prepared runtime no longer matches signed launch".into(),
            ));
        }
        owned
            .verify_observation(&container_snapshot(&owned.container_id)?)
            .map_err(ServiceError::Backend)?;
        let readiness = tilo_net::http::blocking_client()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(250))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        launch_container(handle, false)?;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10 * 60) {
            if model_endpoint_ready(&readiness, &command.endpoint) {
                let observed = container_snapshot(&owned.container_id)?;
                owned
                    .verify_observation(&observed)
                    .map_err(ServiceError::Backend)?;
                if observed
                    .pointer("/State/Running")
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
                {
                    return Err(ServiceError::Backend(
                        "healthy endpoint has no running owned container".into(),
                    ));
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Err(ServiceError::Backend(
            "verified local model did not become healthy with exact custody".into(),
        ))
    }

    fn observe(
        &self,
        handle: &ProcessHandle,
    ) -> crate::run_observation::Observed<crate::run_observation::RuntimeSample> {
        match observe_owned_runtime(handle) {
            Ok(value) => crate::run_observation::Observed::Actual { value },
            Err(reason) => crate::run_observation::Observed::Unavailable { reason },
        }
    }

    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
        let owned = owned_container(handle)?;
        let present = container_exists(&owned.container_id)?;
        if present {
            owned
                .verify_observation(&container_snapshot(&owned.container_id)?)
                .map_err(ServiceError::Backend)?;
        }
        // Removing the exact container first lets attach/conmon finish normally.
        // Even failed removal must be followed by the exact manager stop attempt.
        let removal = if present {
            bounded_process(
                "podman",
                &["rm".into(), "--force".into(), owned.container_id.clone()],
            )
            .and_then(|output| {
                if output.status.success() {
                    Ok(())
                } else {
                    Err(ServiceError::Backend(
                        "podman could not remove the exact owned container".into(),
                    ))
                }
            })
        } else {
            Ok(())
        };
        let manager = stop_owned_unit(&handle.id);
        let absence = container_exists(&owned.container_id).and_then(|present| {
            if present {
                Err(ServiceError::Backend(
                    "owned runtime container remains after cleanup".into(),
                ))
            } else {
                Ok(())
            }
        });
        removal?;
        manager?;
        absence
    }
}

fn observed_unit(
    handle: &ProcessHandle,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut command = Command::new("systemctl");
    command.args([
        "--user",
        "show",
        &handle.id,
        "--property=Id,LoadState,ActiveState,MainPID,ControlGroup,InvocationID",
    ]);
    let output = bounded_command::run_prepared(command, None, 8192, Duration::from_secs(3))
        .map_err(|_| "owned unit observation unavailable")?;
    if !output.status.success() {
        return Err("owned unit observation refused".into());
    }
    let body = String::from_utf8(output.stdout).map_err(|_| "invalid owned unit observation")?;
    parse_unit(&body, handle)
}

fn parse_unit(
    body: &str,
    handle: &ProcessHandle,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut properties = std::collections::BTreeMap::new();
    for line in body.lines() {
        let (key, value) = line.split_once('=').ok_or("invalid manager property")?;
        if properties.insert(key.into(), value.into()).is_some() {
            return Err("duplicate manager property".into());
        }
    }
    if properties
        .get("Id")
        .and_then(|id: &String| id.strip_suffix(".service"))
        != Some(handle.id.as_str())
        || properties.get("LoadState").map(String::as_str) != Some("loaded")
        || properties.get("ActiveState").map(String::as_str) != Some("active")
    {
        return Err("owned unit is not currently active".into());
    }
    if !properties
        .get("MainPID")
        .is_some_and(|pid| pid.parse::<u32>().is_ok_and(|pid| pid > 0))
        || !properties
            .get("InvocationID")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        || !properties.get("ControlGroup").is_some_and(|group| {
            group.starts_with('/')
                && group != "/"
                && !Path::new(group)
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        })
    {
        return Err("owned unit process or lifetime metadata unavailable".into());
    }
    Ok(properties)
}

fn observed_payload(handle: &ProcessHandle) -> Result<u32, String> {
    let owned = owned_container(handle).map_err(|_| "runtime custody unavailable")?;
    let mut command = Command::new("podman");
    command.args(["container", "inspect", &owned.container_id]);
    let output = bounded_command::run_prepared(command, None, 131_072, Duration::from_secs(3))
        .map_err(|_| "owned payload observation unavailable")?;
    if !output.status.success() {
        return Err("owned payload inspection refused".into());
    }
    let bodies: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).map_err(|_| "invalid payload observation")?;
    let [body] = bodies.as_slice() else {
        return Err("payload inspection is not exact".into());
    };
    payload_pid(owned, body)
}

fn payload_pid(owned: &ContainerOwnership, body: &serde_json::Value) -> Result<u32, String> {
    owned.verify_observation(body)?;
    if body
        .pointer("/State/Running")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err("owned payload is not running".into());
    }
    body.pointer("/State/Pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| "owned payload PID unavailable".into())
}

fn observe_owned_runtime(
    handle: &ProcessHandle,
) -> Result<crate::run_observation::RuntimeSample, String> {
    use crate::run_observation::{RuntimeSample, sample_process, verify_process};
    owned_container(handle).map_err(|_| "runtime custody unavailable")?;
    let started = Instant::now();
    let captured_at_unix_ms =
        tilo_clock::try_unix_now_ms().ok_or("observation wall clock unavailable")?;
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "boot identity unavailable")?;
    if boot_id.trim().len() != 36 {
        return Err("boot identity malformed".into());
    }
    let unit = observed_unit(handle)?;
    let wrapper_pid = unit
        .get("MainPID")
        .ok_or("wrapper PID missing")?
        .parse()
        .map_err(|_| "wrapper PID malformed")?;
    let invocation = unit.get("InvocationID").ok_or("unit invocation missing")?;
    if invocation.len() != 32 || !invocation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("unit invocation malformed".into());
    }
    let control_group = unit.get("ControlGroup").ok_or("unit cgroup missing")?;
    let payload_pid = observed_payload(handle)?;
    let wrapper = sample_process(wrapper_pid)?;
    let payload = sample_process(payload_pid)?;
    if !Path::new(&wrapper.identity.membership).starts_with(control_group) || control_group == "/" {
        return Err("wrapper does not belong to the owned unit cgroup".into());
    }
    if observed_unit(handle)? != unit || observed_payload(handle)? != payload_pid {
        return Err("owned unit or payload lifetime changed".into());
    }
    verify_process(&wrapper.identity)?;
    verify_process(&payload.identity)?;
    Ok(RuntimeSample {
        boot_id: boot_id.trim().into(),
        captured_at_unix_ms,
        unit_invocation_id: invocation.clone(),
        unit_control_group: control_group.clone(),
        elapsed_micros: u64::try_from(started.elapsed().as_micros())
            .map_err(|_| "observation duration overflow")?,
        wrapper,
        payload,
    })
}

fn prepare_container(command: &VllmCommand, fixture: bool) -> Result<ProcessHandle, ServiceError> {
    let mut random = [0_u8; 32];
    SysRng
        .try_fill_bytes(&mut random)
        .map_err(|error| ServiceError::Backend(error.to_string()))?;
    let run_id = hex::encode(random);
    let model_id_sha256 = hex::encode(Sha256::digest(command.endpoint.model_id.as_bytes()));
    let mut arguments = podman_vllm_argv(command);
    if fixture {
        arguments.splice(1..1, ["--entrypoint".into(), "/bin/sleep".into()]);
    }
    arguments.splice(
        1..1,
        [
            "--name".into(),
            format!("tilo-local-model-{run_id}"),
            "--label".into(),
            format!("io.tilo.local-model.run={run_id}"),
            "--label".into(),
            format!("io.tilo.local-model.model={model_id_sha256}"),
            "--label".into(),
            format!("io.tilo.local-model.image={}", command.runtime_image),
        ],
    );
    let output = bounded_process("podman", &arguments)?;
    if !output.status.success() {
        return Err(ServiceError::Backend(
            "podman could not prepare verified local runtime".into(),
        ));
    }
    let owned = ContainerOwnership {
        container_id: String::from_utf8(output.stdout)
            .map_err(|_| ServiceError::Backend("invalid prepared CID".into()))?
            .trim()
            .into(),
        run_id,
        model_id_sha256,
        image: command.runtime_image.clone(),
        arguments_sha256: arguments_digest(&command.args),
    };
    owned.validate().map_err(ServiceError::Backend)?;
    // Record the exact created CID before any further observation or execution;
    // start revalidates image, labels and arguments before launching code.
    Ok(ProcessHandle {
        id: owned.unit(),
        container: Some(owned),
    })
}

fn launch_container(handle: &ProcessHandle, fixture: bool) -> Result<(), ServiceError> {
    let owned = owned_container(handle)?;
    let mut arguments = vec![
        "--user".into(),
        "--collect".into(),
        "--service-type=exec".into(),
        format!("--slice={MODEL_RUNTIME_SLICE}"),
        "--unit".into(),
        handle.id.clone(),
        "--property=KillMode=control-group".into(),
        "--property=TimeoutStopSec=30s".into(),
    ];
    if fixture {
        arguments.push("--property=RuntimeMaxSec=150s".into());
    }
    arguments.extend([
        "--".into(),
        "podman".into(),
        "start".into(),
        "--attach".into(),
    ]);
    if fixture {
        arguments.push("--sig-proxy=false".into());
    }
    arguments.push(owned.container_id.clone());
    let launch = bounded_process("systemd-run", &arguments)?;
    if !launch.status.success() {
        return Err(ServiceError::Backend(
            "systemd-run rejected the prepared local runtime".into(),
        ));
    }
    Ok(())
}

fn bounded_process(
    program: &str,
    arguments: &[String],
) -> Result<bounded_command::Output, ServiceError> {
    let mut command = Command::new(program);
    command.args(arguments);
    bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(30))
        .map_err(|error| ServiceError::Backend(error.to_string()))
}

fn owned_container(handle: &ProcessHandle) -> Result<&ContainerOwnership, ServiceError> {
    let owned = handle.container.as_ref().ok_or_else(|| {
        ServiceError::Backend(
            "legacy runtime has no exact container custody; cleanup cannot be certified".into(),
        )
    })?;
    owned.validate().map_err(ServiceError::Backend)?;
    if handle.id != owned.unit() {
        return Err(ServiceError::Backend(
            "runtime unit does not match owned run".into(),
        ));
    }
    Ok(owned)
}

fn container_snapshot(id: &str) -> Result<serde_json::Value, ServiceError> {
    let output = bounded_process("podman", &["container".into(), "inspect".into(), id.into()])?;
    if !output.status.success() {
        return Err(ServiceError::Backend(
            "owned container inspection unavailable".into(),
        ));
    }
    let mut observations: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    if observations.len() != 1 {
        return Err(ServiceError::Backend(
            "owned container inspection is not exact".into(),
        ));
    }
    observations
        .pop()
        .ok_or_else(|| ServiceError::Backend("container observation missing".into()))
}

fn container_exists(id: &str) -> Result<bool, ServiceError> {
    let observed = bounded_process("podman", &["container".into(), "exists".into(), id.into()])?;
    match observed.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(ServiceError::Backend(
            "owned container absence is not observable".into(),
        )),
    }
}

fn stop_owned_unit(unit: &str) -> Result<(), ServiceError> {
    // The service's TimeoutStopSec is 30s. The client must outlive that
    // manager deadline to observe completion rather than race deactivation.
    let mut command = Command::new("systemctl");
    command.args(["--user", "stop", unit]);
    let stopped =
        bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(45));
    let observed = bounded_process(
        "systemctl",
        &[
            "--user".into(),
            "show".into(),
            unit.into(),
            "--property=LoadState,ActiveState,MainPID,Job".into(),
        ],
    )?;
    let body = String::from_utf8(observed.stdout)
        .map_err(|_| ServiceError::Backend("invalid manager observation".into()))?;
    verify_stopped_unit(&body).map_err(ServiceError::Backend)?;
    let not_found = body.lines().any(|line| line == "LoadState=not-found");
    if !(observed.status.success() || observed.status.code() == Some(1) && not_found)
        || !stopped.is_ok_and(|output| output.status.success()) && !not_found
    {
        return Err(ServiceError::Backend(
            "exact local runtime unit stop was not observed".into(),
        ));
    }
    Ok(())
}

/// Content-free observations from fixed host-owned process fixtures.
#[derive(Debug, serde::Serialize)]
pub struct DeveloperCleanupReceipt {
    pub kind: &'static str,
    pub target: ProcessHandle,
    pub control: ProcessHandle,
    pub observations: std::collections::BTreeMap<&'static str, bool>,
    pub source_sha256: String,
    pub runtime_observations:
        std::collections::BTreeMap<String, crate::run_observation::RuntimeSample>,
}

struct FixtureCustody(Vec<ProcessHandle>);
impl Drop for FixtureCustody {
    fn drop(&mut self) {
        for handle in &self.0 {
            let _ = SystemdSupervisor.stop(handle);
        }
    }
}

fn fixture_running(handle: &ProcessHandle) -> Result<bool, ServiceError> {
    let owned = owned_container(handle)?;
    let body = container_snapshot(&owned.container_id)?;
    owned
        .verify_observation(&body)
        .map_err(ServiceError::Backend)?;
    body.pointer("/State/Running")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| ServiceError::Backend("fixture running state is unobservable".into()))
}

fn wait_fixture_running(handle: &ProcessHandle) -> Result<(), ServiceError> {
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(10) {
        if fixture_running(handle)? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(ServiceError::Backend(
        "fixed fixture did not start within its bound".into(),
    ))
}

fn verify_fixture_image(image: &str) -> Result<(), ServiceError> {
    ContainerOwnership {
        container_id: "0".repeat(64),
        run_id: "0".repeat(64),
        image: image.into(),
        model_id_sha256: "0".repeat(64),
        arguments_sha256: "0".repeat(64),
    }
    .validate()
    .map_err(ServiceError::Backend)?;
    let output = bounded_process(
        "podman",
        &[
            "image".into(),
            "inspect".into(),
            "--format".into(),
            "{{json .RepoDigests}}".into(),
            image.into(),
        ],
    )?;
    if !output.status.success() {
        return Err(ServiceError::Backend("fixture image is unavailable".into()));
    }
    let digests: Vec<String> = serde_json::from_slice(&output.stdout)?;
    if !digests.iter().any(|digest| digest == image) {
        return Err(ServiceError::Backend(
            "fixture image has no matching immutable digest".into(),
        ));
    }
    Ok(())
}

fn publish_fixture_custody(
    directory: &Path,
    name: &str,
    handle: &ProcessHandle,
) -> Result<(), ServiceError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(name))?;
    file.write_all(&serde_json::to_vec_pretty(handle)?)?;
    file.sync_all()?;
    Ok(())
}

fn fixture_observations(
    directory: &Path,
    target: &ProcessHandle,
    control: &ProcessHandle,
) -> Result<std::collections::BTreeMap<String, crate::run_observation::RuntimeSample>, ServiceError>
{
    use crate::run_observation::{Observed, RunCheckpoint};
    let mut output = std::collections::BTreeMap::new();
    for handle in [target, control] {
        let sample = observe_owned_runtime(handle).map_err(ServiceError::Backend)?;
        if !matches!(
            sample.payload.counters.get("memory.current"),
            Some(Observed::Actual { .. })
        ) {
            return Err(ServiceError::Backend(
                "fixture payload memory counter unavailable".into(),
            ));
        }
        let checkpoint = RunCheckpoint {
            schema: 1,
            phase: "ready".into(),
            model_id: "developer-lifecycle-fixture".into(),
            model_sha256: hex::encode(Sha256::digest(
                b"developer process-lifecycle fixture; no model weights\n",
            )),
            runtime_id: None,
            process: handle.clone(),
            sample: Observed::Actual {
                value: sample.clone(),
            },
        };
        crate::run_observation::retain(directory, &checkpoint).map_err(ServiceError::Backend)?;
        output.insert(handle.id.clone(), sample);
    }
    Ok(output)
}

/// Qualify exact cleanup with two newly owned, fixed 120-second sleep fixtures.
/// This executes no model and does not admit a catalog or evaluate coding quality.
/// The fixture disables attach signal proxy to exercise a lingering container;
/// production model starts retain podman's usual signal forwarding.
///
/// # Errors
/// Refuses unavailable managers/images, mismatched custody, missing observations
/// or failed cleanup. Best-effort cleanup retains exact custody on every exit.
pub fn developer_cleanup_probe(
    image: &str,
    directory: &Path,
) -> Result<DeveloperCleanupReceipt, ServiceError> {
    use std::io::Write;
    verify_fixture_image(image)?;
    let model_path = directory.join("fixed-fixture.txt");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&model_path)?;
    file.write_all(b"developer process-lifecycle fixture; no model weights\n")?;
    file.sync_all()?;
    let command = VllmCommand {
        runtime_image: image.into(),
        runtime_kind: RuntimeKind::Cpu,
        args: vec!["120".into()],
        model_path,
        model_container_path: "/models/model.gguf".into(),
        endpoint: LocalEndpoint {
            url: "http://127.0.0.1:1/v1".into(),
            model_id: "developer-lifecycle-fixture".into(),
        },
        runtime_extension_id: "developer-fixture-no-extension-admission".into(),
    };
    let mut custody = FixtureCustody(Vec::new());
    let target = prepare_container(&command, true)?;
    custody.0.push(target.clone());
    publish_fixture_custody(directory, "prepared-target.json", &target)?;
    let control = prepare_container(&command, true)?;
    custody.0.push(control.clone());
    publish_fixture_custody(directory, "prepared-control.json", &control)?;
    for handle in [&target, &control] {
        owned_container(handle)?
            .verify_observation(&container_snapshot(&owned_container(handle)?.container_id)?)
            .map_err(ServiceError::Backend)?;
        launch_container(handle, true)?;
        wait_fixture_running(handle)?;
    }
    let target_running_before_stop = fixture_running(&target)?;
    if !target_running_before_stop {
        return Err(ServiceError::Backend(
            "target fixture ended before observed stop".into(),
        ));
    }
    let runtime_observations = fixture_observations(directory, &target, &control)?;
    stop_owned_unit(&target.id)?;
    let target_exists_after_manager_stop =
        container_exists(&owned_container(&target)?.container_id)?;
    if !target_exists_after_manager_stop {
        return Err(ServiceError::Backend(
            "manager-only stop removed the target container object before the cleanup gate".into(),
        ));
    }
    // Process liveness and container-object existence are different observations.
    // A stopped retained container must also be removed by production cleanup.
    let target_running_after_manager_stop = fixture_running(&target)?;
    SystemdSupervisor.stop(&target)?;
    let target_absent_after_cleanup = !container_exists(&owned_container(&target)?.container_id)?;
    let control_running_after_target_cleanup = fixture_running(&control)?;
    if !target_absent_after_cleanup || !control_running_after_target_cleanup {
        return Err(ServiceError::Backend(
            "exact cleanup did not preserve the separate owned control fixture".into(),
        ));
    }
    SystemdSupervisor.stop(&control)?;
    let control_absent_after_final_cleanup =
        !container_exists(&owned_container(&control)?.container_id)?;
    if !control_absent_after_final_cleanup {
        return Err(ServiceError::Backend(
            "control fixture cleanup is incomplete".into(),
        ));
    }
    custody.0.clear();
    Ok(DeveloperCleanupReceipt {
        kind: "developer_fixed_process_cleanup_qualification",
        runtime_observations,
        target,
        control,
        observations: std::collections::BTreeMap::from([
            ("target_running_before_stop", target_running_before_stop),
            (
                "target_exists_after_manager_stop",
                target_exists_after_manager_stop,
            ),
            (
                "target_running_after_manager_stop",
                target_running_after_manager_stop,
            ),
            (
                "control_running_after_target_cleanup",
                control_running_after_target_cleanup,
            ),
            ("target_absent_after_cleanup", target_absent_after_cleanup),
            (
                "control_absent_after_final_cleanup",
                control_absent_after_final_cleanup,
            ),
        ]),
        source_sha256: hex::encode(Sha256::digest(include_bytes!("supervision.rs"))),
    })
}

#[cfg(test)]
#[path = "supervision_observation_tests.rs"]
mod observation_tests;
