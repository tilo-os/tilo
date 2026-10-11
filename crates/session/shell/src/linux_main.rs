//! Compositor binary entry point.
//!
//! Picks a [`Mode`] from `--mode`, then delegates to the matching
//! `compositor::*::run` function. [`Mode::Native`] is the default (real
//! smithay server, vello+wgpu winit backend); [`Mode::Hosted`] is an
//! explicit dev opt-in (`--mode hosted`) for the legacy winit-hosted path.
//!
//! Side-channel modes are resolved before `parse_mode`: a removed run mode
//! (`--portal-server-only`) is refused with exit code 2 before any setup, so a
//! stale unit fails loudly instead of starting a compositor; `--firstboot-only`
//! runs machine setup and exits, while the host-session `--firstboot` hook
//! performs that idempotent setup and continues into the compositor.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use shell::compositor::hosted::HostedOptions;
use shell::compositor::{self, Mode};
use shell::firstboot;
use shell::shell::identity_provisioning;

const SECRET_SERVICE_STARTUP_ATTEMPTS: usize = 8;
const SECRET_SERVICE_STARTUP_RETRY_DELAY: std::time::Duration =
    std::time::Duration::from_millis(250);

/// The in-sandbox Landlock shim token. Only `tilo-launcher` implements it;
/// the compositor never builds a sandbox.
const SANDBOX_EXEC_SUBCOMMAND: &str = "__sandbox-exec";

/// Refuse `shell __sandbox-exec …`. Something that still execs the
/// compositor as a Landlock shim expects the program after `--` to run
/// confined; starting a compositor, or the program unconfined, would both
/// be silent failures of that expectation, so the process exits instead.
/// Split out of `main` to keep it short.
#[allow(
    clippy::exit,
    reason = "refusing before any compositor state exists; there is no caller to return to"
)]
fn refuse_sandbox_exec(raw_args: &[String]) {
    if raw_args
        .get(1)
        .is_some_and(|a| a == SANDBOX_EXEC_SUBCOMMAND)
    {
        eprintln!(
            "tilo-sandbox-exec: the compositor does not install sandboxes; \
             launch through tilo-launcher"
        );
        std::process::exit(126);
    }
}

