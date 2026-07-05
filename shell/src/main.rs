//! Compositor binary entry point.
//!
//! Phase 1 of `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! split the launch surface in two: pick a [`Mode`] from `--mode`, then
//! delegate to the matching `compositor::*::run` function.
//!
//! As of Phase 7, [`Mode::Native`] is the default — no flag at all spins up
//! the real smithay server (auto-binding `wayland-N`) with the vello+wgpu
//! winit backend from Phase 4. [`Mode::Hosted`] remains as an explicit dev
//! opt-in (`--mode hosted`) for the legacy iced-on-winit path when DRM or a
//! spare Wayland session is unavailable.
//!
//! Two side-channel modes short-circuit the normal dispatch before
//! `parse_mode` runs:
//!
//! - `--portal-server-only` runs only the `xdg-desktop-portal` D-Bus
//!   backend (used by the `quick-desktop-portal.service` user unit so
//!   the portal doesn't race the compositor for `wayland-N`).
//! - `--firstboot` runs first-boot setup (creating user config dirs)
//!   and *then* continues with the normal compositor boot — invoked
//!   by `quick-desktop-firstboot.service` on first login.
//!
//! The two are mutually exclusive: passing both is a usage error.

use shell::compositor::hosted::HostedOptions;
use shell::compositor::{self, Mode};
use shell::shell::identity_provisioning;
use shell::{firstboot, portal};

