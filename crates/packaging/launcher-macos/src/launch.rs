//! Starting an app in the mode its signature earns.
//!
//! Both paths spawn with `POSIX_SPAWN_CLOEXEC_DEFAULT`: the child inherits
//! stdio and exactly the descriptors the request names — the runtime's
//! pre-connected service sockets — and nothing else the launcher holds.
//!
//! **App Sandbox.** The file was inspected before launch, but a file can be
//! replaced between inspection and exec. So the process is spawned
//! suspended, the *running* image is verified ([`verify_process`]: trusted
//! identity, not ad-hoc, App Sandbox enabled), and only then resumed; a
//! process that fails is killed before it executes an instruction. App
//! Sandbox itself is applied by `libsystem_secinit` as the process starts,
//! which aborts the process if it cannot.
//!
//! **Seatbelt.** The profile is applied by `/usr/bin/sandbox-exec` (SIP
//! protected), which calls `sandbox_init_with_parameters` in the child and
//! only then execs the app; if the profile does not apply it exits without
//! exec'ing anything. A preflight runs the same profile against a target
//! that cannot exist first, so a profile the kernel rejects becomes a
//! [`LaunchError::SeatbeltUnavailable`] rather than a dead child.

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString, c_char, c_int};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use launcher::capabilities::{CapabilityPlan, private_store_roots};
use launcher::netgate::{EgressDecision, GateError, LoopbackGate, proxy_env};

use crate::classify::{CodeSignature, MacLaunchMode, TrustRequirement, decide};
use crate::codesign::{adhoc_sign, inspect, verify_process};
use crate::policy::{
    DARWIN_DIR_SUFFIX_ENV, DarwinCache, PolicyError, PolicyInputs, SeatbeltNetwork, SeatbeltPolicy,
    app_environment, prepare_darwin_cache,
};
use crate::sbpl::{self, SeatbeltProfile};

/// The system's Seatbelt trampoline.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// `sandbox-exec` exits `EX_OSERR` when the profile applied and only the
/// exec failed; a profile that did not apply exits `EX_DATAERR`.
const EXEC_FAILED_AFTER_APPLY: i32 = 71;
/// Inside a root-owned directory that is empty by definition.
const PREFLIGHT_TARGET: &str = "/var/empty/tilo-seatbelt-preflight";
/// Locale variables passed through from the launcher's environment.
const INHERITED_ENV: &[&str] = &["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES"];
const PATH_ENV: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

unsafe extern "C" {
    /// Apple extension: keep `fd` open across a spawn that sets
    /// `POSIX_SPAWN_CLOEXEC_DEFAULT`.
    fn posix_spawn_file_actions_addinherit_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        fd: c_int,
    ) -> c_int;
}

/// One app launch.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub package_id: String,
    /// The Mach-O to exec; must resolve inside [`Self::payload_dir`].
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    /// The installed package, already verified against the registry's
    /// signed index.
    pub payload_dir: PathBuf,
    pub runtime_dylib_dir: Option<PathBuf>,
    /// The app's container, from [`crate::policy::prepare_container`].
    pub container_dir: PathBuf,
    pub capabilities: CapabilityPlan,
    /// A [`EgressDecision::Filtered`] launch is served by a
    /// [`LoopbackGate`] the launched app holds.
    pub egress: EgressDecision,
    /// Pre-connected service descriptors, inherited at the same numbers.
    pub inherit_fds: Vec<RawFd>,
    /// Extra environment. Under Seatbelt the container variables
    /// (`HOME`, `XDG_*`, `TMPDIR`) are set last and cannot be overridden.
    pub env: Vec<(OsString, OsString)>,
    pub trust: TrustRequirement,
}