/// Log the GPU-pin decision taken before the tracing subscriber existed.
/// Split out of `main` to keep it short.
fn log_gpu_pin(gpu_pin: Option<&compositor::render_node::ProcessPin>) {
    let Some(pin) = gpu_pin else {
        return;
    };
    tracing::info!(
        target: "shell::compositor::gpu",
        node = %pin.node.display(),
        driver = pin.driver.as_deref().unwrap_or("<unknown>"),
        "session pinned to render node: {}",
        if pin.applied.is_empty() {
            "no loader variables derived".to_string()
        } else {
            pin.applied
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    );
    for note in &pin.notes {
        tracing::warn!(target: "shell::compositor::gpu", "{note}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingIdentityFailureAction {
    /// The native compositor can present the owner-only restore/unlock flow.
    EnterTrustedRecovery,
    /// There is no trusted recovery surface in this process shape.
    FailClosed,
}

/// Decide how startup handles a failure while checking an *existing* identity.
///
/// An unavailable keyring and malformed or mismatched persisted identity
/// artifacts both require an explicit owner recovery ceremony in native mode.
/// They must never be interpreted as permission to create a replacement
/// identity. Errors which do not prove an invalid stored identity (such as
/// permission or storage failures) remain fail-closed.
fn existing_identity_failure_action(
    provision_requested: bool,
    mode: Mode,
    error: &std::io::Error,
) -> ExistingIdentityFailureAction {
    if provision_requested || mode == Mode::Hosted {
        return ExistingIdentityFailureAction::FailClosed;
    }

    if error.kind() == std::io::ErrorKind::InvalidData {
        ExistingIdentityFailureAction::EnterTrustedRecovery
    } else {
        ExistingIdentityFailureAction::FailClosed
    }
}

fn verify_existing_identity_with_retry(
    mut verify: impl FnMut() -> Result<firstboot::ProvisionOutcome, std::io::Error>,
) -> Result<firstboot::ProvisionOutcome, std::io::Error> {
    let mut attempt = 1;
    loop {
        match verify() {
            Err(error)
                if identity_provisioning::is_secret_service_unavailable(&error)
                    && attempt < SECRET_SERVICE_STARTUP_ATTEMPTS =>
            {
                tracing::info!(
                    attempt,
                    max_attempts = SECRET_SERVICE_STARTUP_ATTEMPTS,
                    "Secret Service is not ready; retrying existing identity verification"
                );
                std::thread::sleep(SECRET_SERVICE_STARTUP_RETRY_DELAY);
                attempt = attempt.saturating_add(1);
            }
            result => return result,
        }
    }
}

/// Once the identity chain verifies, an interrupted pre-acquisition journal
/// is residue, not a reason to send an established user through recovery.
fn reconcile_verified_onboarding_progress(
    identity_dir: &std::path::Path,
) -> Option<std::process::ExitCode> {
    match shell::onboarding_view::clear_verified_pre_acquisition_progress(identity_dir) {
        Ok(true) => {
            tracing::info!(
                "cleared interrupted pre-acquisition onboarding journal after identity verification"
            );
            None
        }
        Ok(false) => None,
        Err(error) => {
            tracing::error!(
                %error,
                "could not reconcile verified identity onboarding progress"
            );
            Some(std::process::ExitCode::FAILURE)
        }
    }
}

/// Verify an existing identity record, entering trusted recovery or failing
/// closed as its failure calls for. `Some` is the exit code to stop with.
fn verify_existing_identity(
    identity_dir: &std::path::Path,
    provision_requested: bool,
) -> Option<std::process::ExitCode> {
    let paths = firstboot::collaboration_identity_paths();
    if let Err(error) = verify_existing_identity_with_retry(|| {
        firstboot::ensure_collaboration_identity(identity_dir, &paths)
    }) {
        match existing_identity_failure_action(
            provision_requested,
            compositor::parse_mode_from_args(),
            &error,
        ) {
            ExistingIdentityFailureAction::EnterTrustedRecovery => {
                // An existing schema-2 record remains the person's
                // identity even when its key is unavailable or its
                // collaboration chain cannot verify. Never replace it
                // or mint a second identity; native trusted chrome
                // offers keyring recovery or encrypted restore.
                tracing::warn!(
                    %error,
                    "existing Tilo identity needs recovery; opening trusted recovery"
                );
                mark_identity_recovery();
            }
            ExistingIdentityFailureAction::FailClosed => {
                if provision_requested {
                    tracing::error!("could not provision collaboration identity: {error}");
                } else if compositor::parse_mode_from_args() == Mode::Hosted {
                    tracing::error!(
                        "hosted mode cannot recover an existing identity; start a native Tilo session to unlock or restore it: {error}"
                    );
                } else {
                    tracing::error!("Tilo identity chain is incomplete or invalid: {error}");
                }
                return Some(std::process::ExitCode::FAILURE);
            }
        }
    }
    None
}

fn prepare_identity(provision_requested: bool) -> Option<std::process::ExitCode> {
    match identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) if identity_provisioning::needs_first_run_flow(&store) => {
            if provision_requested {
                let Some(identity_dir) = store.path().parent() else {
                    tracing::error!("identity record has no parent directory");
                    return Some(std::process::ExitCode::FAILURE);
                };
                let paths = firstboot::collaboration_identity_paths();
                let result = if compositor::parse_install_mode_from_args()
                    == compositor::InstallMode::HostDesktop
                {
                    firstboot::ensure_collaboration_identity_for_legacy_linux_host(
                        identity_dir,
                        &paths,
                    )
                } else {
                    firstboot::ensure_collaboration_identity(identity_dir, &paths)
                };
                return Some(match result {
                    Ok(_) => {
                        tracing::info!(
                            path = %store.path().display(),
                            "Tilo collaboration identity is provisioned"
                        );
                        std::process::ExitCode::SUCCESS
                    }
                    Err(error) => {
                        tracing::error!("could not provision collaboration identity: {error}");
                        std::process::ExitCode::FAILURE
                    }
                });
            }
            if compositor::parse_mode_from_args() == Mode::Hosted {
                tracing::error!(
                    path = %store.path().display(),
                    "hosted mode cannot create the first identity; start a native Tilo session once to complete setup"
                );
                return Some(std::process::ExitCode::FAILURE);
            }
            tracing::info!(
                "no Tilo identity found at {}; native compositor chrome will run {} setup",
                store.path().display(),
                if compositor::parse_install_mode_from_args()
                    == compositor::InstallMode::HostDesktop
                {
                    "the reduced host-session identity"
                } else {
                    "first-login"
                }
            );
        }
        Ok(store) => {
            let Some(identity_dir) = store.path().parent() else {
                tracing::error!("identity record has no parent directory");
                return Some(std::process::ExitCode::FAILURE);
            };
            if let Some(exit) = verify_existing_identity(identity_dir, provision_requested) {
                return Some(exit);
            }
            if !provision_requested
                && let Some(exit) = reconcile_verified_onboarding_progress(identity_dir)
            {
                return Some(exit);
            }
            if provision_requested {
                tracing::info!("Tilo collaboration identity is provisioned");
                return Some(std::process::ExitCode::SUCCESS);
            }
            tracing::info!("loaded Tilo identity from {}", store.path().display());
        }
        Err(error) => {
            tracing::error!("could not initialise identity store: {error}");
            return Some(std::process::ExitCode::FAILURE);
        }
    }
    None
}

/// Select the trusted identity-recovery surface for the compositor, which
/// reads the marker from its own environment.
#[allow(
    clippy::disallowed_methods,
    reason = "set before compositor startup; the only thread running then (font warm-up) never reads it"
)]
fn mark_identity_recovery() {
    // SAFETY: only the font warm-up thread may run yet; it never reads this
    // variable, and the marker only selects a stricter recovery surface.
    unsafe { std::env::set_var(identity_provisioning::IDENTITY_RECOVERY_ENV, "1") };
}