/// Install a panic hook that writes a clearly-formatted crash report to
/// `~/.cache/qos/crashes/<timestamp>.log` AND echoes the panic message
/// + the crash-log path to stderr. Without this, panics on the live
/// shell write to stderr only — and a session launched outside a
/// terminal (greetd, .desktop file double-click) discards stderr,
/// making "the shell just disappeared" reports unactionable.
///
/// Chains to the default hook AFTER writing the file so a developer
/// running from a terminal still sees the colored panic + backtrace
/// in stderr.
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
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
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
    // Startup timing capture. `init()` is a no-op unless the env var
    // is set, so production boots pay zero cost. The scope guard
    // dumps the recorded events to disk when `main` returns.
    qos_startup_metrics::init();
    let _startup_dump = qos_startup_metrics::ScopedDumper;
    qos_startup_metrics::mark("main-start");

    // Kick off cold-start warmup on a background thread as early as
    // possible. Two things dominate the compositor's time-to-first-paint
    // on a cold process: cosmic-text's FontSystem::new (mmaps every
    // installed face, up to ~1s on release builds) and vello's
    // Renderer::new (compiles ~30 WGSL pipelines). The warmup shapes
    // canonical strings + paints a scratch 4×4 offscreen scene so the
    // OS page cache, fontconfig cache, and the process-wide wgpu
    // RenderContext are all hot by the time the compositor thread hits
    // its real FontSystem/Renderer builds. Disabled with
    // QOS_STARTUP_WARM=off (used for A/B measurement). The handle is
    // kept alive on the main-fn stack so the thread survives to
    // completion regardless of what `main` is currently blocked on.
    // FontSystem warmup is enabled by default and measures a clean win
    // on cold-start (~90 ms / 22% of first-paint against the shots harness).
    // The Vello scratch-render warmup is off by default — it contends
    // for the process-wide `SHARED_RENDER_CONTEXT` mutex with the
    // compositor's own paint, and measured slower than no warmup at all.
    // Opt back in with QOS_STARTUP_WARM_VELLO=1 when a separate-device
    // strategy lands.
    let want_vello = std::env::var("QOS_STARTUP_WARM_VELLO")
        .ok()
        .is_some_and(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "off" | "false" | "no"));
    let _warmup_handle = ui::warmup::spawn_warmup(!want_vello);

    // Landlock launcher shim (`plan/security.md` §7.1). bwrap execs us as
    // `shell __sandbox-exec --ro <p>… --rw <p>… [--net-restricted] -- <app
    // argv>` from inside the per-workspace sandbox; we apply an in-kernel
    // Landlock ruleset to ourselves (and thus to the app we then exec) and
    // `execvp` the real app. This must short-circuit before *any* normal
    // compositor setup — we are not the compositor here, just a one-shot
    // restriction step. `run_sandbox_exec` diverges (execvp or exit).
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

    let portal_only = compositor::parse_portal_server_only_from_args();
    let firstboot_requested = compositor::parse_firstboot_from_args();

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

    if firstboot_requested {
        if let Err(e) = firstboot::run() {
            // Don't fail the boot — log and continue. The systemd unit
            // marks first-boot done via `ExecStartPost=touch …` even if
            // we return non-zero, but a non-zero exit still poisons the
            // wider boot transaction, so we swallow the error here.
            tracing::warn!("firstboot: setup hit an error ({e}); continuing with compositor boot");
        }
        // Fall through to the normal compositor entry point — the
        // systemd unit runs us as `Type=oneshot`, so the compositor
        // process exits at the bottom of `app.run()` as usual.
    }

    // Phase 8 identity gate. The full UI ceremony lives in
    // `ui::widget::identity_flow::IdentityFlow`; the disk-backed
    // persistence in `shell::shell::identity_provisioning`. v1
    // surfaces the check here so a fresh-machine boot logs a clear
    // line (and the integration follow-up can swap the log for a
    // pre-desktop winit loop that hosts IdentityFlow).
    //
    // TODO Phase 8 integration: replace this log with a blocking
    // winit loop that renders IdentityFlow until is_done() returns
    // true; persist + then fall through to the desktop boot path
    // below. The wiring requires composing the compositor's existing
    // `App` setup with an IdentityFlow-only `App` and is documented
    // in the Phase 8 follow-up.
    match identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) if identity_provisioning::needs_first_run_flow(&store) => {
            tracing::warn!(
                "no qOS identity found at {}; the desktop will boot without \
                 a signing identity. Run the identity_flow_demo example to \
                 walk the ceremony manually.",
                store.path().display()
            );
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
        // `--mode native` doesn't take the portal flag yet — Sandbox
        // Phase 4.3 only wires the hosted-mode boot path. Surface a
        // warning so users who pass `--enable-portals --mode native`
        // don't silently get nothing.
        Mode::Native => {
            if enable_portals {
                tracing::warn!(
                    "--enable-portals is hosted-mode only in v1 of Sandbox Phase 4.3; \
                     native mode currently ignores it"
                );
            }
            qos_startup_metrics::mark("compositor-native-run-start");
            compositor::native::run();
        }
    }
}

/// Run only the `xdg-desktop-portal` D-Bus backend — no compositor, no
/// rendering. Bootstraps a minimal tokio runtime, owns the session-bus
/// connection that the `org.freedesktop.impl.portal.desktop.nirios`
/// name resolves to, and blocks until the runtime is torn down (process
/// kill / SIGTERM from systemd).
///
/// Used by the user-level `quick-desktop-portal.service` unit so the
/// portal backend doesn't compete with the compositor for `wayland-N`
/// and can be `Type=dbus`-activated independently.
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
        // v1: stand up a fresh GrantStoreHandle on an empty
        // WorkspaceTree + a Channel-mode PromptQueueHandle. The portal
        // serves requests but every prompt is buffered (no compositor
        // to drain it) — production wires per-workspace tree state
        // when the bridge accept path lands in the portal-only mode.
        // Until then the unit's job is to *own the bus name* so the
        // session has a portal backend at all.
        // C-3: the portal-only entry point doesn't share with chrome /
        // peer-sync (no compositor is live in `--portal-only` mode), but
        // the handle shape still has to match the production API. Build
        // a fresh handle around a default tree.
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

        // Block forever — systemd sends SIGTERM to tear us down. The
        // ServerHandle keeps the connection alive in the meantime.
        std::future::pending::<()>().await;
    });
}
