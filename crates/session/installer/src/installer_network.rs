//! Non-blocking `NetworkManager` operations for the installer wizard.
//!
//! The live installer must remain usable when `NetworkManager`, a Wi-Fi adapter,
//! or internet access is absent. All potentially slow `nmcli` work therefore
//! happens on worker threads. Results are returned through a small shared queue
//! and the UI event-loop proxy is used only as a redraw wake-up.

use std::sync::{Arc, Mutex};

use os_actions::{ConnectivityState, WifiNetworkInfo};
use ui::app::UserEvent;
use winit::event_loop::EventLoopProxy;
use zeroize::Zeroizing;

/// External-event payload used only to wake the installer after a network
/// worker publishes into its shared result queue.
pub const NETWORK_WAKE_EVENT: &str = "\u{1}network";

/// Side-effect boundary for live `NetworkManager` calls. Keeping it narrow makes
/// the installer's decision logic testable without a radio or system bus.
pub trait NetworkBackend: Send + Sync {
    /// Check the system's connectivity now.
    ///
    /// # Errors
    ///
    /// Returns an error when the connectivity state cannot be queried.
    fn connectivity(&self) -> Result<ConnectivityState, String>;

    /// Scan for currently visible Wi-Fi networks.
    ///
    /// # Errors
    ///
    /// Returns an error when Wi-Fi networks cannot be enumerated.
    fn wifi_networks(&self) -> Result<Vec<WifiNetworkInfo>, String>;

    /// Obtain the sign-in URI for the current captive portal.
    ///
    /// # Errors
    ///
    /// Returns an error when the captive portal URI cannot be determined.
    fn captive_portal_uri(&self) -> Result<String, String>;

    /// Connect to `ssid`, using `password` when the network is secured.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection attempt cannot be started or completed.
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String>;
}

#[derive(Default)]
struct RealNetworkBackend;

impl NetworkBackend for RealNetworkBackend {
    fn connectivity(&self) -> Result<ConnectivityState, String> {
        // Every snapshot here is a deliberate look: the first scan, the
        // person pressing refresh, or the result of joining a network. Ask
        // NetworkManager to check again rather than repeat a stale verdict.
        os_actions::check_connectivity_now()
    }

    fn wifi_networks(&self) -> Result<Vec<WifiNetworkInfo>, String> {
        os_actions::list_wifi_networks()
    }

    fn captive_portal_uri(&self) -> Result<String, String> {
        os_actions::captive_portal_uri()
    }

    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        os_actions::connect_wifi(ssid, password)
    }
}

/// One best-effort view of networking. Connectivity and Wi-Fi discovery are
/// independent: wired internet can be usable when Wi-Fi scanning fails, and a
/// Wi-Fi list is useful even when `NetworkManager`'s internet check is disabled.
#[derive(Clone, Debug)]
pub struct NetworkSnapshot {
    pub connectivity: ConnectivityState,
    pub connectivity_error: Option<String>,
    pub networks: Vec<WifiNetworkInfo>,
    pub wifi_error: Option<String>,
    pub portal_uri: Option<String>,
    pub portal_error: Option<String>,
}

impl NetworkSnapshot {
    fn collect(backend: &dyn NetworkBackend) -> Self {
        let (connectivity, connectivity_error) = match backend.connectivity() {
            Ok(value) => (value, None),
            Err(error) => (ConnectivityState::Unknown, Some(error)),
        };
        let (mut networks, wifi_error) = match backend.wifi_networks() {
            Ok(value) => (value, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        sort_networks(&mut networks);

        let (portal_uri, portal_error) = if connectivity == ConnectivityState::Portal {
            match backend.captive_portal_uri() {
                Ok(uri) => (Some(uri), None),
                Err(error) => (None, Some(error)),
            }
        } else {
            (None, None)
        };

        Self {
            connectivity,
            connectivity_error,
            networks,
            wifi_error,
            portal_uri,
            portal_error,
        }
    }

    /// Truthful primary status for the current `NetworkManager` result.
    pub fn guidance(&self) -> &'static str {
        match self.connectivity {
            ConnectivityState::Full => {
                "Internet access is available. The OS itself still installs from this medium."
            }
            ConnectivityState::Portal => {
                "This network requires a sign-in page before it provides internet access."
            }
            ConnectivityState::Limited => {
                "A network is connected, but NetworkManager cannot confirm internet access."
            }
            ConnectivityState::Offline => {
                "No internet connection. That's okay: Tilo can install completely offline."
            }
            ConnectivityState::Unknown => {
                "Connectivity could not be confirmed. You can still install Tilo offline."
            }
        }
    }
}

fn sort_networks(networks: &mut [WifiNetworkInfo]) {
    networks.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
}

#[derive(Debug)]
enum WorkerResult {
    Scanned(NetworkSnapshot),
    Connected {
        ssid: String,
        result: Result<(), String>,
        snapshot: NetworkSnapshot,
    },
}

/// Current lifecycle of a background network operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkPhase {
    Idle,
    Scanning,
    Connecting(String),
}

