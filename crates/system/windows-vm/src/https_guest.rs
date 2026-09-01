//! Authenticated host client for the Windows guest bridge.
//!
//! The bridge is reachable only over qOS's non-forwarding libvirt network.
//! Its first, HMAC-authenticated health response carries the self-signed
//! certificate.  qOS validates that response with the root-only bootstrap
//! key, records the exact certificate DER and SHA-256 digest, and uses it as
//! a Rustls trust anchor thereafter.  A different certificate is a repair
//! condition, never an automatic re-enrolment.

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use reqwest::Certificate;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::appliance::HOST_PROTOCOL_MAJOR;
use crate::{
    AgentError, AgentHealth, ApplicationPresentation, Catalog, GuestAgent, GuestLaunch,
    GuestSession, LicenseState, RdpEndpoint, Secret,
};

const DEFAULT_ENDPOINT: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 251, 2)), 49_888);
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
const MAX_CERTIFICATE_BYTES: usize = 64 * 1024;
const MAX_SECRET_BYTES: usize = 128;
const PIN_FILE: &str = "guest-bridge.sha256";
const CERTIFICATE_FILE: &str = "guest-bridge.der";

/// Root-owned bridge locations and the fixed host-only endpoint.  qOS's
/// libvirt network reserves `192.168.251.2` for the singleton appliance; the
/// constructor rejects all other address ranges and public ports.
#[derive(Clone, Debug)]
pub struct GuestBridgeConfig {
    pub endpoint: SocketAddr,
    pub bootstrap_secret: PathBuf,
    pub pin_directory: PathBuf,
    /// Host presentation selected before the guest credential is minted.
    pub presentation: ApplicationPresentation,
}

impl GuestBridgeConfig {
    #[must_use]
    pub fn system(presentation: ApplicationPresentation) -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT,
            bootstrap_secret: PathBuf::from("/var/lib/qos/windows/bridge/bootstrap.secret"),
            pin_directory: PathBuf::from("/var/lib/qos/windows/bridge"),
            presentation,
        }
    }

    fn validate(&self) -> Result<(), AgentError> {
        let expected_network = Ipv4Addr::new(192, 168, 251, 0);
        let IpAddr::V4(address) = self.endpoint.ip() else {
            return Err(AgentError::InvalidEndpoint);
        };
        if address.octets()[0..3] != expected_network.octets()[0..3]
            || address.octets()[3] != 2
            || self.endpoint.port() != 49_888
            || !self.bootstrap_secret.is_absolute()
            || !self.pin_directory.is_absolute()
        {
            return Err(AgentError::InvalidEndpoint);
        }
        Ok(())
    }
}

/// Concrete [`GuestAgent`] used by the privileged Windows compatibility host.
/// No shell is involved in HTTPS, certificate validation, HMAC generation, or
/// JSON decoding.
pub struct HttpsGuestBridge {
    config: GuestBridgeConfig,
    bootstrap_secret: Vec<u8>,
}

impl HttpsGuestBridge {
    /// Opens the root-only bootstrap secret.  The caller is normally the
    /// privileged qOS compatibility host; an unprivileged manager receives no
    /// instance of this type and cannot read or use this key.
    pub fn new(config: GuestBridgeConfig) -> Result<Self, AgentError> {
        config.validate()?;
        let bootstrap_secret = read_private_file(&config.bootstrap_secret, MAX_SECRET_BYTES)?;
        if bootstrap_secret.len() < 32 {
            return Err(AgentError::Authentication);
        }
        Ok(Self {
            config,
            bootstrap_secret,
        })
    }

    fn bridge_url(&self, path: &str) -> String {
        format!("https://{}{}", self.config.endpoint, path)
    }