fn verify_collaboration_identity_entrypoint(provision_requested: bool) -> std::process::ExitCode {
    if provision_requested {
        eprintln!(
            "error: --provision-identity and --verify-collaboration-identity are mutually exclusive"
        );
        return std::process::ExitCode::from(2);
    }
    let store = match identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("identity verification failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let Some(identity_dir) = store.path().parent() else {
        eprintln!("identity verification failed: primary identity has no parent directory");
        return std::process::ExitCode::FAILURE;
    };
    let now_ns = tilo_clock::unix_now_ns();
    if let Err(error) = firstboot::verify_collaboration_identity(
        identity_dir,
        &firstboot::collaboration_identity_paths(),
        now_ns,
    ) {
        eprintln!("identity verification failed: {error}");
        return std::process::ExitCode::FAILURE;
    }
    println!("Tilo collaboration identity verified");
    std::process::ExitCode::SUCCESS
}

/// Make the person's and this device's settings records the session's
/// settings document. Readers and writers in this process go through them;
/// `settings.json` becomes the copy other processes read and write, which
/// the session keeps current and takes their writes in from.
fn dictionary_identity_owner() -> Option<[u8; 32]> {
    let store = identity_provisioning::DiskIdentityStore::from_env().ok()?;
    runtime_identity::load_metadata_from_path(store.path())
        .ok()
        .map(|identity| identity.group_public_key)
}

fn legacy_dictionary_owner() -> Option<[u8; 32]> {
    let store = identity_provisioning::DiskIdentityStore::from_env().ok()?;
    let path = store.path().parent()?.join("primary.json");
    runtime_identity::load_metadata_from_path(&path)
        .ok()
        .map(|identity| identity.group_public_key)
}

