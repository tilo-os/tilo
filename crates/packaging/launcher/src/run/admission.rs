//! Read-only admission through the same package, SDK and service gates as launch.

use super::{
    LaunchMode, RunError, RunOptions, identity_store, launch_entry_bin, resolve_capabilities,
    resolve_identity_host_path, resolve_runtime, validate_interfaces, validate_launch_mode,
    verified_original_manifest,
};
use crate::manifest::MinimalManifest;
use crate::services::{
    ServiceLaunchContext, resolve_blob_bind, resolve_chooser_bind, resolve_identity_bind,
    resolve_service_binds_for_launch,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

/// Facts observed by the actual authenticated launcher, never a promise that
/// an application has started or a replacement for image/owner admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAdmission {
    pub package_id: String,
    pub package_root: String,
    pub workspace_id: String,
    pub launcher_blake3: String,
    pub runtime: runtime_bundle::BundleManifest,
    pub connected_services: Vec<String>,
}

/// Admit an exact signed native installation without spawning its payload,
/// creating its data directories or preparing a D-Bus/network proxy.
///
/// The host must register this launcher child through its ordinary launch
/// gate before calling it: services authenticate the same kernel custody as
/// for a real launch. Connections are dropped on return.
///
/// # Errors
/// Refuses incomplete observation, launch bypasses, changed package/SDK bytes,
/// unsupported entrypoints and every normal required service refusal.
pub fn probe_native_admission(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
) -> Result<NativeAdmission, RunError> {
    let refuse = |message: &str| RunError::InvalidServiceLaunch(message.into());
    validate_probe_options(opts)?;
    let expected = opts
        .expected_package_hash
        .as_deref()
        .ok_or_else(|| refuse("native admission requires an exact signed package root"))?;
    let workspace = opts
        .workspace_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| refuse("native admission requires an authenticated workspace"))?;
    let manifest = verified_original_manifest(manifest_path, payload_root, opts, expected)?;
    validate_launch_mode(&manifest, opts)?;
    validate_interfaces(&manifest)?;
    crate::manifest::enforce_fs_policy(&manifest)?;
    // Open the kernel executable link directly: a userspace path can select
    // a replacement sysext while this process still executes its old inode.
    let executable = Path::new("/proc/self/exe");
    let launcher_before = hash_binary(executable)?;
    let (runtime_root, version) = resolve_runtime(&manifest, opts)?
        .ok_or_else(|| refuse("native admission requires a signed SDK"))?;
    // resolve_runtime already applied the ordinary publisher/scoped-retention
    // policy. Read all bytes here so evidence cannot describe only metadata.
    let runtime = runtime_bundle::verify_bundle_with(
        &runtime_root,
        &runtime_bundle::VerifyOptions {
            require_signature: true,
            require_publisher: opts.require_runtime_publisher.clone(),
            require_arch: Some(std::env::consts::ARCH.into()),
        },
    )
    .map_err(|error| RunError::RuntimeBundleSignature {
        path: runtime_root.clone(),
        reason: error.to_string(),
    })?;
    if runtime.version != version {
        return Err(refuse(
            "native admission SDK version differs from the selected application",
        ));
    }
    check_sdk_closure(&manifest, payload_root, opts, (&runtime_root, &version))?;
    let services = authenticate_probe_services(&manifest, opts, workspace)?;
    // Bind the receipt to an unchanged installation, SDK and executed host.
    verified_original_manifest(manifest_path, payload_root, opts, expected)?;
    let runtime_after = runtime_bundle::verify_bundle_with(
        &runtime_root,
        &runtime_bundle::VerifyOptions {
            require_signature: true,
            require_publisher: runtime.publisher_hex.clone(),
            require_arch: Some(std::env::consts::ARCH.into()),
        },
    )
    .map_err(|error| refuse(&error.to_string()))?;
    if runtime != runtime_after || launcher_before != hash_binary(executable)? {
        return Err(refuse("native admission inputs changed during observation"));
    }
    // Recheck the scoped authority too, not only the signed SDK content.
    if resolve_runtime(&manifest, opts)?.as_ref() != Some(&(runtime_root, version)) {
        return Err(refuse(
            "native admission selected SDK changed during observation",
        ));
    }
    let mut connected_services: Vec<_> = services
        .iter()
        .map(|service| service.name.clone())
        .collect();
    connected_services.sort();
    connected_services.dedup();
    Ok(NativeAdmission {
        package_id: manifest.package_id,
        package_root: expected.into(),
        workspace_id: workspace.into(),
        launcher_blake3: launcher_before,
        runtime,
        connected_services,
    })
}

fn authenticate_probe_services(
    manifest: &MinimalManifest,
    opts: &RunOptions,
    workspace: &str,
) -> Result<Vec<crate::services::ResolvedService>, RunError> {
    let capabilities = resolve_capabilities(manifest, opts);
    if capabilities.participation.carries_identity()
        && let Some(path) = resolve_identity_host_path(opts)
    {
        super::load_launch_identity(&path, identity_store(&opts.identity).as_deref())?;
    }
    let egress = crate::sandbox::egress_decision(manifest, &capabilities);
    let mut services = resolve_service_binds_for_launch(
        &manifest.sandbox.services,
        false,
        ServiceLaunchContext {
            app_id: &manifest.package_id,
            workspace_id: Some(workspace),
        },
    )?;
    crate::services::authenticate_shell_ui_bind(&services, &manifest.package_id, &egress)?;
    for service in [
        resolve_identity_bind(&manifest.package_id, Some(workspace), false)?,
        resolve_blob_bind(&manifest.package_id, Some(workspace), false)?,
        resolve_chooser_bind(&manifest.package_id, Some(workspace), false)?,
    ]
    .into_iter()
    .flatten()
    {
        services.push(service);
    }
    Ok(services)
}

