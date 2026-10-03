//! Authenticated host client for the Windows guest bridge.
//!
//! The bridge is reachable only over Tilo's non-forwarding libvirt network.
//! Its first, HMAC-authenticated health response carries the self-signed
//! certificate.  Tilo validates that response with the root-only bootstrap
//! key, records the exact certificate DER and SHA-256 digest, and uses it as
//! a Rustls trust anchor thereafter.  A different certificate is a repair
//! condition, never an automatic re-enrolment.

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand::Rng;
use reqwest::Certificate;
use reqwest::blocking::{Body, Client, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tilo_clock::try_unix_now;

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
/// A deliberately finite appliance import. The stream is never buffered in
/// memory by this crate, but both ends reject anything larger before writing.
pub const MAX_INSTALLER_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const INSTALLER_UPLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_INSTALLER_EXPIRY_SECONDS: u64 = 60 * 60;
const PIN_FILE: &str = "guest-bridge.sha256";
const CERTIFICATE_FILE: &str = "guest-bridge.der";
// `pin_directory` is shared with the owner-writable session state in the live
// appliance.  Pins get their own private child so first enrollment never
// changes the access mode of the state directory.
const PINS_DIRECTORY: &str = "guest-bridge-pins";

/// The only two package formats accepted by the appliance import protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallerKind {
    Exe,
    Msi,
}

impl InstallerKind {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Exe => "exe",
            Self::Msi => "msi",
        }
    }
}

/// Host-generated identity and immutable digest for one portal-owned stream.
/// It intentionally contains no host or guest path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallerPackage {
    package_id: String,
    kind: InstallerKind,
    byte_len: u64,
    sha256: String,
}

impl InstallerPackage {
    pub fn new(
        kind: InstallerKind,
        byte_len: u64,
        sha256: impl Into<String>,
    ) -> Result<Self, AgentError> {
        let sha256 = sha256.into().to_ascii_lowercase();
        if byte_len == 0 || byte_len > MAX_INSTALLER_BYTES || !is_sha256(&sha256) {
            return Err(AgentError::InvalidInstaller);
        }
        Ok(Self {
            package_id: nonce(),
            kind,
            byte_len,
            sha256,
        })
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.package_id
    }

    #[must_use]
    pub fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// A package accepted into the guest's one-use, expiring staging area.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedInstaller {
    package_id: String,
    expires_at_unix: u64,
}

impl StagedInstaller {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.package_id
    }

    #[must_use]
    pub fn expires_at_unix(&self) -> u64 {
        self.expires_at_unix
    }
}

/// Opaque handle for one installer process in the managed interactive session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationOperationId(String);