fn session_host_view(
    session: std::sync::Arc<settings_store::SessionSettings>,
    copy_watched: bool,
) -> std::sync::Arc<dyn settings_store::SettingsSource> {
    // With the copy watched, its watch takes other processes' writes in and
    // the view sleeps between reads; without it the view looks on a timer.
    let view = if copy_watched {
        session.host_view()
    } else {
        session.host_view_polling_copy(settings_store::live::POLL_INTERVAL)
    };
    match view {
        Ok(source) => source,
        Err(error) => {
            tracing::warn!(%error, "settings read view unavailable; using synchronous records");
            session
        }
    }
}

fn install_session_settings() {
    let Some(paths) = settings_store::SessionPaths::configured() else {
        tracing::warn!("no config directory; settings are not kept this session");
        return;
    };
    let copy = paths.copy.clone();
    match settings_store::SessionSettings::open(paths) {
        Ok(session) => {
            let session = std::sync::Arc::new(session);
            // Another process writing the copy (Settings, a CLI) is taken in
            // as it lands, which wakes every app following its settings.
            let taken_in = std::sync::Arc::downgrade(&session);
            let copy_watched = match settings_store::watch::watch_settings_at(&copy, move || {
                if let Some(session) = taken_in.upgrade() {
                    session.refresh();
                }
            }) {
                Ok(watch) => {
                    static COPY_WATCH: std::sync::OnceLock<settings_store::watch::SettingsWatch> =
                        std::sync::OnceLock::new();
                    // Only the first session's watch is kept; a later one
                    // would be dropped here and watch nothing.
                    COPY_WATCH.set(watch).is_ok()
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "settings copy watch unavailable; other processes' writes reach apps within a second"
                    );
                    false
                }
            };
            let source = session_host_view(session, copy_watched);
            let dictionary_root = copy.parent().map(|root| root.join("personal-dictionary"));
            let source = match dictionary_root
                .and_then(|root| settings_store::personal_dictionary::Service::new(root).ok())
            {
                Some(service) => {
                    use settings_store::personal_dictionary::Vocabulary as _;
                    // The legacy global file has no account identity. Claim it
                    // once for the verified local person, never for an app or
                    // a room's XDG directory and never again after reset.
                    let legacy_owner = legacy_dictionary_owner();
                    if let Some(owner) = dictionary_identity_owner()
                        && Some(owner) == legacy_owner
                        && let Some(reader) = service.bind(Some(owner))
                        && let Some(path) = copy.parent().map(|root| root.join("userdict.txt"))
                        && let Ok(metadata) = std::fs::symlink_metadata(&path)
                        && metadata.file_type().is_file()
                        && metadata.len() <= 512 * 1024
                    {
                        use std::io::Read;
                        use std::os::unix::fs::OpenOptionsExt;
                        let mut text = String::new();
                        if let Ok(file) = std::fs::OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(&path)
                            && file.take(512 * 1024 + 1).read_to_string(&mut text).is_ok()
                            && let Ok(snapshot) = reader.snapshot()
                            && let Err(error) =
                                service.import_legacy(&reader, snapshot.revision, owner, &text)
                        {
                            tracing::warn!(%error, "legacy personal dictionary import unavailable");
                        }
                    }
                    let host = std::sync::Arc::new(
                        settings_store::personal_dictionary::host::Host::new(
                            source,
                            std::sync::Arc::new(service),
                            std::sync::Arc::new(dictionary_identity_owner),
                        )
                        .with_legacy_owner(legacy_owner),
                    );
                    if let Ok(store) = identity_provisioning::DiskIdentityStore::from_env()
                        && let Some(directory) =
                            store.path().parent().filter(|directory| directory.is_dir())
                    {
                        let weak = std::sync::Arc::downgrade(&host);
                        if let Ok(watch) =
                            settings_store::watch::watch_identity_at(directory, move || {
                                if let Some(host) = weak.upgrade() {
                                    settings_store::SettingsSource::dictionary_service(
                                        host.as_ref(),
                                    );
                                }
                            })
                        {
                            static IDENTITY_WATCH: std::sync::OnceLock<
                                settings_store::watch::SettingsWatch,
                            > = std::sync::OnceLock::new();
                            let _ = IDENTITY_WATCH.set(watch);
                        }
                    }
                    host as std::sync::Arc<dyn settings_store::SettingsSource>
                }
                None => source,
            };
            settings_store::install_source(Some(source));
        }
        Err(error) => tracing::error!(
            %error,
            "cannot open the settings records; this session reads and writes settings.json itself"
        ),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "startup decisions must remain in one ordered, single-threaded entry point before the compositor and its long-lived services start"
)]
pub fn run() -> std::process::ExitCode {
    // Automerge isolation workers re-enter this binary; serve them before
    // anything else starts.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return std::process::ExitCode::from(code);
    }
    let early_args: Vec<String> = std::env::args().collect();
    if let Some(code) = shell::shader_seed::dispatch(&early_args) {
        return code;
    }
    // Answered before identity, GPU or session setup, so asking a host what
    // it has installed touches none of them.
    if let Some(version) = version_request(std::env::args().skip(1)) {
        println!("{version}");
        return std::process::ExitCode::SUCCESS;
    }
    // Refused before any setup: a removed run mode must neither start a
    // compositor nor claim anything a running session owns.
    if let Some(refusal) = removed_run_mode_refusal(&early_args) {
        eprintln!("error: {refusal}");
        return std::process::ExitCode::from(2);
    }
    if let Err(error) = shell::shell::install_approved_checks_from_args(std::env::args_os().skip(1))
    {
        eprintln!("approved project check configuration refused: {error}");
        return std::process::ExitCode::FAILURE;
    }
    if !early_args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--firstboot-only" | "--provision-identity" | "--verify-collaboration-identity"
        )
    }) && let Err(error) = shell::shader_seed::prepare_session()
    {
        eprintln!("session shader seed unavailable ({error}); compiling normally");
    }
    // `init()` is a no-op unless the env var is set, so production boots pay
    // zero cost; the scope guard dumps recorded events to disk when `main` returns.
    tilo_startup_metrics::init();
    let _startup_dump = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("main-start");

    // The shell's copy and every toolkit widget it draws, in the desktop's
    // language.
    shell::shell::i18n::ensure();

    // Inspect process shape while startup is still single-threaded. Host
    // packages pass an explicit installation context; deriving it from a
    // distro name, desktop variable or backend would conflate independent
    // facts and break on a different display manager.
    let raw_args: Vec<String> = std::env::args().collect();
    refuse_sandbox_exec(&raw_args);
    let arg_refs: Vec<&str> = raw_args
        .iter()
        .skip(1)
        .map(std::string::String::as_str)
        .collect();
    if compositor::parse_install_mode(&arg_refs) == compositor::InstallMode::HostDesktop {
        match identity_provisioning::configure_host_identity_root() {
            Ok(root) => eprintln!(
                "Tilo host session: device identity will be stored under {}",
                root.display()
            ),
            Err(error) => {
                eprintln!("error: cannot resolve host-session identity storage: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    let firstboot_requested = compositor::parse_firstboot_from_args();
    let firstboot_only_requested = compositor::parse_firstboot_only_from_args();
    let provision_identity_requested = compositor::parse_provision_identity_from_args();
    let verify_identity_requested = compositor::parse_verify_collaboration_identity_from_args();
    // Systemd also invokes this binary for setup and identity checks. Those
    // commands never paint: font discovery and shader warmup only compete
    // with the greeter and the session they are preparing.
    let paints =
        !(firstboot_only_requested || provision_identity_requested || verify_identity_requested);

    // Decide which GPU this session runs on before anything opens one — the
    // warmup thread below builds a wgpu context, and on a multi-GPU box its
    // adapter may disagree with what the compositor advertises to clients.
    // Pinning here, while `main` is still single-threaded, makes the
    // compositor and every launched app agree on one device.
    let gpu_pin = paints.then(compositor::render_node::pin_process).flatten();
    // The single-threaded start is over: startup marks are written from their
    // own thread from here on, never by the thread that marks.
    tilo_startup_metrics::start_writer();

    // Cold-start warmup on a background thread, as early as possible.
    // cosmic-text's `FontSystem::new` (mmaps every face, up to ~1s) and
    // vello's `Renderer::new` (compiles ~30 WGSL pipelines) dominate
    // time-to-first-paint; warming them here gets the OS page cache,
    // fontconfig cache, and process-wide wgpu context hot before the
    // compositor thread's real builds. Kept alive on the main-fn stack so the thread survives whatever `main` blocks on.
    //
    // FontSystem warmup is on by default (~90ms / 22% of first-paint). Vello
    // scratch-render warmup is off by default — it contends for the
    // process-wide render-context mutex and measured slower than no warmup.
    // TILO_STARTUP_WARM=off disables FontSystem warmup; TILO_STARTUP_WARM_VELLO=1 opts back in.
    let want_vello = std::env::var("TILO_STARTUP_WARM_VELLO")
        .ok()
        .is_some_and(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "off" | "false" | "no"
            )
        });
    let _warmup_handle = paints
        .then(|| ui::warmup::spawn_warmup(!want_vello))
        .flatten();

    // RUST_LOG as `fmt::init` reads it, with the per-launch timing lines kept:
    // the open-latency gate reads them from the session log.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref()),
        ))
        .init();
    shell::crash_log::install("tilo shell panic");
    // After the crash log: a panic outside a helper's panic boundary still
    // ends the compositor, in every build. Inside one (the collaboration
    // broker's thread) it unwinds to the helper's owner, which restarts it.
    tilo_rt::panic_boundary::abort_outside_boundaries();
    log_gpu_pin(gpu_pin.as_ref());
    compositor::dev_env::report_build_mode();

    if firstboot_requested && firstboot_only_requested {
        eprintln!(
            "error: --firstboot and --firstboot-only are mutually exclusive; \
             --firstboot continues into the compositor while --firstboot-only exits."
        );
        return std::process::ExitCode::from(2);
    }

    if firstboot_only_requested {
        if let Err(e) = firstboot::run() {
            tracing::error!("firstboot-only: setup failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
        return std::process::ExitCode::SUCCESS;
    }

    if verify_identity_requested {
        return verify_collaboration_identity_entrypoint(provision_identity_requested);
    }

    if firstboot_requested {
        if let Err(e) = firstboot::run() {
            // Don't fail the boot — log and continue. A non-zero exit still poisons
            // the wider boot transaction even though the systemd unit marks first-boot done regardless, so we swallow the error here.
            tracing::warn!("firstboot: setup hit an error ({e}); continuing with compositor boot");
        }
        if compositor::parse_install_mode(&arg_refs) == compositor::InstallMode::HostDesktop
            && let Err(e) = firstboot::import_host_input_settings()
        {
            tracing::warn!("firstboot: host input import failed ({e}); using Tilo defaults");
        }
    }
    // `--firstboot` is the host-session wrapper hook: it performs setup then
    // falls through to the normal compositor entry point. System units use
    // `--firstboot-only` instead.

    // `--provision-identity` is migration/repair for an existing record.
    // Creation is interactive and lives in trusted native-compositor chrome;
    // a pre-graphical winit app has no display server to connect to.
    if let Some(exit_code) = prepare_identity(provision_identity_requested) {
        return exit_code;
    }

    // The handle deliberately lives until the compositor exits. Start it only
    // for the actual desktop path: first-boot-only, verification,
    // and repair commands must not bind the user's collaboration endpoint.
    // Startup is fail-closed when the administrator supplied a broken
    // bind/advertise configuration, so the UI never mints unreachable URLs.
    let _invite_broker = match identity_provisioning::start_configured_invite_broker() {
        Ok(server) => server,
        Err(error) => {
            eprintln!("error: cannot start configured invite broker: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // After first boot has written what it imports, so the records take it
    // in, and before the compositor's first read.
    install_session_settings();

    let enable_peer_sync = compositor::parse_peer_sync_from_args();
    let enable_portals = compositor::parse_portals_from_args();
    match compositor::parse_mode_from_args() {
        Mode::Hosted => compositor::hosted::run_with_full_options(HostedOptions {
            enable_peer_sync,
            enable_portals,
        }),
        // Native mode serves portals unconditionally — the shipped session runs
        // `shell --backend drm` with no flags, so gating file open/save behind
        // an opt-in flag meant no working file dialog. `--enable-portals` is still accepted for existing units/scripts.
        Mode::Native => {
            let _ = enable_portals;
            tilo_startup_metrics::mark("compositor-native-run-start");
            compositor::native::run();
        }
    }
    // Layout changes are saved on a background thread; the process must not
    // exit ahead of them.
    shell::shell::persist::wait_for_saves_within(std::time::Duration::from_secs(10));
    std::process::ExitCode::SUCCESS
}

/// The refusal for a removed run-mode flag among the process arguments
/// (`args[0]` is the program name).
fn removed_run_mode_refusal(args: &[String]) -> Option<&'static str> {
    let flags: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    compositor::removed_run_mode_refusal(&flags)
}

/// The session release, as packaged (`dist/VERSION`), for a lone
/// `--version` or `-V`.
fn version_request(mut args: impl Iterator<Item = String>) -> Option<String> {
    let first = args.next()?;
    (matches!(first.as_str(), "--version" | "-V") && args.next().is_none()).then(|| {
        let version = include_str!("../../../../dist/VERSION").trim();
        format!("tilo-shell {version}")
    })
}

#[cfg(test)]
mod tests {
    use super::{ExistingIdentityFailureAction, existing_identity_failure_action};

    #[test]
    fn version_is_answered_alone_and_names_the_packaged_release() {
        let ask = |args: &[&str]| super::version_request(args.iter().map(|a| (*a).to_string()));
        let packaged = include_str!("../../../../dist/VERSION").trim();
        assert_eq!(ask(&["--version"]), Some(format!("tilo-shell {packaged}")));
        assert_eq!(ask(&["-V"]), Some(format!("tilo-shell {packaged}")));
        assert_eq!(ask(&[]), None);
        assert_eq!(ask(&["--backend", "drm"]), None);
        assert_eq!(ask(&["--version", "--backend"]), None);
    }
    use shell::compositor::Mode;
    use std::io;

    #[test]
    fn native_recovery_defers_only_an_invalid_existing_chain() {
        // Phase `only_native_desktop_startup_can_defer_an_invalid_existing_chain_to_recovery`.
        {
            let invalid_cert = io::Error::new(
                io::ErrorKind::InvalidData,
                "device certificate signature is invalid",
            );
            let unavailable_keyring = io::Error::from_raw_os_error(libc::ENOKEY);

            assert_eq!(
                existing_identity_failure_action(false, Mode::Native, &invalid_cert),
                ExistingIdentityFailureAction::EnterTrustedRecovery,
            );
            assert_eq!(
                existing_identity_failure_action(false, Mode::Native, &unavailable_keyring),
                ExistingIdentityFailureAction::FailClosed,
                "transient keyring unavailability must not be presented as identity corruption",
            );
            assert_eq!(
                existing_identity_failure_action(false, Mode::Hosted, &invalid_cert),
                ExistingIdentityFailureAction::FailClosed,
            );
            assert_eq!(
                existing_identity_failure_action(true, Mode::Native, &invalid_cert),
                ExistingIdentityFailureAction::FailClosed,
            );
        }
        // Phase `native_recovery_does_not_mask_unrelated_startup_failures`.
        {
            let permission_denied = io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity directory is not private",
            );
            assert_eq!(
                existing_identity_failure_action(false, Mode::Native, &permission_denied),
                ExistingIdentityFailureAction::FailClosed,
            );
        }
    }
}