    fn pinned_client(&self) -> Result<Client, AgentError> {
        let certificate = read_private_file(
            &self.config.pin_directory.join(CERTIFICATE_FILE),
            MAX_CERTIFICATE_BYTES,
        )?;
        let expected = read_pin(&self.config.pin_directory.join(PIN_FILE))?;
        if sha256_hex(&certificate) != expected {
            return Err(AgentError::Authentication);
        }
        let certificate = Certificate::from_der(&certificate)
            .map_err(|error| AgentError::Transport(format!("stored guest certificate: {error}")))?;
        Client::builder()
            .use_rustls_tls()
            .add_root_certificate(certificate)
            // The enrolled guest certificate is self-signed for its qOS DNS
            // name while the host-only bridge has a fixed numeric endpoint.
            // Chain validation still occurs against the exact persisted DER.
            .danger_accept_invalid_hostnames(true)
            .connect_timeout(BRIDGE_TIMEOUT)
            .timeout(BRIDGE_TIMEOUT)
            .build()
            .map_err(|error| AgentError::Transport(format!("TLS client: {error}")))
    }

    fn bootstrap_client() -> Result<Client, AgentError> {
        Client::builder()
            .use_rustls_tls()
            // Only the first response is allowed to use this client. Its body
            // is verified with the root-only bootstrap HMAC before its DER is
            // persisted as the Rustls trust anchor.
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .connect_timeout(BRIDGE_TIMEOUT)
            .timeout(BRIDGE_TIMEOUT)
            .build()
            .map_err(|error| AgentError::Transport(format!("bootstrap TLS client: {error}")))
    }

    fn client(&self) -> Result<Client, AgentError> {
        match self.pinned_client() {
            Ok(client) => Ok(client),
            Err(AgentError::Transport(_) | AgentError::Authentication) => self.enrol(),
            Err(error) => Err(error),
        }
    }

    fn enrol(&self) -> Result<Client, AgentError> {
        let client = Self::bootstrap_client()?;
        let (body, response) = self.request_with(&client, "GET", "/v1/health", &[])?;
        self.verify_response(&response, &body)?;
        let health: WireHealth = deserialize(&body)?;
        let certificate = health.certificate_der()?;
        let digest = sha256_hex(&certificate);
        if digest != health.certificate_sha256.to_ascii_lowercase() {
            return Err(AgentError::Authentication);
        }
        let pin_path = self.config.pin_directory.join(PIN_FILE);
        if pin_path.exists() && read_pin(&pin_path)? != digest {
            return Err(AgentError::Authentication);
        }
        write_private_new_or_equal(&pin_path, digest.as_bytes())?;
        write_private_new_or_equal(
            &self.config.pin_directory.join(CERTIFICATE_FILE),
            &certificate,
        )?;
        self.pinned_client()
    }

    fn request(&self, method: &str, path: &str, body: &[u8]) -> Result<Vec<u8>, AgentError> {
        let client = self.client()?;
        let (response_body, response) = self.request_with(&client, method, path, body)?;
        self.verify_response(&response, &response_body)?;
        Ok(response_body)
    }

