//! Whether the device is online, and whether its connection is metered.
//!
//! Neither target can promise that a particular server is reachable, so
//! [`Connectivity::Online`] means "worth trying", and a failed request remains
//! the real answer. Unknown metering is never permission for background work.

use std::sync::OnceLock;

use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Connectivity {
    Online,
    Offline,
    /// The platform cannot tell.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct NetStatus {
    pub connectivity: Connectivity,
    /// Whether the user pays per byte or asked to save data; `None` where the
    /// platform does not say ([`Feature::Metered`](crate::Feature::Metered)).
    pub metered: Option<bool>,
}

impl NetStatus {
    #[must_use]
    pub fn online(self) -> bool {
        self.connectivity != Connectivity::Offline
    }
}

/// The latest status; it never waits for D-Bus.
#[must_use]
pub fn current() -> NetStatus {
    (*host().borrow()).unwrap_or_else(imp::current)
}

/// The hints a host sends this process over its own connection, on every target: the window's
/// shell-ui link on Tilo, and nothing on the web. A reading the host sent is the answer; the
/// platform's own source is only read while it sent none.
fn host() -> &'static watch::Sender<Option<NetStatus>> {
    static HOST: OnceLock<watch::Sender<Option<NetStatus>>> = OnceLock::new();
    HOST.get_or_init(|| watch::channel(None).0)
}

/// Host integration: an absent reading is unknown, not unmetered. This does
/// not grant network access; clear it when the host transport disconnects.
#[doc(hidden)]
pub fn set_host_status(available: Option<bool>, metered: Option<bool>) {
    host().send_replace(Some(NetStatus {
        connectivity: match available {
            Some(true) => Connectivity::Online,
            Some(false) => Connectivity::Offline,
            None => Connectivity::Unknown,
        },
        metered,
    }));
}

/// Forget a disconnected host and resume portable status hints.
#[doc(hidden)]
pub fn clear_host_status() {
    host().send_replace(None);
}

#[must_use]
pub fn online() -> bool {
    current().online()
}

pub struct Watch {
    last: NetStatus,
    inner: imp::Watch,
    host: watch::Receiver<Option<NetStatus>>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub fn watch() -> Watch {
    Watch {
        last: current(),
        inner: imp::Watch::new(),
        host: host().subscribe(),
    }
}

impl Watch {
    #[must_use]
    pub fn last(&self) -> NetStatus {
        self.last
    }

    pub async fn changed(&mut self) -> NetStatus {
        loop {
            let now = current();
            if now != self.last {
                self.last = now;
                return now;
            }
            tokio::select! {
                changed = self.host.changed() => {
                    if changed.is_err() {
                        self.inner.wait().await;
                    }
                }
                () = self.inner.wait() => {}
            }
        }
    }
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::io::ErrorKind;
    use std::net::{SocketAddr, UdpSocket};
    use std::time::Duration;

    use super::{Connectivity, NetStatus};

    /// How often the desktop re-checks its routes.
    pub(super) const POLL: Duration = Duration::from_secs(5);

    // Connecting a UDP socket asks the kernel for a route and sends nothing;
    // these are documentation addresses, never contacted.
    const PROBES: [&str; 2] = ["198.51.100.1:9", "[2001:db8::1]:9"];

    fn route(probe: &str) -> Option<bool> {
        let target: SocketAddr = probe.parse().ok()?;
        let local: SocketAddr = if target.is_ipv4() {
            "0.0.0.0:0".parse().ok()?
        } else {
            "[::]:0".parse().ok()?
        };
        let socket = UdpSocket::bind(local).ok()?;
        match socket.connect(target) {
            Ok(()) => Some(true),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable
                ) =>
            {
                Some(false)
            }
            Err(_) => None,
        }
    }

    pub(super) fn fallback() -> NetStatus {
        let answers = PROBES.map(route);
        let connectivity = if answers.contains(&Some(true)) {
            Connectivity::Online
        } else if answers.iter().all(|answer| *answer == Some(false)) {
            Connectivity::Offline
        } else {
            Connectivity::Unknown
        };
        NetStatus {
            connectivity,
            metered: None,
        }
    }

    #[cfg(all(target_os = "linux", feature = "system-status"))]
    pub(super) use super::linux::{Watch, current};

    #[cfg(any(
        not(target_os = "linux"),
        all(target_os = "linux", not(feature = "system-status"))
    ))]
    pub(super) fn current() -> NetStatus {
        fallback()
    }

    #[cfg(any(
        not(target_os = "linux"),
        all(target_os = "linux", not(feature = "system-status"))
    ))]
    pub(super) struct Watch;

    #[cfg(any(
        not(target_os = "linux"),
        all(target_os = "linux", not(feature = "system-status"))
    ))]
    impl Watch {
        pub(super) fn new() -> Self {
            Self
        }

        #[allow(
            clippy::needless_pass_by_ref_mut,
            reason = "the same signature as the page backend"
        )]
        pub(super) async fn wait(&mut self) {
            tokio::time::sleep(POLL).await;
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod imp {
    use super::{Connectivity, NetStatus};

    pub(super) fn current() -> NetStatus {
        let navigator = crate::web::global_get("navigator");
        let connectivity = match navigator
            .as_ref()
            .and_then(|navigator| crate::web::get(navigator, "onLine"))
            .and_then(|online| online.as_bool())
        {
            Some(true) => Connectivity::Online,
            Some(false) => Connectivity::Offline,
            None => Connectivity::Unknown,
        };
        let connection = navigator
            .as_ref()
            .and_then(|navigator| crate::web::get(navigator, "connection"));
        let metered = connection.as_ref().map(|connection| {
            let save_data = crate::web::get(connection, "saveData")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let cellular = crate::web::get(connection, "type")
                .and_then(|value| value.as_string())
                .is_some_and(|kind| kind == "cellular");
            save_data || cellular
        });
        NetStatus {
            connectivity,
            metered,
        }
    }

    pub(super) struct Watch(crate::web::Events);

    impl Watch {
        pub(super) fn new() -> Self {
            let mut events = crate::web::Events::new();
            if let Some(global) = crate::web::global() {
                events.listen(&global, &["online", "offline"]);
            }
            if let Some(connection) = crate::web::global_get("navigator")
                .and_then(|navigator| crate::web::get(&navigator, "connection"))
            {
                events.listen(&connection, &["change"]);
            }
            Self(events)
        }

        pub(super) async fn wait(&mut self) {
            self.0.next().await;
        }
    }
}

#[cfg(test)]
#[cfg(not(target_family = "wasm"))]
mod tests {
    use super::*;

    #[test]
    fn the_route_probe_answers_without_sending() {
        // CI machines may be offline; the answer only has to be consistent.
        let status = imp::fallback();
        assert_eq!(
            status.metered, None,
            "an absent reading is unknown, never unmetered"
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(watch().last().connectivity, status.connectivity);
    }

    /// An app process's build: the hints its window's host sends are the answer, and losing the
    /// host falls back to the local probe rather than keeping a stale reading.
    #[cfg(not(all(target_os = "linux", feature = "system-status")))]
    #[test]
    fn a_hosts_hint_is_the_answer_and_its_loss_falls_back() {
        set_host_status(Some(false), Some(true));
        assert_eq!(current().connectivity, Connectivity::Offline);
        assert_eq!(current().metered, Some(true));
        clear_host_status();
        assert_eq!(
            current().metered,
            None,
            "the local probe never claims metering"
        );
    }
}

#[cfg(all(target_os = "linux", feature = "system-status"))]
#[path = "status_linux.rs"]
mod linux;