fn check_sdk_closure(
    manifest: &MinimalManifest,
    payload_root: &Path,
    opts: &RunOptions,
    runtime: (&Path, &str),
) -> Result<(), RunError> {
    let refuse = |message: &str| RunError::InvalidServiceLaunch(message.into());
    let (runtime_root, version) = runtime;
    let entry = launch_entry_bin(manifest, &opts.mode)
        .ok_or_else(|| refuse("native admission requires an observable ELF entrypoint"))?;
    let payload_mount = crate::sandbox::payload_mount_for(&manifest.package_id);
    let view = crate::loader_preflight::LoaderView::new(
        vec![
            (payload_mount.clone(), payload_root.to_path_buf()),
            (
                crate::sandbox::runtime_mount_for(version),
                runtime_root.to_path_buf(),
            ),
        ],
        vec![crate::sandbox::runtime_library_path(version)],
        crate::loader_preflight::LdCache::Absent,
        Vec::new(),
    );
    view.check_closed(&payload_mount.join(entry))
        .map_err(|error| refuse(&error))?;
    if let Some(interpreter) = crate::loader_preflight::read_interpreter(&payload_root.join(entry))?
    {
        if !crate::sandbox::CANONICAL_LOADER_PATHS.contains(&interpreter.as_str()) {
            return Err(refuse(
                "native admission requires the canonical SDK interpreter",
            ));
        }
        view.check_closed(
            &crate::sandbox::runtime_library_path(version)
                .join(interpreter.trim_start_matches('/')),
        )
        .map_err(|error| refuse(&error))?;
    }
    Ok(())
}

fn validate_probe_options(opts: &RunOptions) -> Result<(), RunError> {
    let refuse = |message: &str| RunError::InvalidServiceLaunch(message.into());
    if !matches!(opts.mode, LaunchMode::Gui)
        || opts.no_services
        || opts.allow_unsigned_runtime
        || opts.allow_missing_runtime
        || opts.minimal_libs
        || !opts.landlock_enforce
        || opts.landlock_best_effort
        || opts.open_document.is_some()
        || opts.open_uri.is_some()
    {
        return Err(refuse(
            "native admission requires the ordinary confined signed GUI launch",
        ));
    }
    Ok(())
}

fn hash_binary(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(&mut file)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Observation made by the actual native registry host supervising the child.
/// Both identities describe executed inodes, not source declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeAdmissionObservation {
    pub native_host_blake3: String,
    pub admission: NativeAdmission,
}

/// Original host-selected target for a registered probe child.
#[derive(Clone, Copy)]
pub struct NativeAdmissionScope<'a> {
    pub package_id: &'a str,
    pub package_root: &'a str,
    pub workspace_id: &'a str,
}

/// Supervise a probe with the host's ordinary registration and retirement.
/// No output stands in for custody: the registered child must return exact
/// signed target evidence and the executed binary must remain unchanged.
///
/// # Errors
/// Refuses failed registration, timeout, excess output, changed host bytes,
/// malformed/foreign evidence or a refused admission. Always reaps the child.
pub fn observe_registered_probe(
    mut command: Command,
    scope: NativeAdmissionScope<'_>,
    register: impl FnOnce(u32) -> Result<(), String>,
    retire: impl FnOnce(u32),
) -> Result<NativeAdmissionObservation, String> {
    use std::io::Read as _;
    const OUTPUT_LIMIT: u64 = 1024 * 1024;
    let native_host_before =
        hash_binary(Path::new("/proc/self/exe")).map_err(|error| error.to_string())?;
    let executable = PathBuf::from(command.get_program());
    let expected_host = hash_binary(&executable).map_err(|error| error.to_string())?;
    command
        .arg("--admission-probe")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let gate =
        crate::registration_gate::install(&mut command).map_err(|error| error.to_string())?;
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.id();
    let result = (|| {
        register(pid)?;
        let stdout = child
            .stdout
            .take()
            .ok_or("native admission stdout is missing")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("native admission stderr is missing")?;
        let output = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let diagnostic = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        // Release only after the host retained the actual PID/start-time.
        let release = gate.release();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(60))
            .ok_or("native admission deadline overflow")?;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if release.is_ok() && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("native admission registration/wait deadline failed".to_string());
                }
            }
        };
        let bytes = output
            .join()
            .map_err(|_| "native admission output reader failed")?
            .map_err(|error| error.to_string())?;
        let diagnostics = diagnostic
            .join()
            .map_err(|_| "native admission diagnostic reader failed")?
            .map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 || diagnostics.len() > 64 * 1024 {
            return Err("native admission output exceeds its bound".into());
        }
        if !status?.success() {
            return Err(format!(
                "native admission refused: {}",
                String::from_utf8_lossy(&diagnostics)
            ));
        }
        let evidence: NativeAdmission = serde_json::from_slice(&bytes)
            .map_err(|error| format!("native admission evidence is invalid: {error}"))?;
        if evidence.package_id != scope.package_id
            || evidence.package_root != scope.package_root
            || evidence.workspace_id != scope.workspace_id
            || evidence.launcher_blake3 != expected_host
            || hash_binary(&executable).map_err(|error| error.to_string())? != expected_host
        {
            return Err("native admission evidence does not match the original target/host".into());
        }
        Ok(evidence)
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    retire(pid);
    let admission = result?;
    if hash_binary(Path::new("/proc/self/exe")).map_err(|error| error.to_string())?
        != native_host_before
    {
        return Err("native admission registry host changed during observation".into());
    }
    Ok(NativeAdmissionObservation {
        native_host_blake3: native_host_before,
        admission,
    })
}