    fn request_with(
        &self,
        client: &Client,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<(Vec<u8>, Response), AgentError> {
        let timestamp = now_unix()?;
        let nonce = nonce();
        let authentication = hmac_hex(
            &self.bootstrap_secret,
            format!("{timestamp}\n{nonce}\n").as_bytes(),
            body,
        );
        let mut request = match method {
            "GET" => client.get(self.bridge_url(path)),
            "POST" => client.post(self.bridge_url(path)).body(body.to_vec()),
            _ => {
                return Err(AgentError::Transport(
                    "unsupported bridge method".to_string(),
                ));
            }
        };
        request = request
            .header("X-qOS-Timestamp", timestamp.to_string())
            .header("X-qOS-Nonce", &nonce)
            .header("X-qOS-Auth", authentication)
            .header("Accept", "application/json");
        if method == "POST" {
            request = request.header("Content-Type", "application/json");
        }
        let mut response = request
            .send()
            .map_err(|error| AgentError::Transport(format!("guest bridge: {error}")))?;
        if !response.status().is_success() {
            return Err(AgentError::Transport(format!(
                "guest bridge rejected request with HTTP {}",
                response.status()
            )));
        }
        let response_auth = response
            .headers()
            .get("X-qOS-Response-Auth")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
            .ok_or(AgentError::Authentication)?;
        let mut limited = response.by_ref().take(MAX_RESPONSE_BYTES + 1);
        let mut response_body = Vec::new();
        limited
            .read_to_end(&mut response_body)
            .map_err(|error| AgentError::Transport(format!("guest bridge body: {error}")))?;
        if response_body.len() > usize::try_from(MAX_RESPONSE_BYTES).unwrap_or(usize::MAX) {
            return Err(AgentError::Transport(
                "guest bridge response was too large".to_string(),
            ));
        }
        // Retain the authenticated header only through this response object;
        // response bodies are bounded before JSON parsing.
        response
            .extensions_mut()
            .insert(ResponseAuth(response_auth));
        response
            .extensions_mut()
            .insert(RequestBinding { timestamp, nonce });
        Ok((response_body, response))
    }

    fn verify_response(&self, response: &Response, body: &[u8]) -> Result<(), AgentError> {
        let Some(ResponseAuth(provided)) = response.extensions().get::<ResponseAuth>() else {
            return Err(AgentError::Authentication);
        };
        // The bridge response MAC is calculated over the exact request
        // timestamp/nonce plus response bytes. Those request values are not
        // retained by reqwest, so they are copied into response extensions by
        // `request_with` before this verifier is called.
        let Some(RequestBinding { timestamp, nonce }) =
            response.extensions().get::<RequestBinding>()
        else {
            return Err(AgentError::Authentication);
        };
        let expected = hmac_hex(
            &self.bootstrap_secret,
            format!("{timestamp}\n{nonce}\n").as_bytes(),
            body,
        );
        if constant_time_eq(expected.as_bytes(), provided.as_bytes()) {
            Ok(())
        } else {
            Err(AgentError::Authentication)
        }
    }
}

impl GuestAgent for HttpsGuestBridge {
    fn wait_ready(&self, _timeout: Duration) -> Result<GuestSession, AgentError> {
        let body = self.request("GET", "/v1/health", &[])?;
        let health: WireHealth = deserialize(&body)?;
        health.validate()?;
        // The health request itself is mutually authenticated. The nonce is a
        // fresh opaque host session id; every later operation is again HMAC
        // authenticated, so a guest-side cookie is not a second authority.
        Ok(GuestSession {
            id: nonce(),
            health: health.agent_health()?,
        })
    }

    fn catalog(&self, _session: &GuestSession) -> Result<Catalog, AgentError> {
        let body = self.request("GET", "/v1/catalog", &[])?;
        let catalog: Catalog = deserialize(&body)?;
        catalog.validate().map_err(AgentError::InvalidCatalog)?;
        Ok(catalog)
    }

    fn prepare_launch(
        &self,
        session: &GuestSession,
        application_id: &str,
    ) -> Result<GuestLaunch, AgentError> {
        if session.id.len() < 16 || !valid_identifier(application_id) {
            return Err(AgentError::NotPublished(application_id.to_string()));
        }
        let request = WireLaunchRequest {
            application_id,
            launch_nonce: nonce(),
            presentation_backend: match self.config.presentation {
                ApplicationPresentation::NativeRail => "native_rail",
                ApplicationPresentation::AppFocusedSpice => "app_focused_spice",
            },
        };
        let body = serde_json::to_vec(&request)
            .map_err(|error| AgentError::Transport(format!("launch request JSON: {error}")))?;
        let launch: WireLaunch = deserialize(&self.request("POST", "/v1/launch", &body)?)?;
        launch.into_guest_launch(application_id, self.config.presentation)
    }
}

#[derive(Clone, Debug)]
struct ResponseAuth(String);

#[derive(Clone, Debug)]
struct RequestBinding {
    timestamp: u64,
    nonce: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireHealth {
    protocol_major: u16,
    certificate_sha256: String,
    certificate_der_base64: String,
    license: String,
    catalog_generation: u64,
    guest_boot_id: String,
    presentation_backend: String,
    app_focused_spice_available: bool,
    app_focused_spice_unavailable_reason: Option<String>,
}

impl WireHealth {
    fn certificate_der(&self) -> Result<Vec<u8>, AgentError> {
        let certificate = base64::engine::general_purpose::STANDARD
            .decode(&self.certificate_der_base64)
            .map_err(|_| AgentError::Authentication)?;
        if certificate.is_empty() || certificate.len() > MAX_CERTIFICATE_BYTES {
            return Err(AgentError::Authentication);
        }
        Ok(certificate)
    }