/// Why an app was not launched. None of these leaves a process running.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("cannot resolve {path}: {source}")]
    Resolve {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{executable} is not inside the payload directory {payload}")]
    OutsidePayload {
        executable: PathBuf,
        payload: PathBuf,
    },
    #[error("could not ad-hoc sign the unsigned executable {path}: {source}")]
    AdHocSign {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Policy(#[from] PolicyError),
    #[error(
        "refusing to launch {package}: the Seatbelt sandbox could not be applied ({detail}). \
         Tilo never runs an app unsandboxed."
    )]
    SeatbeltUnavailable { package: String, detail: String },
    #[error("refusing to launch {package}: its egress gate could not start: {source}")]
    EgressGate {
        package: String,
        #[source]
        source: GateError,
    },
    #[error("refusing to launch {package}: App Sandbox could not be verified ({detail})")]
    AppSandboxUnverified { package: String, detail: String },
    #[error("could not start {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A launched app. Dropping it leaves the app running, but stops its egress
/// gate; call [`LaunchedApp::wait`] to reap it.
#[derive(Debug)]
pub struct LaunchedApp {
    pid: libc::pid_t,
    mode: MacLaunchMode,
    _gate: Option<LoopbackGate>,
}

impl LaunchedApp {
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    pub fn mode(&self) -> MacLaunchMode {
        self.mode
    }

    /// Wait for the app to exit.
    ///
    /// # Errors
    ///
    /// Returns the `waitpid` error.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        wait_pid(self.pid)
    }

    /// Send `SIGKILL`.
    ///
    /// # Errors
    ///
    /// Returns the `kill` error.
    pub fn kill(&self) -> io::Result<()> {
        signal(self.pid, libc::SIGKILL)
    }
}

/// Launch `request` under App Sandbox or Seatbelt — never neither.
///
/// # Errors
///
/// Returns [`LaunchError`] when the executable is outside its payload, the
/// policy cannot be resolved, the chosen sandbox cannot be applied or
/// verified, or the process cannot be spawned.
pub fn launch(request: &LaunchRequest) -> Result<LaunchedApp, LaunchError> {
    let payload = resolve(&request.payload_dir)?;
    let executable = resolve(&request.executable)?;
    if executable == payload || !executable.starts_with(&payload) {
        return Err(LaunchError::OutsidePayload {
            executable,
            payload,
        });
    }

    let mut signature = inspect(&executable, &request.trust);
    if signature == CodeSignature::Unsigned && cfg!(target_arch = "aarch64") {
        adhoc_sign(&executable).map_err(|source| LaunchError::AdHocSign {
            path: executable.clone(),
            source,
        })?;
        signature = inspect(&executable, &request.trust);
    }
    let mode = decide(&signature);
    tracing::info!(
        target: "tilo_launcher::macos",
        package = %request.package_id,
        ?signature,
        ?mode,
        "launching"
    );

    let (pid, gate) = match mode {
        MacLaunchMode::AppSandbox => {
            let pid = start_app_sandboxed(request, &executable)?;
            if let Err(e) = signal(pid, libc::SIGCONT) {
                kill_and_reap(pid);
                return Err(LaunchError::Spawn {
                    program: executable,
                    source: e,
                });
            }
            (pid, None)
        }
        MacLaunchMode::Seatbelt(reason) => {
            tracing::info!(
                target: "tilo_launcher::macos",
                package = %request.package_id,
                "confining with Seatbelt because {}",
                reason.describe()
            );
            start_seatbelted(request, &executable)?
        }
    };
    Ok(LaunchedApp {
        pid,
        mode,
        _gate: gate,
    })
}

/// Spawn suspended and verify the running image; the returned process is
/// still suspended.
fn start_app_sandboxed(
    request: &LaunchRequest,
    executable: &Path,
) -> Result<libc::pid_t, LaunchError> {
    let argv = app_argv(executable, &request.args);
    let fixed: Vec<(String, String)> = surface_env(&request.payload_dir);
    let env = environment(&request.env, &fixed);
    let pid = spawn(executable, &argv, &env, &request.inherit_fds, true).map_err(|source| {
        LaunchError::Spawn {
            program: executable.to_path_buf(),
            source,
        }
    })?;
    if let Err(detail) = verify_process(pid, &request.trust) {
        kill_and_reap(pid);
        return Err(LaunchError::AppSandboxUnverified {
            package: request.package_id.clone(),
            detail,
        });
    }
    Ok(pid)
}

