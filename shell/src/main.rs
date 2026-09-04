//! Compositor binary entry point.
//!
//! Picks a [`Mode`] from `--mode`, then delegates to the matching
//! `compositor::*::run` function. [`Mode::Native`] is the default (real
//! smithay server, vello+wgpu winit backend); [`Mode::Hosted`] is an
//! explicit dev opt-in (`--mode hosted`) for the legacy iced-on-winit path.
//!
//! Side-channel modes are resolved before `parse_mode`: `--portal-server-only`
//! runs only the `xdg-desktop-portal` D-Bus backend (so it doesn't race the
//! compositor for `wayland-N`); `--firstboot-only` runs machine setup and
//! exits, while the host-session `--firstboot` hook performs that idempotent
//! setup and continues into the compositor.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use shell::compositor::hosted::HostedOptions;
use shell::compositor::{self, Mode};
use shell::shell::identity_provisioning;
use shell::{firstboot, portal};

/// Install a panic hook that writes a crash report to
/// `~/.cache/qos/crashes/<timestamp>.log` and echoes the panic message +
/// log path to stderr — without this, a session launched outside a
/// terminal (greetd, .desktop double-click) discards stderr, making "the shell just disappeared" unactionable.
///
/// Chains to the default hook AFTER writing the file so a developer running from a terminal still sees the colored panic + backtrace.
fn install_crash_log_hook() {
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("qos")
        .join("crashes");
    let _ = std::fs::create_dir_all(&dir);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ts = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
        let path = dir.join(format!("{ts}.log"));
        let backtrace = std::backtrace::Backtrace::force_capture();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<unknown payload>".to_string());
        let loc = info.location().map_or_else(
            || "<unknown location>".to_string(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let body = format!(
            "qos shell panic\n\
             ===============\n\
             timestamp: {ts}\n\
             location:  {loc}\n\
             message:   {payload}\n\n\
             backtrace:\n{backtrace}\n"
        );
        let wrote = std::fs::write(&path, &body).is_ok();
        if wrote {
            eprintln!("\n[qos] crash log written to {}", path.display());
        } else {
            eprintln!("\n[qos] FAILED to write crash log to {}", path.display());
        }
        default_hook(info);
    }));
}

/// Landlock launcher shim short-circuit: `shell __sandbox-exec --ro PATH... --rw
/// PATH... [--net-restricted] -- APP-ARGV`, execed by bwrap from inside the
/// sandbox. Applies the in-kernel Landlock ruleset and `execvp`s the real
/// app when `raw_args` is that invocation; otherwise a no-op. Split out of
/// `main` to keep it short.
fn maybe_run_sandbox_exec(raw_args: &[String]) {
    if raw_args
        .get(1)
        .is_some_and(|a| a == shell::sandbox::SANDBOX_EXEC_SUBCOMMAND)
    {
        tracing_subscriber::fmt::init();
        shell::sandbox::run_sandbox_exec(raw_args.get(2..).unwrap_or(&[]));
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

fn prepare_identity(provision_requested: bool) -> Option<std::process::ExitCode> {
    match identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) if identity_provisioning::needs_first_run_flow(&store) => {
            if provision_requested {
                tracing::info!(
                    path = %store.path().display(),
                    "identity repair found no existing identity; interactive native setup will handle creation"
                );
                return Some(std::process::ExitCode::SUCCESS);
            }
            if compositor::parse_mode_from_args() == Mode::Hosted {
                tracing::error!(
                    path = %store.path().display(),
                    "hosted mode cannot create the first identity; start a native qOS session once to complete setup"
                );
                return Some(std::process::ExitCode::FAILURE);
            }
            tracing::info!(
                "no qOS identity found at {}; native compositor chrome will run {} setup",
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
            let paths = firstboot::collaboration_identity_paths();
            if let Err(error) = firstboot::ensure_collaboration_identity(identity_dir, &paths) {
                if provision_requested {
                    tracing::error!("could not provision collaboration identity: {error}");
                } else {
                    tracing::error!("qOS identity chain is incomplete or invalid: {error}");
                }
                return Some(std::process::ExitCode::FAILURE);
            }
            if provision_requested {
                tracing::info!("qOS collaboration identity is provisioned");
                return Some(std::process::ExitCode::SUCCESS);
            }
            tracing::info!("loaded qOS identity from {}", store.path().display());
        }
        Err(error) => {
            tracing::error!("could not initialise identity store: {error}");
            return Some(std::process::ExitCode::FAILURE);
        }
    }
    None
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
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX);
    if let Err(error) = firstboot::verify_collaboration_identity(
        identity_dir,
        &firstboot::collaboration_identity_paths(),
        now_ns,
    ) {
        eprintln!("identity verification failed: {error}");
        return std::process::ExitCode::FAILURE;
    }
    println!("qOS collaboration identity verified");
    std::process::ExitCode::SUCCESS
}