    fn validate(&self) -> Result<(), AgentError> {
        if self.protocol_major != HOST_PROTOCOL_MAJOR
            || !is_sha256(&self.certificate_sha256)
            || self.guest_boot_id.is_empty()
            || self.guest_boot_id.len() > 128
            || self.guest_boot_id.chars().any(char::is_control)
        {
            return Err(AgentError::Authentication);
        }
        self.certificate_der()?;
        if !matches!(
            self.presentation_backend.as_str(),
            "native_rail_rds" | "native_rail_client_best_effort" | "app_focused_spice"
        ) {
            return Err(AgentError::Transport(
                "guest reported unknown presentation".to_string(),
            ));
        }
        if !self.app_focused_spice_available
            && self
                .app_focused_spice_unavailable_reason
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(AgentError::Transport(
                "guest omitted SPICE status reason".to_string(),
            ));
        }
        Ok(())
    }

    fn agent_health(&self) -> Result<AgentHealth, AgentError> {
        self.validate()?;
        Ok(AgentHealth {
            protocol_major: self.protocol_major,
            guest_boot_id: self.guest_boot_id.clone(),
            catalog_generation: self.catalog_generation,
            license: match self.license.as_str() {
                "licensed" => LicenseState::ProductKeyActivated,
                "unlicensed" => LicenseState::Unlicensed,
                _ => LicenseState::Unknown,
            },
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireLaunchRequest<'a> {
    application_id: &'a str,
    launch_nonce: String,
    presentation_backend: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireLaunch {
    endpoint: String,
    username: String,
    password: String,
    remote_app_alias: String,
    launch_nonce: String,
    certificate_sha256: String,
    presentation_backend: String,
}

impl WireLaunch {
    fn into_guest_launch(
        self,
        application_id: &str,
        requested: ApplicationPresentation,
    ) -> Result<GuestLaunch, AgentError> {
        let endpoint = self
            .endpoint
            .parse()
            .map_err(|_| AgentError::InvalidEndpoint)?;
        if endpoint != SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 251, 2)), 3389)
            || !valid_identifier(&self.remote_app_alias)
            || self.remote_app_alias != application_id
            || !valid_identifier(&self.launch_nonce)
            || self.launch_nonce.len() < 16
            || !valid_username(&self.username)
            || self.password.is_empty()
            || self.password.len() > 512
            || !is_sha256(&self.certificate_sha256)
        {
            return Err(AgentError::Authentication);
        }
        let presentation = match self.presentation_backend.as_str() {
            "native_rail_rds" | "native_rail_client_best_effort"
                if requested == ApplicationPresentation::NativeRail =>
            {
                ApplicationPresentation::NativeRail
            }
            "app_focused_spice" if requested == ApplicationPresentation::AppFocusedSpice => {
                ApplicationPresentation::AppFocusedSpice
            }
            _ => {
                return Err(AgentError::Transport(
                    "guest presentation probe changed".to_string(),
                ));
            }
        };
        Ok(GuestLaunch {
            endpoint: RdpEndpoint {
                address: endpoint,
                username: self.username,
                certificate_sha256: self.certificate_sha256,
            },
            password: Secret::new(self.password.into_bytes()),
            remote_app_alias: self.remote_app_alias,
            launch_nonce: self.launch_nonce,
            presentation,
        })
    }
}

fn deserialize<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, AgentError> {
    serde_json::from_slice(body)
        .map_err(|error| AgentError::Transport(format!("guest JSON: {error}")))
}

fn read_private_file(path: &Path, maximum: usize) -> Result<Vec<u8>, AgentError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| AgentError::Transport(format!("bridge state: {error}")))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o077 != 0
    {
        return Err(AgentError::Authentication);
    }
    let bytes =
        fs::read(path).map_err(|error| AgentError::Transport(format!("bridge state: {error}")))?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(AgentError::Authentication);
    }
    Ok(bytes)
}

