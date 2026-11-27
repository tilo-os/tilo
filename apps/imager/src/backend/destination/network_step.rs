//! The network step's view over the system's connectivity service.
//!
//! The installer on the live medium runs before any person or workspace
//! exists, so it drives the service itself: [`Connectivity::unmanaged`] reads
//! and changes this device on its worker thread, and this holds only what the
//! step shows of that reading.

use std::sync::Arc;

use tilo_connectivity_service::Connectivity as Live;
use tilo_connectivity_service::Service as _;
pub use tilo_connectivity_service::Wake;
use tilo_connectivity_service::device::OsDevice;
pub use tilo_connectivity_service::model::{Connectivity, WifiNetwork};
use zeroize::Zeroizing;

// What a test lends the step instead of this machine's radio.
#[cfg(any(test, feature = "test-support"))]
pub use tilo_connectivity_service::device::{Change, Device, Readings};
#[cfg(any(test, feature = "test-support"))]
pub use tilo_connectivity_service::model::{Network, Wifi};

/// What the step is waiting for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkPhase {
    Idle,
    Scanning,
    Connecting(String),
}

pub struct NetworkStep {
    service: Option<Live>,
    /// The networks in reach, the connected one first, then the strongest.
    pub networks: Vec<WifiNetwork>,
    pub connectivity: Connectivity,
    /// Why the last search failed, until the next one.
    pub wifi_error: Option<String>,
    /// The sign-in page a captive portal asks for.
    pub portal_uri: Option<String>,
    /// Why the sign-in page could not be read.
    pub portal_error: Option<String>,
    pub phase: NetworkPhase,
    pub selected_ssid: Option<String>,
    pub notice: String,
    started: bool,
}

impl Default for NetworkStep {
    fn default() -> Self {
        Self {
            service: None,
            networks: Vec::new(),
            connectivity: Connectivity::Unknown,
            wifi_error: None,
            portal_uri: None,
            portal_error: None,
            phase: NetworkPhase::Idle,
            selected_ssid: None,
            notice: "Checking network availability…".to_string(),
            started: false,
        }
    }
}