impl InstallationOperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, AgentError> {
        let value = value.into();
        if valid_identifier(&value) && value.len() >= 16 {
            Ok(Self(value))
        } else {
            Err(AgentError::InvalidInstaller)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One accepted installer plus the explicit app-focused surface that makes its
/// interactive UI visible. The desktop remains unavailable as a launch type.
#[derive(Debug)]
pub struct PreparedInstallation {
    pub operation_id: InstallationOperationId,
    pub(crate) presentation: GuestLaunch,
}

/// Opaque handle for one discovered Windows Installer removal operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallationOperationId(String);

impl UninstallationOperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, AgentError> {
        let value = value.into();
        if valid_identifier(&value) && value.len() >= 16 {
            Ok(Self(value))
        } else {
            Err(AgentError::InvalidInstaller)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct PreparedUninstallation {
    pub operation_id: UninstallationOperationId,
    pub(crate) presentation: GuestLaunch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallationStatus {
    pub operation_id: UninstallationOperationId,
    pub state: InstallationState,
    pub exit_code: Option<u32>,
    /// Set only after a successful removal also retracts its publication.
    pub catalog_generation: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Starting,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredUninstaller {
    pub id: String,
    pub display_name: String,
    pub display_version: Option<String>,
    pub publisher: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredApplication {
    pub id: String,
    pub display_name: String,
    pub product_name: Option<String>,
    pub publisher: Option<String>,
    pub version: Option<String>,
    pub uninstall: Option<DiscoveredUninstaller>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallationStatus {
    pub operation_id: InstallationOperationId,
    pub state: InstallationState,
    pub exit_code: Option<u32>,
    pub candidates: Vec<DiscoveredApplication>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationResult {
    pub application_id: String,
    pub catalog_generation: u64,
}

/// Root-owned bridge locations and the fixed host-only endpoint.  Tilo's
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
            bootstrap_secret: PathBuf::from("/var/lib/tilo/windows/bridge/bootstrap.secret"),
            pin_directory: PathBuf::from("/var/lib/tilo/windows/bridge"),
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
    /// privileged Tilo compatibility host; an unprivileged manager receives no
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

    fn pins_directory(&self) -> PathBuf {
        self.config.pin_directory.join(PINS_DIRECTORY)
    }

    fn enrolled_certificate(&self) -> Result<(Vec<u8>, String), AgentError> {
        let pins_directory = self.pins_directory();
        let certificate = read_private_file(
            &pins_directory.join(CERTIFICATE_FILE),
            MAX_CERTIFICATE_BYTES,
        )?;
        let expected = read_pin(&pins_directory.join(PIN_FILE))?;
        if !constant_time_eq(sha256_hex(&certificate).as_bytes(), expected.as_bytes()) {
            return Err(AgentError::Authentication);
        }
        Ok((certificate, expected))
    }

    fn require_enrolled_certificate(&self, digest: &str) -> Result<(), AgentError> {
        if !is_sha256(digest) {
            return Err(AgentError::Authentication);
        }
        let (_, enrolled_digest) = self.enrolled_certificate()?;
        if constant_time_eq(digest.as_bytes(), enrolled_digest.as_bytes()) {
            Ok(())
        } else {
            Err(AgentError::Authentication)
        }
    }

    fn pinned_client(&self) -> Result<Client, AgentError> {
        self.pinned_client_with_timeout(BRIDGE_TIMEOUT)
    }

    fn pinned_client_with_timeout(&self, timeout: Duration) -> Result<Client, AgentError> {
        let (certificate, _) = self.enrolled_certificate()?;
        let certificate = Certificate::from_der(&certificate)
            .map_err(|error| AgentError::Transport(format!("stored guest certificate: {error}")))?;
        tilo_net::http::blocking_client()
            .tls_backend_rustls()
            // Trust only the enrolled DER, never the system bundle. The
            // certificate is self-signed for its Tilo DNS name while the
            // host-only bridge has a fixed numeric endpoint, so hostname
            // checks are off; chain validation against that DER still applies.
            .tls_certs_only([certificate])
            .tls_danger_accept_invalid_hostnames(true)
            .connect_timeout(BRIDGE_TIMEOUT)
            .timeout(timeout)
            .build()
            .map_err(|error| AgentError::Transport(format!("TLS client: {error}")))
    }

    fn bootstrap_client() -> Result<Client, AgentError> {
        tilo_net::http::blocking_client()
            .tls_backend_rustls()
            // Only the first response is allowed to use this client. Its body
            // is verified with the root-only bootstrap HMAC before its DER is
            // persisted as the Rustls trust anchor.
            .tls_danger_accept_invalid_certs(true)
            .tls_danger_accept_invalid_hostnames(true)
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
        health.validate()?;
        let certificate = health.certificate_der()?;
        let digest = health.certificate_digest()?;
        let pins_directory = self.pins_directory();
        let pin_path = pins_directory.join(PIN_FILE);
        if pin_path.exists() && read_pin(&pin_path)? != digest {
            return Err(AgentError::Authentication);
        }
        write_private_new_or_equal(&pin_path, digest.as_bytes())?;
        write_private_new_or_equal(&pins_directory.join(CERTIFICATE_FILE), &certificate)?;
        self.pinned_client()
    }

    /// Streams one already-authorised portal file into the guest. Authentication
    /// binds the package's opaque identity, format, exact size and digest; the
    /// guest independently verifies all four before making the file visible.
    pub fn stage_installer<R>(
        &self,
        package: &InstallerPackage,
        reader: R,
    ) -> Result<StagedInstaller, AgentError>
    where
        R: Read + Send + 'static,
    {
        if package.byte_len == 0
            || package.byte_len > MAX_INSTALLER_BYTES
            || !valid_identifier(&package.package_id)
            || !is_sha256(&package.sha256)
        {
            return Err(AgentError::InvalidInstaller);
        }
        // Enrollment must complete with the short control-plane timeout. The
        // actual byte stream then uses a separately bounded transfer timeout.
        let _ = self.client()?;
        let client = self.pinned_client_with_timeout(INSTALLER_UPLOAD_TIMEOUT)?;
        let timestamp = now_unix()?;
        let request_nonce = nonce();
        let canonical = format!(
            "{timestamp}\n{request_nonce}\nUPLOAD\n{}\n{}\n{}\n{}\n",
            package.package_id,
            package.kind.wire_name(),
            package.byte_len,
            package.sha256
        );
        let authentication = hmac_hex(&self.bootstrap_secret, canonical.as_bytes(), &[]);
        let path = format!("/v1/installers/{}", package.package_id);
        let mut response = client
            .post(self.bridge_url(&path))
            .header("X-Tilo-Timestamp", timestamp.to_string())
            .header("X-Tilo-Nonce", &request_nonce)
            .header("X-Tilo-Auth", authentication)
            .header("X-Tilo-Installer-Kind", package.kind.wire_name())
            .header("X-Tilo-Payload-SHA256", &package.sha256)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", package.byte_len)
            .header("Accept", "application/json")
            .body(Body::sized(reader, package.byte_len))
            .send()
            .map_err(|error| AgentError::Transport(format!("guest installer upload: {error}")))?;
        if !response.status().is_success() {
            return Err(AgentError::Transport(format!(
                "guest bridge rejected installer with HTTP {}",
                response.status()
            )));
        }
        let response_auth = response
            .headers()
            .get("X-Tilo-Response-Auth")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
            .ok_or(AgentError::Authentication)?;
        let mut response_body = Vec::new();
        response
            .by_ref()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut response_body)
            .map_err(|error| AgentError::Transport(format!("guest bridge body: {error}")))?;
        if response_body.len() > usize::try_from(MAX_RESPONSE_BYTES).unwrap_or(usize::MAX) {
            return Err(AgentError::Transport(
                "guest bridge response was too large".to_string(),
            ));
        }
        response
            .extensions_mut()
            .insert(ResponseAuth(response_auth));
        response.extensions_mut().insert(RequestBinding {
            timestamp,
            nonce: request_nonce,
        });
        self.verify_response(&response, &response_body)?;
        let staged: WireStagedInstaller = deserialize(&response_body)?;
        if staged.package_id != package.package_id
            || staged.sha256 != package.sha256
            || staged.byte_len != package.byte_len
            || staged.expires_at_unix <= now_unix()?
            || staged.expires_at_unix > now_unix()?.saturating_add(MAX_INSTALLER_EXPIRY_SECONDS)
        {
            return Err(AgentError::Authentication);
        }
        Ok(StagedInstaller {
            package_id: staged.package_id,
            expires_at_unix: staged.expires_at_unix,
        })
    }

    /// Consumes a staged package exactly once and starts it in the appliance's
    /// existing managed interactive session. No command line crosses the wire.
    pub fn begin_installation(
        &self,
        staged: &StagedInstaller,
    ) -> Result<PreparedInstallation, AgentError> {
        if !valid_identifier(&staged.package_id) || staged.expires_at_unix <= now_unix()? {
            return Err(AgentError::InvalidInstaller);
        }
        let operation_id = nonce();
        let body = serde_json::to_vec(&WireBeginInstallation {
            package_id: &staged.package_id,
            operation_id: &operation_id,
        })
        .map_err(|error| AgentError::Transport(format!("installation request JSON: {error}")))?;
        let accepted: WireInstallationAccepted =
            deserialize(&self.request("POST", "/v1/installations", &body)?)?;
        self.require_enrolled_certificate(&accepted.certificate_sha256)?;
        accepted.into_prepared(&operation_id, &staged.package_id)
    }

    pub fn installation_status(
        &self,
        operation: &InstallationOperationId,
    ) -> Result<InstallationStatus, AgentError> {
        if !valid_identifier(operation.as_str()) {
            return Err(AgentError::InvalidInstaller);
        }
        let path = format!("/v1/installations/{}", operation.as_str());
        let status: WireInstallationStatus = deserialize(&self.request("GET", &path, &[])?)?;
        status.validate(operation)
    }

    /// Publishes only a candidate previously discovered by the SYSTEM bridge.
    /// Neither executable paths nor process arguments are accepted from Tilo.
    pub fn publish_candidate(
        &self,
        operation: &InstallationOperationId,
        candidate_id: &str,
        application_id: &str,
        display_name: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, AgentError> {
        if !valid_identifier(operation.as_str())
            || !valid_identifier(candidate_id)
            || !valid_identifier(application_id)
            || !valid_display_name(display_name)
        {
            return Err(AgentError::InvalidInstaller);
        }
        let body = serde_json::to_vec(&WirePublishRequest {
            operation_id: operation.as_str(),
            candidate_id,
            application_id,
            display_name,
            expected_generation,
        })
        .map_err(|error| AgentError::Transport(format!("publication request JSON: {error}")))?;
        let result: WirePublicationResult =
            deserialize(&self.request("POST", "/v1/publications", &body)?)?;
        result.validate(application_id, expected_generation)
    }

    pub fn remove_publication(
        &self,
        application_id: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, AgentError> {
        if !valid_identifier(application_id) {
            return Err(AgentError::InvalidInstaller);
        }
        let body = serde_json::to_vec(&WireRemovePublicationRequest {
            application_id,
            expected_generation,
        })
        .map_err(|error| AgentError::Transport(format!("removal request JSON: {error}")))?;
        let result: WirePublicationResult =
            deserialize(&self.request("POST", "/v1/publications/remove", &body)?)?;
        result.validate(application_id, expected_generation)
    }

    /// Starts only a previously discovered, administrator-stored uninstall
    /// record. The caller supplies no executable, product code, or arguments.
    pub fn begin_uninstallation(
        &self,
        application_id: &str,
    ) -> Result<PreparedUninstallation, AgentError> {
        if !valid_identifier(application_id) {
            return Err(AgentError::InvalidInstaller);
        }
        let operation_id = nonce();
        let body = serde_json::to_vec(&WireBeginUninstallation {
            application_id,
            operation_id: &operation_id,
        })
        .map_err(|error| AgentError::Transport(format!("uninstall request JSON: {error}")))?;
        let accepted: WireUninstallationAccepted =
            deserialize(&self.request("POST", "/v1/uninstallations", &body)?)?;
        self.require_enrolled_certificate(&accepted.certificate_sha256)?;
        accepted.into_prepared(&operation_id, application_id)
    }

    pub fn uninstallation_status(
        &self,
        operation: &UninstallationOperationId,
    ) -> Result<UninstallationStatus, AgentError> {
        if !valid_identifier(operation.as_str()) {
            return Err(AgentError::InvalidInstaller);
        }
        let path = format!("/v1/uninstallations/{}", operation.as_str());
        let status: WireUninstallationStatus = deserialize(&self.request("GET", &path, &[])?)?;
        status.validate(operation)
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
            .header("X-Tilo-Timestamp", timestamp.to_string())
            .header("X-Tilo-Nonce", &nonce)
            .header("X-Tilo-Auth", authentication)
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
            .get("X-Tilo-Response-Auth")
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
        self.require_enrolled_certificate(&health.certificate_digest()?)?;
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
        network: Option<&crate::GuestNetworkBinding>,
    ) -> Result<GuestLaunch, AgentError> {
        if session.id.len() < 16 || !valid_identifier(application_id) {
            return Err(AgentError::NotPublished(application_id.to_string()));
        }
        let request = WireLaunchRequest {
            application_id,
            launch_nonce: nonce(),
            network_proxy: network.map(WireNetworkProxy::from),
            presentation_backend: match self.config.presentation {
                ApplicationPresentation::NativeRail => "native_rail",
                ApplicationPresentation::AppFocusedSpice => "app_focused_spice",
            },
        };
        let body = serde_json::to_vec(&request)
            .map_err(|error| AgentError::Transport(format!("launch request JSON: {error}")))?;
        let launch: WireLaunch = deserialize(&self.request("POST", "/v1/launch", &body)?)?;
        self.require_enrolled_certificate(&launch.certificate_sha256)?;
        launch.into_guest_launch(application_id, self.config.presentation)
    }

    fn stage_installer<R>(
        &self,
        session: &GuestSession,
        package: &InstallerPackage,
        reader: R,
    ) -> Result<StagedInstaller, AgentError>
    where
        R: Read + Send + 'static,
    {
        require_session(session)?;
        HttpsGuestBridge::stage_installer(self, package, reader)
    }

    fn begin_installation(
        &self,
        session: &GuestSession,
        staged: &StagedInstaller,
    ) -> Result<PreparedInstallation, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::begin_installation(self, staged)
    }

    fn installation_status(
        &self,
        session: &GuestSession,
        operation: &InstallationOperationId,
    ) -> Result<InstallationStatus, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::installation_status(self, operation)
    }

    fn publish_candidate(
        &self,
        session: &GuestSession,
        operation: &InstallationOperationId,
        candidate_id: &str,
        application_id: &str,
        display_name: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::publish_candidate(
            self,
            operation,
            candidate_id,
            application_id,
            display_name,
            expected_generation,
        )
    }

    fn remove_publication(
        &self,
        session: &GuestSession,
        application_id: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::remove_publication(self, application_id, expected_generation)
    }

    fn begin_uninstallation(
        &self,
        session: &GuestSession,
        application_id: &str,
    ) -> Result<PreparedUninstallation, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::begin_uninstallation(self, application_id)
    }

    fn uninstallation_status(
        &self,
        session: &GuestSession,
        operation: &UninstallationOperationId,
    ) -> Result<UninstallationStatus, AgentError> {
        require_session(session)?;
        HttpsGuestBridge::uninstallation_status(self, operation)
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
struct WireStagedInstaller {
    package_id: String,
    byte_len: u64,
    sha256: String,
    expires_at_unix: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireBeginInstallation<'a> {
    package_id: &'a str,
    operation_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireInstallationAccepted {
    package_id: String,
    operation_id: String,
    endpoint: String,
    remote_app_alias: String,
    launch_nonce: String,
    certificate_sha256: String,
    presentation_backend: String,
}

impl WireInstallationAccepted {
    fn into_prepared(
        self,
        expected_operation: &str,
        expected_package: &str,
    ) -> Result<PreparedInstallation, AgentError> {
        if self.operation_id != expected_operation || self.package_id != expected_package {
            return Err(AgentError::Authentication);
        }
        let presentation = WireLaunch {
            endpoint: self.endpoint,
            username: None,
            password: None,
            remote_app_alias: self.remote_app_alias,
            launch_nonce: self.launch_nonce,
            certificate_sha256: self.certificate_sha256,
            presentation_backend: self.presentation_backend,
        }
        .into_guest_launch(expected_operation, ApplicationPresentation::AppFocusedSpice)?;
        Ok(PreparedInstallation {
            operation_id: InstallationOperationId(expected_operation.to_string()),
            presentation,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireInstallationStatus {
    operation_id: String,
    state: InstallationState,
    exit_code: Option<u32>,
    candidates: Vec<DiscoveredApplication>,
}

impl WireInstallationStatus {
    fn validate(
        self,
        expected: &InstallationOperationId,
    ) -> Result<InstallationStatus, AgentError> {
        if self.operation_id != expected.as_str()
            || self.candidates.len() > 512
            || self.candidates.iter().any(|candidate| !candidate.valid())
            || (matches!(
                self.state,
                InstallationState::Starting | InstallationState::Running
            ) && (self.exit_code.is_some() || !self.candidates.is_empty()))
            || (self.state == InstallationState::Completed && self.exit_code.is_none())
        {
            return Err(AgentError::Authentication);
        }
        Ok(InstallationStatus {
            operation_id: expected.clone(),
            state: self.state,
            exit_code: self.exit_code,
            candidates: self.candidates,
        })
    }
}

impl DiscoveredApplication {
    fn valid(&self) -> bool {
        valid_identifier(&self.id)
            && valid_display_name(&self.display_name)
            && self
                .product_name
                .as_deref()
                .is_none_or(valid_optional_metadata)
            && self
                .publisher
                .as_deref()
                .is_none_or(valid_optional_metadata)
            && self.version.as_deref().is_none_or(valid_optional_metadata)
            && self
                .uninstall
                .as_ref()
                .is_none_or(DiscoveredUninstaller::valid)
    }
}

impl DiscoveredUninstaller {
    fn valid(&self) -> bool {
        valid_identifier(&self.id)
            && valid_display_name(&self.display_name)
            && self
                .display_version
                .as_deref()
                .is_none_or(valid_optional_metadata)
            && self
                .publisher
                .as_deref()
                .is_none_or(valid_optional_metadata)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WirePublishRequest<'a> {
    operation_id: &'a str,
    candidate_id: &'a str,
    application_id: &'a str,
    display_name: &'a str,
    expected_generation: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRemovePublicationRequest<'a> {
    application_id: &'a str,
    expected_generation: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WirePublicationResult {
    application_id: String,
    catalog_generation: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireBeginUninstallation<'a> {
    application_id: &'a str,
    operation_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireUninstallationAccepted {
    application_id: String,
    operation_id: String,
    endpoint: String,
    remote_app_alias: String,
    launch_nonce: String,
    certificate_sha256: String,
    presentation_backend: String,
}

impl WireUninstallationAccepted {
    fn into_prepared(
        self,
        expected_operation: &str,
        expected_application: &str,
    ) -> Result<PreparedUninstallation, AgentError> {
        if self.operation_id != expected_operation || self.application_id != expected_application {
            return Err(AgentError::Authentication);
        }
        let presentation = WireLaunch {
            endpoint: self.endpoint,
            username: None,
            password: None,
            remote_app_alias: self.remote_app_alias,
            launch_nonce: self.launch_nonce,
            certificate_sha256: self.certificate_sha256,
            presentation_backend: self.presentation_backend,
        }
        .into_guest_launch(expected_operation, ApplicationPresentation::AppFocusedSpice)?;
        Ok(PreparedUninstallation {
            operation_id: UninstallationOperationId(expected_operation.to_string()),
            presentation,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireUninstallationStatus {
    operation_id: String,
    state: InstallationState,
    exit_code: Option<u32>,
    catalog_generation: Option<u64>,
}

impl WireUninstallationStatus {
    fn validate(
        self,
        expected: &UninstallationOperationId,
    ) -> Result<UninstallationStatus, AgentError> {
        let terminal_success = self.state == InstallationState::Completed;
        if self.operation_id != expected.as_str()
            || (matches!(
                self.state,
                InstallationState::Starting | InstallationState::Running
            ) && (self.exit_code.is_some() || self.catalog_generation.is_some()))
            || (terminal_success && (self.exit_code.is_none() || self.catalog_generation.is_none()))
            || (self.state == InstallationState::Failed && self.catalog_generation.is_some())
        {
            return Err(AgentError::Authentication);
        }
        Ok(UninstallationStatus {
            operation_id: expected.clone(),
            state: self.state,
            exit_code: self.exit_code,
            catalog_generation: self.catalog_generation,
        })
    }
}

impl WirePublicationResult {
    fn validate(
        self,
        expected_id: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, AgentError> {
        if self.application_id != expected_id
            || self.catalog_generation != expected_generation.saturating_add(1)
        {
            return Err(AgentError::Authentication);
        }
        Ok(PublicationResult {
            application_id: self.application_id,
            catalog_generation: self.catalog_generation,
        })
    }
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
        self.certificate_digest()?;
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

    fn certificate_digest(&self) -> Result<String, AgentError> {
        if !is_sha256(&self.certificate_sha256) {
            return Err(AgentError::Authentication);
        }
        let digest = sha256_hex(&self.certificate_der()?);
        if constant_time_eq(
            digest.as_bytes(),
            self.certificate_sha256.to_ascii_lowercase().as_bytes(),
        ) {
            Ok(digest)
        } else {
            Err(AgentError::Authentication)
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    network_proxy: Option<WireNetworkProxy>,
    presentation_backend: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireNetworkProxy {
    endpoint: String,
    token: String,
}

impl From<&crate::GuestNetworkBinding> for WireNetworkProxy {
    fn from(binding: &crate::GuestNetworkBinding) -> Self {
        Self {
            endpoint: binding.endpoint.to_string(),
            token: binding.token.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireLaunch {
    endpoint: String,
    /// Native RAIL returns a one-use RDP account. Focused SPICE attaches to
    /// the already-running singleton display and therefore returns no RDP
    /// credentials at all.
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
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
        let (username, password) = match presentation {
            ApplicationPresentation::NativeRail => {
                let username = self.username.ok_or(AgentError::Authentication)?;
                let password = self.password.ok_or(AgentError::Authentication)?;
                if !valid_username(&username) || password.is_empty() || password.len() > 512 {
                    return Err(AgentError::Authentication);
                }
                (username, password)
            }
            ApplicationPresentation::AppFocusedSpice => {
                // SPICE does not use the guest RDP listener. Rejecting
                // credentials here prevents an accidentally retained account
                // from becoming an unexpected second access path.
                if self.username.is_some() || self.password.is_some() {
                    return Err(AgentError::Authentication);
                }
                (String::new(), String::new())
            }
        };
        Ok(GuestLaunch {
            endpoint: RdpEndpoint {
                address: endpoint,
                username,
                certificate_sha256: self.certificate_sha256,
            },
            password: Secret::new(password.into_bytes()),
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
        || !private_file_authorized(&metadata)
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

fn private_file_authorized(metadata: &fs::Metadata) -> bool {
    let mode = metadata.mode() & 0o777;
    // SAFETY: these credential accessors have no preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if !matches!(metadata.uid(), 0) && metadata.uid() != effective_uid {
        return false;
    }
    if mode & 0o007 != 0 || mode & 0o030 != 0 {
        return false;
    }
    mode & 0o040 == 0 || process_has_group(metadata.gid())
}

fn process_has_group(group: libc::gid_t) -> bool {
    // SAFETY: `getegid` has no preconditions.
    if unsafe { libc::getegid() } == group {
        return true;
    }
    // SAFETY: a zero-sized query with a null pointer asks only for the count.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let Ok(count) = usize::try_from(count) else {
        return false;
    };
    let mut groups = vec![0 as libc::gid_t; count];
    // SAFETY: `groups` has exactly `count` writable gid slots.
    let filled = unsafe {
        libc::getgroups(
            i32::try_from(count).unwrap_or(i32::MAX),
            groups.as_mut_ptr(),
        )
    };
    usize::try_from(filled).is_ok_and(|filled| {
        filled == count && groups.into_iter().any(|candidate| candidate == group)
    })
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
    try_unix_now()
        .ok_or_else(|| AgentError::Transport("system clock precedes UNIX epoch".to_string()))
}

fn nonce() -> String {
    let mut bytes = [0_u8; 24];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
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

fn require_session(session: &GuestSession) -> Result<(), AgentError> {
    if valid_identifier(&session.id) && session.id.len() >= 16 {
        Ok(())
    } else {
        Err(AgentError::Authentication)
    }
}

fn valid_display_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.len() <= 256 && !trimmed.chars().any(char::is_control)
}

fn valid_optional_metadata(value: &str) -> bool {
    value.len() <= 256 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guest_bridge_is_fixed_hmac_bound_and_certificate_pinned() {
        // Phase `bridge_accepts_only_its_fixed_host_only_endpoint`.
        {
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
        // Phase `hmac_is_stable_and_body_bound`.
        {
            let key = [7_u8; 32];
            let first = hmac_hex(&key, b"7\nnonce\n", b"body");
            assert_eq!(first, hmac_hex(&key, b"7\nnonce\n", b"body"));
            assert_ne!(first, hmac_hex(&key, b"7\nnonce\n", b"different"));
            assert!(constant_time_eq(first.as_bytes(), first.as_bytes()));
        }
        // Phase `health_and_launch_certificates_must_match_the_enrolled_der_digest`.
        {
            let directory = tempfile::tempdir().unwrap();
            let secret = directory.path().join("bootstrap.secret");
            fs::write(&secret, [9_u8; 32]).unwrap();
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
            let bridge = HttpsGuestBridge::new(GuestBridgeConfig {
                endpoint: DEFAULT_ENDPOINT,
                bootstrap_secret: secret,
                pin_directory: directory.path().to_path_buf(),
                presentation: ApplicationPresentation::AppFocusedSpice,
            })
            .unwrap();

            let certificate = b"enrolled-certificate-der";
            let digest = sha256_hex(certificate);
            let pins = bridge.pins_directory();
            write_private_new_or_equal(&pins.join(PIN_FILE), digest.as_bytes()).unwrap();
            write_private_new_or_equal(&pins.join(CERTIFICATE_FILE), certificate).unwrap();

            let mut health = WireHealth {
                protocol_major: HOST_PROTOCOL_MAJOR,
                certificate_sha256: digest.clone(),
                certificate_der_base64: base64::engine::general_purpose::STANDARD
                    .encode(certificate),
                license: "licensed".to_string(),
                catalog_generation: 1,
                guest_boot_id: "boot-id".to_string(),
                presentation_backend: "app_focused_spice".to_string(),
                app_focused_spice_available: true,
                app_focused_spice_unavailable_reason: None,
            };
            health.validate().unwrap();
            bridge
                .require_enrolled_certificate(&health.certificate_digest().unwrap())
                .unwrap();
            bridge.require_enrolled_certificate(&digest).unwrap();

            health.certificate_sha256 = "a".repeat(64);
            assert!(matches!(
                health.certificate_digest(),
                Err(AgentError::Authentication)
            ));
            assert!(matches!(
                bridge.require_enrolled_certificate(&"b".repeat(64)),
                Err(AgentError::Authentication)
            ));
        }
        // Phase `owner_or_member_group_read_is_private_but_world_access_is_not`.
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = tempfile::NamedTempFile::new().unwrap();
            fs::set_permissions(file.path(), fs::Permissions::from_mode(0o640)).unwrap();
            assert!(private_file_authorized(&fs::metadata(file.path()).unwrap()));
            fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!private_file_authorized(
                &fs::metadata(file.path()).unwrap()
            ));
        }
        // Phase `enrollment_pins_do_not_change_shared_session_state_permissions`.
        {
            let session = tempfile::tempdir().unwrap();
            let state = session.path().join("state.json");
            fs::write(&state, b"{\"phase\":\"installing\"}").unwrap();
            fs::set_permissions(session.path(), fs::Permissions::from_mode(0o770)).unwrap();
            fs::set_permissions(&state, fs::Permissions::from_mode(0o660)).unwrap();

            let pins = session.path().join(PINS_DIRECTORY);
            let pin = vec![b'a'; 64];
            write_private_new_or_equal(&pins.join(PIN_FILE), &pin).unwrap();
            write_private_new_or_equal(&pins.join(CERTIFICATE_FILE), b"certificate").unwrap();

            assert_eq!(fs::metadata(session.path()).unwrap().mode() & 0o777, 0o770);
            assert_eq!(fs::read(&state).unwrap(), b"{\"phase\":\"installing\"}");
            assert_eq!(fs::metadata(&pins).unwrap().mode() & 0o777, 0o700);
            assert_eq!(
                fs::metadata(pins.join(PIN_FILE)).unwrap().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn guest_installation_status_is_bounded_digest_bound_and_generation_bound() {
        // Phase `installer_package_is_opaque_digest_bound_and_bounded`.
        {
            let package = InstallerPackage::new(InstallerKind::Msi, 4096, "a".repeat(64)).unwrap();
            assert!(valid_identifier(package.id()));
            assert_eq!(package.byte_len(), 4096);
            assert_eq!(package.sha256(), "a".repeat(64));
            assert!(InstallerPackage::new(InstallerKind::Exe, 0, "a".repeat(64)).is_err());
            assert!(
                InstallerPackage::new(InstallerKind::Exe, MAX_INSTALLER_BYTES + 1, "a".repeat(64))
                    .is_err()
            );
            assert!(InstallerPackage::new(InstallerKind::Exe, 1, "not-a-digest").is_err());
            assert!(InstallationOperationId::parse("too-short").is_err());
            assert!(InstallationOperationId::parse("a".repeat(48)).is_ok());
        }
        // Phase `installation_status_rejects_unbounded_or_early_candidates`.
        {
            let operation = InstallationOperationId("a".repeat(48));
            let candidate = DiscoveredApplication {
                id: "candidate.id".to_string(),
                display_name: "Candidate".to_string(),
                product_name: None,
                publisher: None,
                version: None,
                uninstall: None,
            };
            assert!(
                WireInstallationStatus {
                    operation_id: operation.as_str().to_string(),
                    state: InstallationState::Running,
                    exit_code: None,
                    candidates: vec![candidate.clone()],
                }
                .validate(&operation)
                .is_err()
            );
            assert!(
                WireInstallationStatus {
                    operation_id: operation.as_str().to_string(),
                    state: InstallationState::Completed,
                    exit_code: Some(0),
                    candidates: vec![candidate],
                }
                .validate(&operation)
                .is_ok()
            );
        }
        // Phase `installation_acceptance_can_only_prepare_app_focused_spice`.
        {
            let operation = "a".repeat(48);
            let accepted = WireInstallationAccepted {
                package_id: "b".repeat(48),
                operation_id: operation.clone(),
                endpoint: "192.168.251.2:3389".to_string(),
                remote_app_alias: operation.clone(),
                launch_nonce: "c".repeat(32),
                certificate_sha256: "d".repeat(64),
                presentation_backend: "app_focused_spice".to_string(),
            };
            let prepared = accepted.into_prepared(&operation, &"b".repeat(48)).unwrap();
            assert_eq!(prepared.operation_id.as_str(), operation);
            assert_eq!(
                prepared.presentation.presentation,
                ApplicationPresentation::AppFocusedSpice
            );
            assert!(prepared.presentation.endpoint.username.is_empty());
        }
        // Phase `uninstallation_status_binds_success_to_a_catalog_generation`.
        {
            let operation = UninstallationOperationId::parse("e".repeat(48)).unwrap();
            assert!(
                WireUninstallationStatus {
                    operation_id: operation.as_str().to_string(),
                    state: InstallationState::Completed,
                    exit_code: Some(0),
                    catalog_generation: None,
                }
                .validate(&operation)
                .is_err()
            );
            assert!(
                WireUninstallationStatus {
                    operation_id: operation.as_str().to_string(),
                    state: InstallationState::Completed,
                    exit_code: Some(0),
                    catalog_generation: Some(9),
                }
                .validate(&operation)
                .is_ok()
            );
        }
    }

    #[test]
    fn guest_launches_keep_the_selected_presentation_and_its_credentials() {
        // Phase `launch_rejects_a_guest_that_changes_selected_presentation`.
        {
            let launch = WireLaunch {
                endpoint: "192.168.251.2:3389".to_string(),
                username: Some("qra123".to_string()),
                password: Some("secret".to_string()),
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
        // Phase `focused_spice_launch_has_no_rdp_credentials`.
        {
            let launch = WireLaunch {
                endpoint: "192.168.251.2:3389".to_string(),
                username: None,
                password: None,
                remote_app_alias: "published.app".to_string(),
                launch_nonce: "0123456789abcdef".to_string(),
                certificate_sha256: "a".repeat(64),
                presentation_backend: "app_focused_spice".to_string(),
            };
            let launch = launch
                .into_guest_launch("published.app", ApplicationPresentation::AppFocusedSpice)
                .expect("focused SPICE does not require an RDP account");
            assert!(launch.endpoint.username.is_empty());
            assert!(launch.password.expose().is_empty());
        }
        // Phase `focused_spice_rejects_rdp_credentials_if_guest_sends_them`.
        {
            let launch = WireLaunch {
                endpoint: "192.168.251.2:3389".to_string(),
                username: Some("unexpected-account".to_string()),
                password: Some("unexpected-password".to_string()),
                remote_app_alias: "published.app".to_string(),
                launch_nonce: "0123456789abcdef".to_string(),
                certificate_sha256: "a".repeat(64),
                presentation_backend: "app_focused_spice".to_string(),
            };
            assert!(
                launch
                    .into_guest_launch("published.app", ApplicationPresentation::AppFocusedSpice)
                    .is_err()
            );
        }
        // Phase `native_rail_launch_still_requires_rdp_credentials`.
        {
            let launch = WireLaunch {
                endpoint: "192.168.251.2:3389".to_string(),
                username: None,
                password: None,
                remote_app_alias: "published.app".to_string(),
                launch_nonce: "0123456789abcdef".to_string(),
                certificate_sha256: "a".repeat(64),
                presentation_backend: "native_rail_rds".to_string(),
            };
            assert!(
                launch
                    .into_guest_launch("published.app", ApplicationPresentation::NativeRail)
                    .is_err()
            );
        }
    }
}
