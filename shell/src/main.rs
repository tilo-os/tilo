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
use shell::{firstboot, portal};

fn main() {
    tracing_subscriber::fmt::init();

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
