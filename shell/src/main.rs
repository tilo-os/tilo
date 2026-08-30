//! Compositor binary entry point.
//!
//! Picks a [`Mode`] from `--mode`, then delegates to the matching
//! `compositor::*::run` function. [`Mode::Native`] is the default (real
//! smithay server, vello+wgpu winit backend); [`Mode::Hosted`] is an
//! explicit dev opt-in (`--mode hosted`) for the legacy iced-on-winit path.
//!
//! Two side-channel modes short-circuit dispatch before `parse_mode`
//! runs, mutually exclusive: `--portal-server-only` runs only the
//! `xdg-desktop-portal` D-Bus backend (so it doesn't race the compositor
//! for `wayland-N`); `--firstboot` runs first-boot setup, then continues into the normal compositor boot.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
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

fn main() {
    // `init()` is a no-op unless the env var is set, so production boots pay
    // zero cost; the scope guard dumps recorded events to disk when `main` returns.
    qos_startup_metrics::init();
    let _startup_dump = qos_startup_metrics::ScopedDumper;
    qos_startup_metrics::mark("main-start");

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

    // Landlock launcher shim: bwrap execs us as `shell __sandbox-exec --ro
    // <p>… --rw <p>… [--net-restricted] -- <app argv>` from inside the
    // sandbox; we apply an in-kernel Landlock ruleset and `execvp` the real app. Must short-circuit before any compositor setup.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args
        .get(1)
        .is_some_and(|a| a == shell::sandbox::SANDBOX_EXEC_SUBCOMMAND)
    {
        tracing_subscriber::fmt::init();
        shell::sandbox::run_sandbox_exec(&raw_args[2..]);
    }

    tracing_subscriber::fmt::init();
    install_crash_log_hook();

    // Replay the GPU decision taken before the subscriber existed.
    if let Some(pin) = &gpu_pin {
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

    let portal_only = compositor::parse_portal_server_only_from_args();
    let firstboot_requested = compositor::parse_firstboot_from_args();
    let provision_identity_requested = compositor::parse_provision_identity_from_args();

    if portal_only && firstboot_requested {
        eprintln!(
            "error: --portal-server-only and --firstboot are mutually exclusive; \
             pick one (the systemd units never combine them)."
        );
        std::process::exit(2);
    }

    if portal_only {
        run_portal_server_only();
        return;
    }

    if firstboot_requested && let Err(e) = firstboot::run() {
        // Don't fail the boot — log and continue. A non-zero exit still poisons
        // the wider boot transaction even though the systemd unit marks first-boot done regardless, so we swallow the error here.
        tracing::warn!("firstboot: setup hit an error ({e}); continuing with compositor boot");
    }
    // Fall through to the normal compositor entry point — the systemd unit
    // runs us as `Type=oneshot`, so the process exits at the bottom of `app.run()` as usual.

    // A signing identity belongs to the person, so it's created here — in
    // their session, never by the system unit running as root before anyone
    // has logged in. `--provision-identity` is `qos-identity.service`'s
    // **user**-unit entry point: it runs the ceremony and exits. Reaching
    // this point without an identity and without that flag means something started the desktop directly.
    match identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) if identity_provisioning::needs_first_run_flow(&store) => {
            if provision_identity_requested {
                run_identity_onboarding(store.path().parent().map(std::path::Path::to_path_buf));
                return;
            }
            tracing::warn!(
                "no qOS identity found at {}; the desktop will boot without a signing \
                 identity. Nothing you do will be attributable to you until \
                 qos-identity.service has run in this session.",
                store.path().display()
            );
        }
        Ok(store) if provision_identity_requested => {
            // Migrate older installs which have the personal signing record
            // but not yet the certified device/Iroh identity chain.
            let Some(identity_dir) = store.path().parent() else {
                tracing::error!("identity record has no parent directory");
                std::process::exit(1);
            };
            let paths = firstboot::collaboration_identity_paths();
            if let Err(e) = firstboot::ensure_collaboration_identity(identity_dir, &paths) {
                tracing::error!("could not provision collaboration identity: {e}");
                std::process::exit(1);
            }
            tracing::info!("qOS collaboration identity is provisioned");
            return;
        }
        Ok(store) => {
            tracing::info!("loaded qOS identity from {}", store.path().display());
        }
        Err(e) => {
            tracing::warn!("could not initialise identity store: {e}");
        }
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
}

/// Host the first-login identity ceremony and block until finished. Runs
/// as its own `ui::App` rather than inside the compositor, since this happens *before* a desktop exists.
fn run_identity_onboarding(identity_dir: Option<std::path::PathBuf>) {
    let Some(dir) = identity_dir else {
        // No resolvable home means no right place for a signing key, and guessing one is how the key ended up in root's tree to begin with.
        tracing::error!(
            "cannot provision an identity: no user config directory resolved from \
             $XDG_CONFIG_HOME or $HOME"
        );
        std::process::exit(1);
    };
    tracing::info!(
        "running first-login identity onboarding into {}",
        dir.display()
    );
    let state = ui::State::new();
    let view = shell::onboarding_view::OnboardingView::new(dir);
    let app = ui::App::new(state, Box::new(view));
    app.run();
}

/// Run only the `xdg-desktop-portal` D-Bus backend — no compositor, no
/// rendering. Bootstraps a minimal tokio runtime, owns the session-bus
/// connection the `org.freedesktop.impl.portal.desktop.nirios` name
/// resolves to, and blocks until torn down (SIGTERM from systemd). Used
/// by `quick-desktop-portal.service` so the portal backend doesn't compete with the compositor for `wayland-N`.
fn run_portal_server_only() {
    tracing::info!("nirios portal server starting (--portal-server-only)");

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nirios-portal-only")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build tokio runtime for portal-only mode: {e}");
            std::process::exit(1);
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
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("error: portal-only mode could not build session connection: {e}");
                std::process::exit(1);
            }
        };

        let _handle = match portal::serve(connection, ctx).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: portal::serve failed: {e}");
                std::process::exit(1);
            }
        };

        tracing::info!(
            "nirios portal server: owning {} on session bus; blocking until shutdown",
            portal::BUS_NAME
        );

        // Block forever — systemd sends SIGTERM to tear us down; the ServerHandle keeps the connection alive in the meantime.
        std::future::pending::<()>().await;
    });
}
