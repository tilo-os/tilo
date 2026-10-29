//! The D-Bus services the compositor serves for its session: notifications,
//! the tray watcher and the agents whose prompts only the compositor draws
//! (polkit, `NetworkManager` secrets, Bluetooth pairing), plus the host power
//! and time-zone services a seat-owning session runs beside them.

use std::sync::Arc;

use crate::session_runtime::Registration;

/// Every bus registration the session holds. Dropping it releases the bus
/// names and tears down the object servers, in field order; `run` drops it
/// in its ordered shutdown.
pub(super) struct SessionBusServices {
    /// The session runtime keeps driving zbus dispatch in the background for
    /// the life of the process.
    runtime: Option<&'static tokio::runtime::Runtime>,
    /// Registered in the background: the loop starts without waiting on the
    /// bus daemon. Until then (or if it fails) shell-internal notifications
    /// keep working.
    _notifications: Option<Registration<crate::notifications::ServerHandle>>,
    _tray: Option<Registration<tilo_tray_host::watcher::WatcherHandle>>,
    _polkit_agent: Option<Registration<tilo_polkit_agent::service::AgentHandle>>,
    _network_secret_agent: Option<Registration<tilo_network_secret_agent::service::AgentHandle>>,
    _bluetooth_agent: Option<Registration<crate::bluetooth::PairingService>>,
}

impl SessionBusServices {
    /// The `org.freedesktop.Notifications` D-Bus service — the compositor owns
    /// it so sandboxed apps' toasts render as OS-drawn chrome. The bridge is the
    /// `Send + Sync` mailbox between the async zbus service (its own tokio
    /// runtime) and this loop's sync chrome pump. Failure to reach the session
    /// bus downgrades to a warning; shell-internal notifications keep working
    /// regardless. The agents and the tray watcher share its runtime.
    pub(super) fn start(
        notif_bridge: &crate::notifications::NotificationBridge,
        portal_backend: Option<&super::PortalBackend>,
        priority_contacts: crate::priority_contacts::PriorityContactRegistry,
        session_wake: &crate::session_runtime::Wake,
    ) -> Self {
        let mut runtime: Option<&'static tokio::runtime::Runtime> = None;
        let mut notifications = None;
        match crate::session_runtime::runtime() {
            Ok(rt) => {
                let bridge = notif_bridge.clone();
                let notification_workspaces =
                    portal_backend.map(|portal| portal.media_grants.clone());
                notifications = Some(Registration::start(
                    rt.handle(),
                    "notifications",
                    async move {
                        let conn = zbus::connection::Builder::session()?.build().await?;
                        let service = crate::notifications::NotificationService::with_connection(
                            bridge.clone(),
                            // v1: a fresh CallerMap. Per-workspace attribution comes once the bridge
                            // accept path populates it (same shape the portal's CallerMap uses); until then unsandboxed callers carry none.
                            crate::portal::CallerMap::new(),
                            conn.clone(),
                            priority_contacts,
                            notification_workspaces,
                        );
                        let handle =
                            crate::notifications::serve_notifications(conn.clone(), service)
                                .await?;
                        crate::notifications::spawn_signal_emitter(conn, bridge);
                        Ok::<_, zbus::Error>(handle)
                    },
                    |_| {
                        tracing::info!(
                            "notifications: serving {} on session bus",
                            crate::notifications::BUS_NAME
                        );
                    },
                ));
                // The power service runs regardless of notification outcome: it
                // lives on the *system* bus (logind/UPower), independent of the
                // session-bus daemon, sharing these threads.
                runtime = Some(rt);
            }
            Err(e) => {
                tracing::warn!(
                    "notifications: session runtime unavailable ({e}); D-Bus service off"
                );
            }
        }

        // Status-notifier watcher: applications' tray icons for the top bar and
        // the action registry. Held for the session; a second watcher already on
        // the bus (a nested session) leaves this one off.
        let tray = runtime.map(|rt| {
            Registration::start(
                rt.handle(),
                "tray",
                async move {
                    tilo_tray_host::watcher::serve(
                        zbus::connection::Builder::session()?,
                        std::sync::Arc::clone(crate::tray::global()),
                    )
                    .await
                },
                |_| {
                    tracing::info!(
                        "tray: serving {} on session bus",
                        tilo_tray_host::WATCHER_BUS_NAME
                    );
                },
            )
        });

        // polkit authentication agent: administrator prompts from pkexec,
        // GNOME Disks, virt-manager and system Flatpak installs become a sheet
        // only the compositor draws. Registered on the system bus for this login
        // session; without polkit or logind the requests fail as before.
        let polkit_agent =
            runtime.map(|rt| {
                Registration::start(
                    rt.handle(),
                    "polkit agent",
                    crate::polkit_agent::serve(
                        crate::shell::portal_dialog::PortalDialogQueue::global(),
                    ),
                    |_| tracing::info!("polkit: authentication agent registered for this session"),
                )
            });

        // NetworkManager secret agent: a Wi-Fi password that changed, 802.1X
        // sign-in, VPN secrets and SIM PINs become a sheet only the compositor
        // draws, instead of a connection that silently fails.
        let network_secret_agent = runtime.map(|rt| {
            Registration::start(
                rt.handle(),
                "network secret agent",
                crate::network_secrets::serve(
                    crate::shell::portal_dialog::PortalDialogQueue::global(),
                ),
                |_| tracing::info!("network: secret agent registered with NetworkManager"),
            )
        });

        // Bluetooth pairing agent: BlueZ's default agent for this session, so a
        // passkey, PIN, confirmation or service authorisation from any pairing
        // becomes a dialog only the compositor draws, instead of a pairing that
        // silently fails.
        let pairing_prompts = crate::bluetooth::PairingPromptHandle::global();
        pairing_prompts.set_wake(Arc::clone(session_wake));
        let bluetooth_agent = runtime.map(|rt| {
            Registration::start(
                rt.handle(),
                "bluetooth pairing agent",
                crate::bluetooth::PairingService::start(pairing_prompts),
                |_| tracing::info!("bluetooth: pairing agent registered with BlueZ"),
            )
        });

        Self {
            runtime,
            _notifications: notifications,
            _tray: tray,
            _polkit_agent: polkit_agent,
            _network_secret_agent: network_secret_agent,
            _bluetooth_agent: bluetooth_agent,
        }
    }