fn main() -> std::process::ExitCode {
    // `init()` is a no-op unless the env var is set, so production boots pay
    // zero cost; the scope guard dumps recorded events to disk when `main` returns.
    qos_startup_metrics::init();
    let _startup_dump = qos_startup_metrics::ScopedDumper;
    qos_startup_metrics::mark("main-start");

    // Inspect process shape while startup is still single-threaded. Host
    // packages pass an explicit installation context; deriving it from a
    // distro name, desktop variable or backend would conflate independent
    // facts and break on a different display manager.
    let raw_args: Vec<String> = std::env::args().collect();
    maybe_run_sandbox_exec(&raw_args);
    let arg_refs: Vec<&str> = raw_args
        .iter()
        .skip(1)
        .map(std::string::String::as_str)
        .collect();
    if compositor::parse_install_mode(&arg_refs) == compositor::InstallMode::HostDesktop {
        match identity_provisioning::configure_host_identity_root() {
            Ok(root) => eprintln!(
                "qOS host session: device identity will be stored under {}",
                root.display()
            ),
            Err(error) => {
                eprintln!("error: cannot resolve host-session identity storage: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    // Decide which GPU this session runs on before anything opens one — the
    // warmup thread below builds a wgpu context, and on a multi-GPU box its
    // adapter may disagree with what the compositor advertises to clients.
    // Pinning here, while `main` is still single-threaded, makes the
    // compositor and every launched app agree on one device.
    let gpu_pin = compositor::render_node::pin_process();

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
    // QOS_STARTUP_WARM=off disables FontSystem warmup; QOS_STARTUP_WARM_VELLO=1 opts back in.
    let want_vello = std::env::var("QOS_STARTUP_WARM_VELLO")
        .ok()
        .is_some_and(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "off" | "false" | "no"
            )
        });
    let _warmup_handle = ui::warmup::spawn_warmup(!want_vello);

    tracing_subscriber::fmt::init();
    install_crash_log_hook();
    log_gpu_pin(gpu_pin.as_ref());

    let portal_only = compositor::parse_portal_server_only_from_args();
    let firstboot_requested = compositor::parse_firstboot_from_args();
    let firstboot_only_requested = compositor::parse_firstboot_only_from_args();
    let provision_identity_requested = compositor::parse_provision_identity_from_args();
    let verify_identity_requested = compositor::parse_verify_collaboration_identity_from_args();

    if portal_only && (firstboot_requested || firstboot_only_requested) {
        eprintln!(
            "error: --portal-server-only and first-boot modes are mutually exclusive; \
             pick one (the systemd units never combine them)."
        );
        return std::process::ExitCode::from(2);
    }

    if firstboot_requested && firstboot_only_requested {
        eprintln!(
            "error: --firstboot and --firstboot-only are mutually exclusive; \
             --firstboot continues into the compositor while --firstboot-only exits."
        );
        std::process::exit(2);
    }

    if portal_only {
        return run_portal_server_only();
    }

    if firstboot_only_requested {
        if let Err(e) = firstboot::run() {
            tracing::error!("firstboot-only: setup failed: {e}");
            std::process::exit(1);
        }
        return std::process::ExitCode::SUCCESS;
    }

    if verify_identity_requested {
        return verify_collaboration_identity_entrypoint(provision_identity_requested);
    }

    if firstboot_requested && let Err(e) = firstboot::run() {
        // Don't fail the boot — log and continue. A non-zero exit still poisons
        // the wider boot transaction even though the systemd unit marks first-boot done regardless, so we swallow the error here.
        tracing::warn!("firstboot: setup hit an error ({e}); continuing with compositor boot");
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

    let enable_peer_sync = compositor::peer_sync_enabled_from_env_or_marker();
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
            qos_startup_metrics::mark("compositor-native-run-start");
            compositor::native::run();
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Run only the `xdg-desktop-portal` D-Bus backend — no compositor, no
/// rendering. Bootstraps a minimal tokio runtime, owns the session-bus
/// connection the `org.freedesktop.impl.portal.desktop.nirios` name
/// resolves to, and blocks until torn down (SIGTERM from systemd). Used
/// by `quick-desktop-portal.service` so the portal backend doesn't compete with the compositor for `wayland-N`.
fn run_portal_server_only() -> std::process::ExitCode {
    tracing::info!("nirios portal server starting (--portal-server-only)");

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nirios-portal-only")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build tokio runtime for portal-only mode: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    runtime.block_on(async move {
        // Fresh GrantStoreHandle on an empty WorkspaceTree + a Channel-mode
        // PromptQueueHandle — every prompt buffers (no compositor to drain it);
        // until per-workspace state is wired for this mode, the unit's job is just to own the bus name.
        let tree_handle = shell::shell::state::WorkspaceTree::default().shared();
        let grants = portal::GrantStoreHandle::new(tree_handle);
        let prompts = portal::PromptQueueHandle::new(portal::PromptResolver::Channel);
        let ctx = portal::PortalContext::new_with_null_spawners(
            grants,
            prompts,
            portal::CallerMap::new(),
        );

        let connection = match zbus::ConnectionBuilder::session() {
            Ok(builder) => match builder.build().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: portal-only mode could not open the session bus: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            },
            Err(e) => {
                eprintln!("error: portal-only mode could not build session connection: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        let _handle = match portal::serve(connection, ctx).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: portal::serve failed: {e}");
                return std::process::ExitCode::FAILURE;
            }
        };

        tracing::info!(
            "nirios portal server: owning {} on session bus; blocking until shutdown",
            portal::BUS_NAME
        );

        // Block forever — systemd sends SIGTERM to tear us down; the ServerHandle keeps the connection alive in the meantime.
        std::future::pending::<std::process::ExitCode>().await
    })
}