fn read_pin(path: &Path) -> Result<String, AgentError> {
    let bytes = read_private_file(path, 128)?;
    let pin = std::str::from_utf8(&bytes)
        .map_err(|_| AgentError::Authentication)?
        .trim()
        .to_ascii_lowercase();
    if is_sha256(&pin) {
        Ok(pin)
    } else {
        Err(AgentError::Authentication)
    }
}

fn write_private_new_or_equal(path: &Path, value: &[u8]) -> Result<(), AgentError> {
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Transport("bridge state has no parent".to_string()))?;
    fs::create_dir_all(parent)
        .map_err(|error| AgentError::Transport(format!("bridge state: {error}")))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| AgentError::Transport(format!("bridge state: {error}")))?;
    if path.exists() {
        let existing = read_private_file(path, MAX_CERTIFICATE_BYTES)?;
        if constant_time_eq(&existing, value) {
            return Ok(());
        }
        return Err(AgentError::Authentication);
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| AgentError::Transport(format!("bridge state: {error}")))?;
    file.write_all(value)
        .and_then(|()| file.sync_all())
        .map_err(|error| AgentError::Transport(format!("bridge state: {error}")))
}

fn now_unix() -> Result<u64, AgentError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| AgentError::Transport("system clock precedes UNIX epoch".to_string()))
}

fn nonce() -> String {
    let mut bytes = [0_u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn hmac_hex(key: &[u8], prefix: &[u8], body: &[u8]) -> String {
    let mut padded = [0_u8; 64];
    if key.len() > padded.len() {
        padded[..32].copy_from_slice(&Sha256::digest(key));
    } else if let Some(destination) = padded.get_mut(..key.len()) {
        destination.copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(padded.map(|byte| byte ^ 0x36));
    inner.update(prefix);
    inner.update(body);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(padded.map(|byte| byte ^ 0x5c));
    outer.update(inner);
    hex::encode(outer.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (first, second) in left.iter().zip(right) {
        difference |= usize::from(*first ^ *second);
    }
    difference == 0
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('/')
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_accepts_only_its_fixed_host_only_endpoint() {
        assert!(
            GuestBridgeConfig::system(ApplicationPresentation::NativeRail)
                .validate()
                .is_ok()
        );
        let mut configuration = GuestBridgeConfig::system(ApplicationPresentation::NativeRail);
        configuration.endpoint = "127.0.0.1:49888".parse().unwrap();
        assert!(matches!(
            configuration.validate(),
            Err(AgentError::InvalidEndpoint)
        ));
    }

    #[test]
    fn hmac_is_stable_and_body_bound() {
        let key = [7_u8; 32];
        let first = hmac_hex(&key, b"7\nnonce\n", b"body");
        assert_eq!(first, hmac_hex(&key, b"7\nnonce\n", b"body"));
        assert_ne!(first, hmac_hex(&key, b"7\nnonce\n", b"different"));
        assert!(constant_time_eq(first.as_bytes(), first.as_bytes()));
    }

    #[test]
    fn launch_rejects_a_guest_that_changes_selected_presentation() {
        let launch = WireLaunch {
            endpoint: "192.168.251.2:3389".to_string(),
            username: "qra123".to_string(),
            password: "secret".to_string(),
            remote_app_alias: "published.app".to_string(),
            launch_nonce: "0123456789abcdef".to_string(),
            certificate_sha256: "a".repeat(64),
            presentation_backend: "app_focused_spice".to_string(),
        };
        assert!(
            launch
                .into_guest_launch("published.app", ApplicationPresentation::NativeRail)
                .is_err()
        );
    }
}