    /// Power service: async orchestrator suspending via logind, mirroring the
    /// inhibitor registry into a logind lock, watching the battery and lid.
    /// Spawned on the session runtime; if the system bus/logind/UPower are
    /// unreachable it logs and falls back to the pure idle policy (still
    /// blanks/locks/honours env timeouts, just can't actuate real suspend).
    pub(super) fn start_host_power(
        &self,
        power_bridge: &tilo_power_mgmt::PowerBridge,
        inhibit_registry: &tilo_power_mgmt::InhibitRegistry,
        policy: tilo_power_mgmt::PowerPolicy,
    ) {
        let Some(rt) = self.runtime else {
            return;
        };
        let bridge = power_bridge.clone();
        let registry = inhibit_registry.clone();
        rt.spawn(async move {
            match tilo_power_mgmt::PowerService::connect(bridge, registry, policy).await {
                Ok(service) => service.run().await,
                Err(e) => tracing::warn!(
                    target: "tilo::power",
                    "power service unavailable ({e}); suspend/lid/battery inactive"
                ),
            }
        });
    }

    /// The time zone follows the device where the person chose so in
    /// Settings ("Set automatically"): only the session that owns the
    /// machine sets its zone.
    pub(super) fn follow_device_time_zone(&self) {
        if let Some(rt) = self.runtime {
            rt.spawn(crate::time_zone_follower::serve(std::sync::Arc::new(
                crate::time_zone_follower::ThisDevice,
            )));
        }
    }
}