impl NetworkStep {
    /// The step over `device` instead of the machine, so a test drives it with
    /// no radio and no system bus.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(device: Arc<dyn Device>, wake: Option<Wake>) -> Self {
        let mut step = Self {
            service: match Live::unmanaged(device, wake.clone()) {
                Ok(service) => Some(service),
                Err(error) => {
                    return Self {
                        wifi_error: Some(error.to_string()),
                        ..Self::default()
                    };
                }
            },
            started: true,
            ..Self::default()
        };
        if let Some(wake) = wake {
            step.start_scan(wake);
        }
        step
    }

    /// The device's own service, made once there is a wake to hear it on.
    fn open(&mut self, wake: Wake) -> bool {
        if self.service.is_some() {
            return true;
        }
        match Live::unmanaged(Arc::new(OsDevice), Some(wake)) {
            Ok(service) => {
                self.service = Some(service);
                true
            }
            Err(error) => {
                self.wifi_error = Some(error.to_string());
                self.notice = self.guidance().to_string();
                false
            }
        }
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.phase, NetworkPhase::Idle)
    }

    pub fn selected_network(&self) -> Option<&WifiNetwork> {
        let selected = self.selected_ssid.as_deref()?;
        self.networks
            .iter()
            .find(|network| network.name == selected)
    }

    /// What the reading means for an install, in the words the person continues by.
    pub fn guidance(&self) -> &'static str {
        match self.connectivity {
            Connectivity::Full => {
                "Internet access is available. The OS itself still installs from this medium."
            }
            Connectivity::Portal => {
                "This network requires a sign-in page before it provides internet access."
            }
            Connectivity::Limited => {
                "A network is connected, but the internet could not be confirmed."
            }
            Connectivity::Offline => {
                "No internet connection. That's okay: Tilo can install completely offline."
            }
            Connectivity::Unknown => {
                "Connectivity could not be confirmed. You can still install Tilo offline."
            }
        }
    }

    /// Look once, on the first frame a wake exists; nothing after that.
    pub fn ensure_initial_scan(&mut self, wake: Option<Wake>) -> bool {
        if self.started {
            return false;
        }
        let Some(wake) = wake else {
            return false;
        };
        self.started = true;
        self.start_scan(wake)
    }

    pub fn start_scan(&mut self, wake: Wake) -> bool {
        if self.is_busy() {
            return false;
        }
        if !self.open(wake) {
            return true;
        }
        let Some(service) = self.service.as_mut() else {
            return false;
        };
        self.phase = NetworkPhase::Scanning;
        self.notice = "Scanning for Wi-Fi and checking connectivity…".to_string();
        service.scan();
        true
    }

    /// The password is moved into a zeroizing buffer before the service starts
    /// and never appears in argv or an external-event payload.
    pub fn start_connect(&mut self, password: Zeroizing<String>, wake: Wake) -> Result<(), String> {
        if self.is_busy() {
            return Err("A network operation is already in progress".to_string());
        }
        let Some(ssid) = self.selected_ssid.clone() else {
            return Err("Choose a Wi-Fi network first".to_string());
        };
        let Some(network) = self.networks.iter().find(|network| network.name == ssid) else {
            return Err("That Wi-Fi network is no longer visible; refresh the list".to_string());
        };
        if network.connected {
            return Err("That Wi-Fi network is already connected".to_string());
        }
        let secure = network.secure;
        if secure && password.is_empty() {
            return Err("Enter the Wi-Fi password".to_string());
        }
        if !self.open(wake) {
            return Err(self.notice.clone());
        }
        let Some(service) = self.service.as_mut() else {
            return Err("The network controls are not available".to_string());
        };
        service.join(&ssid, secure.then(|| password.as_str()));
        self.phase = NetworkPhase::Connecting(ssid.clone());
        self.notice = format!("Connecting to {ssid}…");
        Ok(())
    }

    /// Returns true when a result changed something visible.
    pub fn drain_results(&mut self) -> bool {
        let Some(service) = self.service.as_mut() else {
            return false;
        };
        let landed = service.poll();
        let network = &service.readings().network;
        let problem = network.problem.clone();
        self.connectivity = network.connectivity;
        self.networks.clone_from(&network.wifi.networks);
        self.wifi_error.clone_from(&network.wifi.scan_problem);
        self.portal_uri.clone_from(&network.portal_uri);
        self.portal_error.clone_from(&network.portal_problem);
        let mut done = false;
        if !matches!(self.phase, NetworkPhase::Idle) && network.pending.is_empty() {
            done = true;
            let phase = std::mem::replace(&mut self.phase, NetworkPhase::Idle);
            self.notice = match phase {
                NetworkPhase::Connecting(ssid) => {
                    let row = self.networks.iter().find(|network| network.name == ssid);
                    match row {
                        Some(row) if row.connected => {
                            format!("Connected to {ssid}. {}", self.guidance())
                        }
                        Some(row) => {
                            let why = row
                                .problem
                                .as_deref()
                                .or(problem.as_deref())
                                .unwrap_or("the network did not answer");
                            format!("Could not connect to {ssid}: {why}")
                        }
                        None => format!(
                            "Could not connect to {ssid}: {}",
                            problem
                                .as_deref()
                                .unwrap_or("the network is no longer in reach")
                        ),
                    }
                }
                NetworkPhase::Scanning | NetworkPhase::Idle => self.guidance().to_string(),
            };
        }
        if self
            .selected_ssid
            .as_ref()
            .is_some_and(|ssid| !self.networks.iter().any(|network| &network.name == ssid))
        {
            self.selected_ssid = None;
        }
        landed || done
    }
}
