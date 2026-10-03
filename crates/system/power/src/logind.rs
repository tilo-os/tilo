//! `systemd-logind` integration (`org.freedesktop.login1`) over `zbus`.
//!
//! Everything here performs actual D-Bus I/O against the system bus, so it only works where
//! `systemd-logind` is running. The pure decision logic for *whether* to call these lives in
//! [`super::policy`] / [`super::battery`]; this module performs the D-Bus operations.
//!
//! Three capabilities the compositor needs:
//!
//! * **Suspend** — [`LogindManager::suspend`] calls `Manager.Suspend`.
//! * **Inhibit** — [`LogindManager::inhibit`] takes a `Manager.Inhibit` lock and returns a
//!   [`LogindInhibitor`] whose `Drop` closes the fd and releases it. The compositor takes a
//!   *block* lock on `handle-lid-switch:handle-power-key` to delegate those events to its own
//!   policy, and a ref-counted `idle:sleep` lock mirroring the [`super::inhibit`] registry.
//! * **Sleep coordination** — [`LogindManager::receive_prepare_for_sleep`] surfaces
//!   `PrepareForSleep` so the compositor can lock before sleep and reset its idle timer on
//!   resume.

use zbus::Connection;
use zbus::zvariant::OwnedFd;

/// login1 well-known names.
const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const IFACE: &str = "org.freedesktop.login1.Manager";

/// A connected handle to `systemd-logind`'s `Manager`.
#[derive(Clone)]
pub struct LogindManager {
    proxy: zbus::Proxy<'static>,
}

impl LogindManager {
    /// Connect to the system bus and build the `Manager` proxy.
    ///
    /// # Errors
    ///
    /// Returns an error if the system bus is unreachable or the `Manager` proxy cannot be
    /// built.
    pub async fn connect() -> zbus::Result<Self> {
        let conn = Connection::system().await?;
        Self::with_connection(&conn).await
    }

    /// Build the proxy on an existing connection (lets the compositor share one system-bus
    /// connection across logind + `UPower`).
    ///
    /// # Errors
    ///
    /// Returns an error if the `Manager` proxy cannot be built on `conn`.
    pub async fn with_connection(conn: &Connection) -> zbus::Result<Self> {
        let proxy = zbus::Proxy::new(conn, DEST, PATH, IFACE).await?;
        Ok(Self { proxy })
    }

    /// The underlying connection (shared with [`super::upower`] when built via [`Self::with_connection`]).
    pub fn connection(&self) -> &Connection {
        self.proxy.connection()
    }

    /// `Manager.CanSuspend()` → `"yes"` / `"no"` / `"na"` / `"challenge"`. Used to decide whether
    /// a suspend request is even worth issuing.
    ///
    /// # Errors
    ///
    /// Returns an error if the `CanSuspend` call fails.
    pub async fn can_suspend(&self) -> zbus::Result<String> {
        self.proxy.call("CanSuspend", &()).await
    }

    /// `Manager.Suspend(interactive)` — actually suspend the machine. `interactive = false`
    /// means "do it without a polkit prompt" (the session user is authorized). The real,
    /// irreversible-looking call; the caller must have consulted policy and the inhibitor
    /// registry first.
    ///
    /// # Errors
    ///
    /// Returns an error if the `Suspend` call fails (e.g. refused by policykit, or logind is
    /// unreachable).
    pub async fn suspend(&self, interactive: bool) -> zbus::Result<()> {
        self.proxy.call("Suspend", &(interactive)).await
    }

    /// `Manager.Inhibit(what, who, why, mode)` → a lock fd.
    ///
    /// * `what` — colon-separated locks, e.g. `"idle:sleep"` or `"handle-lid-switch:handle-power-key"`.
    /// * `mode` — `"block"` (veto) or `"delay"` (up to `InhibitDelayMaxSec`, for cleanup before proceeding).
    ///
    /// The returned [`LogindInhibitor`] holds the fd open; drop it to release.
    ///
    /// # Errors
    ///
    /// Returns an error if the `Inhibit` call fails (e.g. an invalid `what`/`mode`, or logind
    /// is unreachable).
    pub async fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<LogindInhibitor> {
        let fd: OwnedFd = self.proxy.call("Inhibit", &(what, who, why, mode)).await?;
        Ok(LogindInhibitor {
            _fd: fd,
            what: what.to_string(),
        })
    }

