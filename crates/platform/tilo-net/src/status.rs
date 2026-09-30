//! Whether the device is online, and whether its connection is metered.
//!
//! Neither target can promise that a particular server is reachable, so
//! [`Connectivity::Online`] means "worth trying", and a failed request
//! remains the real answer. On the desktop it means a route to the public
//! internet exists (IPv4 or IPv6); in a page it is `navigator.onLine`.
//! Metering comes from `NetworkManager` on Linux and the Network Information
//! API in a page; macOS does not say.

/// Whether the device has a network path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Connectivity {
    Online,
    Offline,
    /// The platform cannot tell.
    Unknown,
}

/// The device's network status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct NetStatus {
    pub connectivity: Connectivity,
    /// Whether the user pays per byte or asked to save data; `None` where
    /// the platform does not say ([`Feature::Metered`](crate::Feature::Metered)).
    pub metered: Option<bool>,
}

impl NetStatus {
    /// Whether requests are worth trying: online, or unknown.
    #[must_use]
    pub fn online(self) -> bool {
        self.connectivity != Connectivity::Offline
    }
}

/// The status now.
#[must_use]
pub fn current() -> NetStatus {
    imp::current()
}

/// Whether requests are worth trying now; see [`NetStatus::online`].
#[must_use]
pub fn online() -> bool {
    current().online()
}

/// Changes to the status.
pub struct Watch {
    last: NetStatus,
    inner: imp::Watch,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

/// Follow the status. The desktop re-checks every few seconds; a page wakes
/// on the browser's `online`, `offline` and connection-change events.
#[must_use]
pub fn watch() -> Watch {
    Watch {
        last: current(),
        inner: imp::Watch::new(),
    }
}

impl Watch {
    /// The status when the watch started or last changed.
    #[must_use]
    pub fn last(&self) -> NetStatus {
        self.last
    }

    /// Wait until the status differs from [`last`](Self::last), and return it.
    pub async fn changed(&mut self) -> NetStatus {
        loop {
            self.inner.wait().await;
            let now = current();
            if now != self.last {
                self.last = now;
                return now;
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
    const POLL: Duration = Duration::from_secs(5);

    /// Connecting a UDP socket sends nothing; it only asks the kernel for a
    /// route. These are documentation addresses, never contacted.
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

    pub(super) fn current() -> NetStatus {
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
            metered: metered::current(),
        }
    }

    /// `NetworkManager`'s view of the active devices, asked through `nmcli`
    /// at most once a minute (the watch polls every few seconds).
    #[cfg(target_os = "linux")]
    pub(crate) mod metered {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::sync::Mutex;
        use std::time::Duration;

        use tilo_clock::Instant;

        const FRESH: Duration = Duration::from_secs(60);
        const DEADLINE: Duration = Duration::from_secs(3);
        const MAX_OUTPUT: u64 = 256 * 1024;

        static LAST: Mutex<Option<(Instant, Option<bool>)>> = Mutex::new(None);

        pub(super) fn current() -> Option<bool> {
            let mut last = LAST
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((at, answer)) = *last
                && at.elapsed() < FRESH
            {
                return answer;
            }
            let answer = ask();
            *last = Some((tilo_clock::now(), answer));
            answer
        }

        fn ask() -> Option<bool> {
            let mut child = Command::new("nmcli")
                .args(["-t", "-f", "GENERAL.METERED", "device", "show"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let started = tilo_clock::now();
            let status = loop {
                if let Some(status) = child.try_wait().ok()? {
                    break status;
                }
                if started.elapsed() > DEADLINE {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut text = String::new();
            child
                .stdout
                .take()?
                .take(MAX_OUTPUT)
                .read_to_string(&mut text)
                .ok()?;
            status.success().then(|| parse(&text)).flatten()
        }

        /// `yes`/`yes (guessed)` on any device is metered; `no`/`no
        /// (guessed)` on at least one, and none metered, is not; only
        /// `unknown` says nothing.
        pub(crate) fn parse(text: &str) -> Option<bool> {
            let mut saw_known = false;
            for line in text.lines() {
                let value = line
                    .split_once(':')
                    .map_or("", |(_, value)| value)
                    .trim()
                    .to_ascii_lowercase();
                if value.starts_with("yes") {
                    return Some(true);
                }
                saw_known |= value.starts_with("no");
            }
            saw_known.then_some(false)
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod metered {
        pub(super) const fn current() -> Option<bool> {
            None
        }
    }

    pub(super) struct Watch;

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

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn the_route_probe_answers_without_sending() {
        // CI machines may be offline; the answer only has to be consistent.
        let status = current();
        #[cfg(not(target_os = "linux"))]
        assert_eq!(status.metered, None);
        assert_eq!(
            status.online(),
            status.connectivity != Connectivity::Offline
        );
        assert_eq!(watch().last().connectivity, status.connectivity);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_manager_answers_read_as_metered_or_not() {
        use super::imp::metered::parse;
        assert_eq!(
            parse("GENERAL.METERED:no\nGENERAL.METERED:yes (guessed)\n"),
            Some(true)
        );
        assert_eq!(
            parse("GENERAL.METERED:no (guessed)\nGENERAL.METERED:unknown\n"),
            Some(false)
        );
        assert_eq!(parse("GENERAL.METERED:unknown\n"), None);
        assert_eq!(parse(""), None);
    }
}