fn start_seatbelted(
    request: &LaunchRequest,
    executable: &Path,
) -> Result<(libc::pid_t, Option<LoopbackGate>), LaunchError> {
    let gate = request
        .egress
        .policy()
        .map(|policy| LoopbackGate::start(policy.clone()))
        .transpose()
        .map_err(|source| LaunchError::EgressGate {
            package: request.package_id.clone(),
            source,
        })?;
    let darwin_cache = darwin_cache(request);
    let private_stores = private_store_roots();
    let policy = SeatbeltPolicy::resolve(&PolicyInputs {
        payload_dir: &request.payload_dir,
        runtime_dylib_dir: request.runtime_dylib_dir.as_deref(),
        container_dir: &request.container_dir,
        capabilities: &request.capabilities,
        egress: &request.egress,
        gate_port: gate.as_ref().map(|gate| gate.local_addr().port()),
        darwin_cache: darwin_cache.as_ref().map(|cache| cache.dir.as_path()),
        private_stores: &private_stores,
    })?;
    for gap in &policy.not_enforced {
        tracing::warn!(
            target: "tilo_launcher::macos",
            package = %request.package_id,
            "grant not enforced under Seatbelt (denied): {gap}"
        );
    }
    let profile = sbpl::render(&policy);
    preflight(&profile).map_err(|detail| LaunchError::SeatbeltUnavailable {
        package: request.package_id.clone(),
        detail,
    })?;

    let mut argv: Vec<OsString> = vec![OsString::from(SANDBOX_EXEC)];
    argv.extend(profile_args(&profile));
    argv.push(OsString::from("--"));
    argv.extend(app_argv(executable, &request.args));

    let mut fixed: Vec<(String, String)> = app_environment(&policy.container);
    fixed.extend(surface_env(&request.payload_dir));
    if let SeatbeltNetwork::Gate(port) = policy.network {
        fixed.extend(
            proxy_env(port)
                .into_iter()
                .map(|(key, value)| (key.to_string(), value)),
        );
    }
    if let Some(cache) = darwin_cache {
        fixed.push((DARWIN_DIR_SUFFIX_ENV.to_string(), cache.suffix));
    }
    let env = environment(&request.env, &fixed);
    let pid = spawn(
        Path::new(SANDBOX_EXEC),
        &argv,
        &env,
        &request.inherit_fds,
        false,
    )
    .map_err(|source| LaunchError::Spawn {
        program: PathBuf::from(SANDBOX_EXEC),
        source,
    })?;
    Ok((pid, gate))
}

/// The app's own per-user cache directory, so Metal keeps its compiled
/// shaders across launches. Without one the app still runs; its shaders
/// compile on every launch.
fn darwin_cache(request: &LaunchRequest) -> Option<DarwinCache> {
    let prepared = user_cache_dir()
        .and_then(|user_cache| prepare_darwin_cache(&user_cache, &request.container_dir));
    match prepared {
        Ok(cache) => Some(cache),
        Err(e) => {
            tracing::warn!(
                target: "tilo_launcher::macos",
                package = %request.package_id,
                "no shader cache for this launch: {e}"
            );
            None
        }
    }
}

/// `confstr(_CS_DARWIN_USER_CACHE_DIR)`: this user's cache directory below
/// `/var/folders`.
fn user_cache_dir() -> io::Result<PathBuf> {
    let mut buf = vec![0_u8; libc::PATH_MAX as usize];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most
    // that many, NUL included.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_CACHE_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len == 0 || len > buf.len() {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len.saturating_sub(1));
    Ok(PathBuf::from(OsString::from_vec(buf)))
}

/// Apply `profile` to a throwaway child that then tries to exec a path that
/// cannot exist. Only "applied, and the exec failed" passes.
///
/// # Errors
///
/// Returns `sandbox-exec`'s diagnostics when the profile did not apply.
pub(crate) fn preflight(profile: &SeatbeltProfile) -> Result<(), String> {
    let output = Command::new(SANDBOX_EXEC)
        .args(profile_args(profile))
        .arg("--")
        .arg(PREFLIGHT_TARGET)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{SANDBOX_EXEC} could not be run: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(EXEC_FAILED_AFTER_APPLY) && stderr.contains("execvp()") {
        Ok(())
    } else {
        Err(format!("{} ({})", stderr.trim(), output.status))
    }
}

fn profile_args(profile: &SeatbeltProfile) -> Vec<OsString> {
    let mut args = Vec::with_capacity(profile.params.len().saturating_mul(2).saturating_add(2));
    for (key, value) in &profile.params {
        args.push(OsString::from("-D"));
        args.push(OsString::from(format!("{key}={value}")));
    }
    args.push(OsString::from("-p"));
    args.push(OsString::from(&profile.text));
    args
}

fn app_argv(executable: &Path, args: &[OsString]) -> Vec<OsString> {
    std::iter::once(executable.as_os_str().to_owned())
        .chain(args.iter().cloned())
        .collect()
}

/// `PATH`, the launcher's locale, the request's variables, then `fixed`.
/// The surface the package ships (`surface.toml` at its payload root), by
/// path and hash: the app reads it, checks the hash and names it in every
/// change it signs. None for a package without one.
fn surface_env(payload_dir: &Path) -> Vec<(String, String)> {
    let file = payload_dir.join("surface.toml");
    let Ok(bytes) = std::fs::read(&file) else {
        return Vec::new();
    };
    vec![
        ("TILO_SURFACE".to_owned(), file.display().to_string()),
        (
            "TILO_SURFACE_HASH".to_owned(),
            blake3::hash(&bytes).to_hex().to_string(),
        ),
    ]
}