    /// `Manager.ListInhibitors()` → `(what, who, why, mode, uid, pid)` rows. Used by tests to
    /// prove an [`Self::inhibit`] lock actually landed.
    ///
    /// # Errors
    ///
    /// Returns an error if the `ListInhibitors` call fails.
    #[allow(
        clippy::type_complexity,
        reason = "the tuple is logind's ListInhibitors D-Bus signature"
    )]
    pub async fn list_inhibitors(
        &self,
    ) -> zbus::Result<Vec<(String, String, String, String, u32, u32)>> {
        self.proxy.call("ListInhibitors", &()).await
    }

    /// Subscribe to `PrepareForSleep(bool)` — `true` just before the system sleeps, `false` just
    /// after it resumes.
    ///
    /// # Errors
    ///
    /// Returns an error if subscribing to the `PrepareForSleep` signal fails.
    pub async fn receive_prepare_for_sleep(
        &self,
    ) -> zbus::Result<zbus::proxy::SignalStream<'static>> {
        self.proxy.receive_signal("PrepareForSleep").await
    }

    /// Take the "delegate the lid switch and power key to us" *block* lock. With this held,
    /// logind stops acting on `HandleLidSwitch`/`HandlePowerKey` itself and leaves them to the
    /// compositor's policy. Keep the returned inhibitor alive for the session's lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying [`Self::inhibit`] call fails.
    pub async fn delegate_handled_keys(&self) -> zbus::Result<LogindInhibitor> {
        self.inhibit(
            "handle-lid-switch:handle-power-key:handle-suspend-key",
            "Tilo compositor",
            "compositor owns lid / power-key policy",
            "block",
        )
        .await
    }
}

/// An outstanding logind inhibitor lock. Dropping it closes the fd, which is how logind
/// learns the lock is released.
#[derive(Debug)]
pub struct LogindInhibitor {
    _fd: OwnedFd,
    what: String,
}

impl LogindInhibitor {
    /// The `what` string this lock was taken for.
    pub fn what(&self) -> &str {
        &self.what
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect to the real system bus, or print why we skipped and return `None`. An environment
    /// probe, not an `#[ignore]`: where logind is present the body runs and asserts for real;
    /// where it's genuinely absent the test cannot exercise D-Bus and says so.
    async fn manager_or_skip(ctx: &str) -> Option<LogindManager> {
        match LogindManager::connect().await {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("[skip] {ctx}: logind unreachable on system bus: {e}");
                None
            }
        }
    }

    #[tokio::test]
    async fn logind_answers_suspend_inhibit_and_key_delegation() {
        // Phase `can_suspend_returns_a_known_verdict`.
        async fn can_suspend_returns_a_known_verdict() {
            let Some(m) = manager_or_skip("can_suspend").await else {
                return;
            };
            let verdict = m.can_suspend().await.expect("CanSuspend call");
            assert!(
                matches!(verdict.as_str(), "yes" | "no" | "na" | "challenge"),
                "unexpected CanSuspend verdict: {verdict:?}"
            );
        }
        // Phase `inhibitor_lands_in_the_list_then_releases`.
        async fn inhibitor_lands_in_the_list_then_releases() {
            let Some(m) = manager_or_skip("inhibit").await else {
                return;
            };
            let why = format!("tilo-power test {}", std::process::id());
            let holder = m
                .inhibit("idle:sleep", "Tilo power test", &why, "block")
                .await
                .expect("Inhibit call");
            assert_eq!(holder.what(), "idle:sleep");

            // The lock is now visible to logind's own bookkeeping.
            let listed = m.list_inhibitors().await.expect("ListInhibitors");
            assert!(
                listed
                    .iter()
                    .any(|(_what, _who, w, _mode, _uid, _pid)| w == &why),
                "our inhibitor should be listed while held"
            );

            // Releasing the fd removes it.
            drop(holder);
            // logind updates asynchronously; retry briefly.
            let mut gone = false;
            for _ in 0..20 {
                let listed = m.list_inhibitors().await.expect("ListInhibitors");
                if !listed.iter().any(|(_w, _who, w, _m, _u, _p)| w == &why) {
                    gone = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(gone, "inhibitor should disappear after the fd is dropped");
        }
        // Phase `delegate_handled_keys_takes_a_block_lock`.
        async fn delegate_handled_keys_takes_a_block_lock() {
            let Some(m) = manager_or_skip("delegate").await else {
                return;
            };
            let holder = m.delegate_handled_keys().await.expect("delegate inhibit");
            let listed = m.list_inhibitors().await.expect("ListInhibitors");
            // A block lock on the handled keys, owned by us, must be present.
            assert!(
                listed.iter().any(|(what, _who, _why, mode, _uid, _pid)| {
                    what.contains("handle-lid-switch") && mode == "block"
                }),
                "the delegate lock should be a block inhibitor on the lid switch"
            );
            drop(holder);
        }
        // Phase `can_suspend_returns_a_known_verdict` (defined above).
        can_suspend_returns_a_known_verdict().await;
        // Phase `inhibitor_lands_in_the_list_then_releases` (defined above).
        inhibitor_lands_in_the_list_then_releases().await;
        // Phase `delegate_handled_keys_takes_a_block_lock` (defined above).
        delegate_handled_keys_takes_a_block_lock().await;
    }
}