/// Network state owned by `InstallerApp`.
pub struct InstallerNetwork {
    backend: Arc<dyn NetworkBackend>,
    results: Arc<Mutex<Vec<WorkerResult>>>,
    pub snapshot: NetworkSnapshot,
    pub phase: NetworkPhase,
    pub selected_ssid: Option<String>,
    pub notice: String,
    initial_scan_started: bool,
}

impl Default for InstallerNetwork {
    fn default() -> Self {
        Self::new(Arc::new(RealNetworkBackend))
    }
}

impl InstallerNetwork {
    fn new(backend: Arc<dyn NetworkBackend>) -> Self {
        Self {
            backend,
            results: Arc::new(Mutex::new(Vec::new())),
            snapshot: NetworkSnapshot {
                connectivity: ConnectivityState::Unknown,
                connectivity_error: None,
                networks: Vec::new(),
                wifi_error: None,
                portal_uri: None,
                portal_error: None,
            },
            phase: NetworkPhase::Idle,
            selected_ssid: None,
            notice: "Checking network availability…".to_string(),
            initial_scan_started: false,
        }
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, NetworkPhase::Idle)
    }

    pub fn selected_network(&self) -> Option<&WifiNetworkInfo> {
        let selected = self.selected_ssid.as_deref()?;
        self.snapshot
            .networks
            .iter()
            .find(|network| network.ssid == selected)
    }

    /// Start the automatic first scan once the live event-loop proxy exists.
    pub fn ensure_initial_scan(&mut self, proxy: Option<EventLoopProxy<UserEvent>>) -> bool {
        if self.initial_scan_started {
            return false;
        }
        let Some(proxy) = proxy else {
            return false;
        };
        self.initial_scan_started = true;
        self.start_scan(proxy)
    }

    pub fn start_scan(&mut self, proxy: EventLoopProxy<UserEvent>) -> bool {
        if self.is_busy() {
            return false;
        }
        self.phase = NetworkPhase::Scanning;
        self.notice = "Scanning for Wi-Fi and checking connectivity…".to_string();
        let backend = Arc::clone(&self.backend);
        let results = Arc::clone(&self.results);
        std::thread::spawn(move || {
            let snapshot = NetworkSnapshot::collect(backend.as_ref());
            if let Ok(mut queue) = results.lock() {
                queue.push(WorkerResult::Scanned(snapshot));
            }
            let _ = proxy.send_event(UserEvent::External(NETWORK_WAKE_EVENT.to_string()));
        });
        true
    }

    /// Validate and start a Wi-Fi connection. The password is moved into a
    /// zeroizing buffer before the worker is launched and never appears in
    /// argv or an external-event payload.
    pub fn start_connect(
        &mut self,
        password: Zeroizing<String>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<(), &'static str> {
        if self.is_busy() {
            return Err("A network operation is already in progress");
        }
        let Some(ssid) = self.selected_ssid.clone() else {
            return Err("Choose a Wi-Fi network first");
        };
        let Some(network) = self
            .snapshot
            .networks
            .iter()
            .find(|network| network.ssid == ssid)
        else {
            return Err("That Wi-Fi network is no longer visible; refresh the list");
        };
        if network.connected {
            return Err("That Wi-Fi network is already connected");
        }
        if network.secure && password.is_empty() {
            return Err("Enter the Wi-Fi password");
        }

        let secure = network.secure;
        self.phase = NetworkPhase::Connecting(ssid.clone());
        self.notice = format!("Connecting to {ssid}…");
        let backend = Arc::clone(&self.backend);
        let results = Arc::clone(&self.results);
        std::thread::spawn(move || {
            let password = secure.then_some(password.as_str());
            let result = backend.connect_wifi(&ssid, password);
            let snapshot = NetworkSnapshot::collect(backend.as_ref());
            if let Ok(mut queue) = results.lock() {
                queue.push(WorkerResult::Connected {
                    ssid,
                    result,
                    snapshot,
                });
            }
            let _ = proxy.send_event(UserEvent::External(NETWORK_WAKE_EVENT.to_string()));
        });
        Ok(())
    }

    /// Fold all completed worker results into UI state. Returns true when a
    /// result changed something visible.
    pub fn drain_results(&mut self) -> bool {
        let results = self
            .results
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        if results.is_empty() {
            return false;
        }

        for result in results {
            match result {
                WorkerResult::Scanned(snapshot) => {
                    self.snapshot = snapshot;
                    self.notice = self.snapshot.guidance().to_string();
                }
                WorkerResult::Connected {
                    ssid,
                    result,
                    snapshot,
                } => {
                    self.snapshot = snapshot;
                    self.notice = match result {
                        Ok(()) => format!("Connected to {ssid}. {}", self.snapshot.guidance()),
                        Err(error) => format!("Could not connect to {ssid}: {error}"),
                    };
                }
            }
        }
        self.phase = NetworkPhase::Idle;
        if self.selected_ssid.as_ref().is_some_and(|ssid| {
            !self
                .snapshot
                .networks
                .iter()
                .any(|network| &network.ssid == ssid)
        }) {
            self.selected_ssid = None;
        }
        true
    }

    pub fn connection_summary(&self) -> String {
        if self.snapshot.connectivity == ConnectivityState::Full {
            if let Some(network) = self
                .snapshot
                .networks
                .iter()
                .find(|network| network.connected)
            {
                return format!("Internet via {} (saved for first boot)", network.ssid);
            }
            return "Internet available".to_string();
        }
        format!(
            "{} — offline installation remains available",
            self.snapshot.connectivity.label()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBackend {
        connectivity: Result<ConnectivityState, String>,
        networks: Result<Vec<WifiNetworkInfo>, String>,
        portal: Result<String, String>,
    }

    impl NetworkBackend for FakeBackend {
        fn connectivity(&self) -> Result<ConnectivityState, String> {
            self.connectivity.clone()
        }
        fn wifi_networks(&self) -> Result<Vec<WifiNetworkInfo>, String> {
            self.networks.clone()
        }
        fn captive_portal_uri(&self) -> Result<String, String> {
            self.portal.clone()
        }
        fn connect_wifi(&self, _ssid: &str, _password: Option<&str>) -> Result<(), String> {
            Ok(())
        }
    }

    fn wifi(ssid: &str, signal: u8, secure: bool, connected: bool) -> WifiNetworkInfo {
        WifiNetworkInfo {
            ssid: ssid.to_string(),
            signal,
            secure,
            security: if secure {
                os_actions::WifiSecurity::Wpa2
            } else {
                os_actions::WifiSecurity::Open
            },
            connected,
            known: false,
        }
    }

    #[test]
    fn network_snapshot_orders_networks_and_guides_offline_installs() {
        {
            let backend = FakeBackend {
                connectivity: Err("check disabled".to_string()),
                networks: Ok(vec![wifi("Cafe", 2, false, false)]),
                portal: Err("unused".to_string()),
            };
            let snapshot = NetworkSnapshot::collect(&backend);
            assert_eq!(snapshot.connectivity, ConnectivityState::Unknown);
            assert_eq!(snapshot.networks.len(), 1);
            assert_eq!(
                snapshot.connectivity_error.as_deref(),
                Some("check disabled")
            );
            assert!(snapshot.guidance().contains("could not be confirmed"));
        }
        {
            let backend = FakeBackend {
                connectivity: Ok(ConnectivityState::Portal),
                networks: Ok(Vec::new()),
                portal: Ok("http://nm-check.example/".to_string()),
            };
            let snapshot = NetworkSnapshot::collect(&backend);
            assert_eq!(
                snapshot.portal_uri.as_deref(),
                Some("http://nm-check.example/")
            );
            assert!(snapshot.guidance().contains("sign-in page"));
        }
        {
            let mut networks = vec![
                wifi("Weak", 1, true, false),
                wifi("Connected", 2, true, true),
                wifi("Strong", 4, false, false),
            ];
            sort_networks(&mut networks);
            let names: Vec<&str> = networks
                .iter()
                .map(|network| network.ssid.as_str())
                .collect();
            assert_eq!(names, vec!["Connected", "Strong", "Weak"]);
        }
        {
            let backend = FakeBackend {
                connectivity: Ok(ConnectivityState::Offline),
                networks: Ok(Vec::new()),
                portal: Err("unused".to_string()),
            };
            let snapshot = NetworkSnapshot::collect(&backend);
            assert!(snapshot.guidance().contains("install completely offline"));
        }
    }
}