fn environment(
    extra: &[(OsString, OsString)],
    fixed: &[(String, String)],
) -> BTreeMap<OsString, OsString> {
    let mut env = BTreeMap::new();
    env.insert(OsString::from("PATH"), OsString::from(PATH_ENV));
    for key in INHERITED_ENV {
        if let Some(value) = std::env::var_os(key) {
            env.insert(OsString::from(key), value);
        }
    }
    for (key, value) in extra {
        env.insert(key.clone(), value.clone());
    }
    for (key, value) in fixed {
        env.insert(OsString::from(key), OsString::from(value));
    }
    env
}

fn resolve(path: &Path) -> Result<PathBuf, LaunchError> {
    std::fs::canonicalize(path).map_err(|source| LaunchError::Resolve {
        path: path.to_path_buf(),
        source,
    })
}

fn c_string(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"))
}

fn spawn(
    program: &Path,
    argv: &[OsString],
    env: &BTreeMap<OsString, OsString>,
    inherit_fds: &[RawFd],
    suspended: bool,
) -> io::Result<libc::pid_t> {
    let program = c_string(program.as_os_str())?;
    let argv: Vec<CString> = argv
        .iter()
        .map(|a| c_string(a))
        .collect::<io::Result<_>>()?;
    let envp: Vec<CString> = env
        .iter()
        .map(|(key, value)| {
            let mut pair = key.clone().into_vec();
            pair.push(b'=');
            pair.extend_from_slice(value.as_bytes());
            CString::new(pair)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"))
        })
        .collect::<io::Result<_>>()?;
    let argv_ptrs: Vec<*mut c_char> = argv
        .iter()
        .map(|a| a.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();
    let envp_ptrs: Vec<*mut c_char> = envp
        .iter()
        .map(|e| e.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect();

    let mut flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
        | libc::POSIX_SPAWN_SETSIGDEF
        | libc::POSIX_SPAWN_SETSIGMASK;
    if suspended {
        flags |= libc::POSIX_SPAWN_START_SUSPENDED;
    }
    let flags =
        libc::c_short::try_from(flags).map_err(|_| io::Error::other("spawn flags overflow"))?;

    let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
    let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
    // SAFETY: `attr` and `actions` are initialised before use and destroyed
    // on every path below; the signal sets are initialised by
    // sigfillset/sigemptyset; every pointer array is NUL-terminated and
    // outlives the call; `pid` is written by `posix_spawn`.
    unsafe {
        check(libc::posix_spawnattr_init(&raw mut attr))?;
        if let Err(e) = check(libc::posix_spawn_file_actions_init(&raw mut actions)) {
            libc::posix_spawnattr_destroy(&raw mut attr);
            return Err(e);
        }
        let result = (|| {
            check(libc::posix_spawnattr_setflags(&raw mut attr, flags))?;
            let mut defaults: libc::sigset_t = 0;
            libc::sigfillset(&raw mut defaults);
            libc::sigdelset(&raw mut defaults, libc::SIGKILL);
            libc::sigdelset(&raw mut defaults, libc::SIGSTOP);
            check(libc::posix_spawnattr_setsigdefault(
                &raw mut attr,
                &raw const defaults,
            ))?;
            let mut mask: libc::sigset_t = 0;
            libc::sigemptyset(&raw mut mask);
            check(libc::posix_spawnattr_setsigmask(
                &raw mut attr,
                &raw const mask,
            ))?;
            for fd in [0, 1, 2].iter().chain(inherit_fds) {
                check(posix_spawn_file_actions_addinherit_np(
                    &raw mut actions,
                    *fd,
                ))?;
            }
            let mut pid: libc::pid_t = 0;
            check(libc::posix_spawn(
                &raw mut pid,
                program.as_ptr(),
                &raw const actions,
                &raw const attr,
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            ))?;
            Ok(pid)
        })();
        libc::posix_spawn_file_actions_destroy(&raw mut actions);
        libc::posix_spawnattr_destroy(&raw mut attr);
        result
    }
}

fn check(rc: c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

fn signal(pid: libc::pid_t, sig: c_int) -> io::Result<()> {
    // SAFETY: `kill` on a pid this process spawned and has not reaped.
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn wait_pid(pid: libc::pid_t) -> io::Result<ExitStatus> {
    loop {
        let mut status: c_int = 0;
        // SAFETY: `pid` is our own child; `status` is writable.
        let rc = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        if rc == pid {
            return Ok(ExitStatus::from_raw(status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A process spawned suspended does not act on `SIGKILL` until it is
/// resumed; the pending kill is taken before any user instruction runs.
fn kill_and_reap(pid: libc::pid_t) {
    let _ = signal(pid, libc::SIGKILL);
    let _ = signal(pid, libc::SIGCONT);
    let _ = wait_pid(pid);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple-signed with `com.apple.security.app-sandbox`, and — unlike
    /// Apple's apps, which launch constraints kill unless launchd starts
    /// them — exec'able from any parent.
    const SANDBOXED: &str = "/usr/libexec/languageassetd";

    fn request(executable: &str, trust: TrustRequirement) -> LaunchRequest {
        LaunchRequest {
            package_id: "test".into(),
            executable: PathBuf::from(executable),
            args: Vec::new(),
            payload_dir: PathBuf::from("/"),
            runtime_dylib_dir: None,
            container_dir: PathBuf::from("/nonexistent"),
            capabilities: CapabilityPlan::empty(),
            egress: EgressDecision::Blocked,
            inherit_fds: Vec::new(),
            env: Vec::new(),
            trust,
        }
    }

    #[test]
    fn a_profile_the_kernel_rejects_is_reported_not_run() {
        let broken = SeatbeltProfile {
            text: "(version 1)(deny default".into(),
            params: Vec::new(),
        };
        let err = preflight(&broken).unwrap_err();
        assert!(err.contains("syntax error"), "{err}");

        let missing_param = SeatbeltProfile {
            text: "(version 1)(deny default)(allow file-read* (subpath (param \"NOPE\")))".into(),
            params: Vec::new(),
        };
        assert!(preflight(&missing_param).is_err());

        let minimal = SeatbeltProfile {
            text: "(version 1)(deny default)".into(),
            params: Vec::new(),
        };
        preflight(&minimal).unwrap();
    }

    #[test]
    fn every_rendered_profile_shape_applies() {
        use crate::policy::SeatbeltPolicy;
        let tmp = tempfile::tempdir().unwrap();
        let dir = |name: &str| {
            let path = tmp.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::canonicalize(path).unwrap()
        };
        for network in [
            SeatbeltNetwork::Denied,
            SeatbeltNetwork::Gate(9050),
            SeatbeltNetwork::Unrestricted,
        ] {
            let policy = SeatbeltPolicy {
                payload: dir("payload"),
                runtime: Some(dir("runtime")),
                container: dir("container"),
                darwin_cache: Some(dir("cache")),
                read_only: vec![dir("ro")],
                read_write: vec![dir("rw with \"quotes\" (and parens)")],
                network,
                hidden: vec![dir("store")],
                pinned: vec![tmp.path().join("missing"), dir("pin")],
                not_enforced: Vec::new(),
            };
            let profile = sbpl::render(&policy);
            if let Err(e) = preflight(&profile) {
                panic!("{e}\n{}", profile.text);
            }
        }
    }

    #[test]
    fn a_trusted_sandboxed_process_is_verified_while_suspended() {
        let req = request(SANDBOXED, TrustRequirement::Custom("anchor apple"));
        let pid = start_app_sandboxed(&req, Path::new(SANDBOXED)).unwrap();
        kill_and_reap(pid);
    }

    #[test]
    fn a_running_process_that_does_not_qualify_is_killed_before_it_runs() {
        // Apple-signed, but no App Sandbox entitlement.
        let req = request("/usr/bin/true", TrustRequirement::Custom("anchor apple"));
        let err = start_app_sandboxed(&req, Path::new("/usr/bin/true")).unwrap_err();
        assert!(
            matches!(&err, LaunchError::AppSandboxUnverified { detail, .. } if detail.contains("app-sandbox")),
            "{err}"
        );
        // Sandboxed, but not a Developer ID signature.
        let req = request(SANDBOXED, TrustRequirement::DeveloperId);
        let err = start_app_sandboxed(&req, Path::new(SANDBOXED)).unwrap_err();
        assert!(
            matches!(&err, LaunchError::AppSandboxUnverified { detail, .. } if detail.contains("trust requirement")),
            "{err}"
        );
    }

    #[test]
    fn an_executable_outside_its_payload_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let mut req = request("/usr/bin/true", TrustRequirement::DeveloperId);
        req.payload_dir = tmp.path().to_path_buf();
        assert!(matches!(
            launch(&req),
            Err(LaunchError::OutsidePayload { .. })
        ));
    }
}
