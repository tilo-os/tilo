//! `systemd-logind` (`org.freedesktop.login1`) over `zbus`: the D-Bus calls
//! themselves, with the decisions about when to make them in [`super::policy`]
//! and [`super::battery`].

use zbus::Connection;
use zbus::zvariant::OwnedFd;

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const IFACE: &str = "org.freedesktop.login1.Manager";
/// The calling process's own session.
const SESSION_AUTO: &str = "/org/freedesktop/login1/session/auto";

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

    pub async fn with_connection(conn: &Connection) -> zbus::Result<Self> {
        let proxy = zbus::Proxy::new(conn, DEST, PATH, IFACE).await?;
        Ok(Self { proxy })
    }

    pub fn connection(&self) -> &Connection {
        self.proxy.connection()
    }

    pub async fn can_suspend(&self) -> zbus::Result<String> {
        self.proxy.call("CanSuspend", &()).await
    }

    pub async fn suspend(&self, interactive: bool) -> zbus::Result<()> {
        self.proxy.call("Suspend", &(interactive)).await
    }

    pub async fn reboot(&self, interactive: bool) -> zbus::Result<()> {
        self.proxy.call("Reboot", &(interactive)).await
    }

    pub async fn power_off(&self, interactive: bool) -> zbus::Result<()> {
        self.proxy.call("PowerOff", &(interactive)).await
    }

    pub async fn lock_session(&self) -> zbus::Result<()> {
        self.own_session("Lock", &()).await
    }

    /// `Session.SetBrightness(subsystem, name, brightness)` on the caller's own session:
    /// logind lets the seat's active session set a backlight it may not write itself.
    pub async fn set_brightness(
        &self,
        subsystem: &str,
        name: &str,
        value: u32,
    ) -> zbus::Result<()> {
        self.own_session("SetBrightness", &(subsystem, name, value))
            .await
    }

    async fn own_session<B>(&self, method: &str, body: &B) -> zbus::Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
    {
        self.connection()
            .call_method(
                Some(DEST),
                SESSION_AUTO,
                Some("org.freedesktop.login1.Session"),
                method,
                body,
            )
            .await?;
        Ok(())
    }

    /// `Manager.Inhibit(what, who, why, mode)` → a lock fd. `what` is a
    /// colon-separated list of locks (`"idle:sleep"`, `"handle-lid-switch"`);
    /// `mode` is `"block"` (veto) or `"delay"` (up to `InhibitDelayMaxSec`, for
    /// cleanup before proceeding). The returned [`LogindInhibitor`] holds the fd
    /// open; drop it to release.
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

    /// `Manager.ListInhibitors()` → `(what, who, why, mode, uid, pid)` rows, to
    /// prove an [`Self::inhibit`] lock actually landed.
    pub async fn list_inhibitors(
        &self,
    ) -> zbus::Result<Vec<(String, String, String, String, u32, u32)>> {
        self.proxy.call("ListInhibitors", &()).await
    }

    /// Subscribe to `PrepareForSleep(bool)`: `true` just before the system sleeps,
    /// `false` just after it resumes.
    pub async fn receive_prepare_for_sleep(
        &self,
    ) -> zbus::Result<zbus::proxy::SignalStream<'static>> {
        self.proxy.receive_signal("PrepareForSleep").await
    }

    /// Take the block lock that delegates the lid switch and the power key to us:
    /// with it held logind stops acting on `HandleLidSwitch` and `HandlePowerKey`
    /// itself. Keep the returned inhibitor alive for the session's lifetime.
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

/// logind learns the lock is released.
#[derive(Debug)]
pub struct LogindInhibitor {
    _fd: OwnedFd,
    what: String,
}

impl LogindInhibitor {
    pub fn what(&self) -> &str {
        &self.what
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect to the real system bus, or print why we skipped and return `None`:
    /// where logind is present the body runs and asserts for real, and where it is
    /// absent the test says so instead of passing silently.
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

            drop(holder);
            // logind updates asynchronously.
            tilo_test_env::within("the inhibitor to leave logind's list", async {
                loop {
                    let listed = m.list_inhibitors().await.expect("ListInhibitors");
                    if !listed.iter().any(|(_w, _who, w, _m, _u, _p)| w == &why) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
            .await;
        }
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
        can_suspend_returns_a_known_verdict().await;
        inhibitor_lands_in_the_list_then_releases().await;
        delegate_handled_keys_takes_a_block_lock().await;
    }
}
