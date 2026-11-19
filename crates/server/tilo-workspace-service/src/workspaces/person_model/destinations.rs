//! Where a workspace allows its content to be sent when asking a model.
//!
//! A workspace without a rule permits any provider a person linked (a person's
//! own workspace stays unrestricted unless they set a rule). Once a workspace
//! sets a [`DestinationPolicy`], every model call working in that workspace is
//! checked against it before any request or vault key is used.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use agent_provider_config::{EndpointLocation, ProviderConfig, ProviderProfile};
use serde::{Deserialize, Serialize};
use tilo_lock::RwLockRecover as _;
use tilo_server_protocol::WorkspaceId;

/// Directory under the service's data root holding per-workspace destination
/// policies, one JSON file per workspace id.
const DIRECTORY: &str = "workspace-destinations";

/// Where a workspace lets its content go when a model is asked on behalf of a
/// member working in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DestinationPolicy {
    /// No workspace restriction: any provider the person linked and this
    /// server can reach may answer.
    #[default]
    Any,
    /// Nothing may leave this server: only loopback endpoints
    /// ([`EndpointLocation::ThisDevice`]) are permitted.
    NoneOutsideServer,
    /// Only destinations matching at least one listed rule are permitted. An
    /// empty list refuses every provider.
    Only(Vec<AllowedDestination>),
}

/// One destination a workspace allows its content to be sent to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowedDestination {
    /// Models served locally on this server (`EndpointLocation::ThisDevice`).
    ThisServer,
    /// Models on the operator's private network (`EndpointLocation::PrivateNetwork`).
    PrivateNetwork,
    /// A specific provider identified by profile id or display label
    /// (case-insensitive).
    Provider(String),
    /// A specific endpoint origin, URL prefix, or domain/IP (for example
    /// `"api.anthropic.com"` or `"https://api.anthropic.com"`), matched
    /// against the parsed endpoint URL.
    Endpoint(String),
}

impl AllowedDestination {
    /// Whether `profile` satisfies this destination rule.
    ///
    /// Fails closed if `profile.endpoint` cannot be parsed as a valid URL.
    #[must_use]
    pub fn matches(&self, profile: &ProviderProfile) -> bool {
        let Some(location) = EndpointLocation::of_endpoint(&profile.endpoint) else {
            return false;
        };
        match self {
            Self::ThisServer => location == EndpointLocation::ThisDevice,
            Self::PrivateNetwork => location == EndpointLocation::PrivateNetwork,
            Self::Provider(name) => {
                let wanted = name.trim();
                !wanted.is_empty()
                    && (profile.id.eq_ignore_ascii_case(wanted)
                        || profile.display_label().eq_ignore_ascii_case(wanted))
            }
            Self::Endpoint(pattern) => endpoint_matches(&profile.endpoint, pattern, location),
        }
    }

    /// A short human-readable description of this allowed destination, for
    /// refusals naming what can continue.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::ThisServer => "this server".to_owned(),
            Self::PrivateNetwork => "private network".to_owned(),
            Self::Provider(name) | Self::Endpoint(name) => name.trim().to_owned(),
        }
    }
}

impl DestinationPolicy {
    /// Whether `profile` is allowed to receive content from a workspace with
    /// this policy.
    #[must_use]
    pub fn permits(&self, profile: &ProviderProfile) -> bool {
        match self {
            Self::Any => true,
            Self::NoneOutsideServer => {
                EndpointLocation::of_endpoint(&profile.endpoint)
                    == Some(EndpointLocation::ThisDevice)
            }
            Self::Only(allowed) => allowed.iter().any(|rule| rule.matches(profile)),
        }
    }

    /// Actionable alternatives that satisfy this policy: first any enabled,
    /// reachable provider the person already linked that this policy permits,
    /// followed by any policy destination not yet covered by a linked
    /// provider.
    #[must_use]
    pub(super) fn allowed_alternatives(
        &self,
        config: &ProviderConfig,
        reachable: impl Fn(&ProviderProfile) -> bool,
    ) -> Vec<String> {
        let mut alternatives = Vec::new();
        let mut linked_allowed: Vec<&ProviderProfile> = Vec::new();
        for profile in config
            .profiles
            .iter()
            .filter(|profile| profile.enabled && reachable(profile) && self.permits(profile))
        {
            linked_allowed.push(profile);
            push_unique(&mut alternatives, profile.display_label().to_owned());
        }
        match self {
            Self::Any => {}
            Self::NoneOutsideServer => {
                if linked_allowed.is_empty() {
                    push_unique(&mut alternatives, "this server".to_owned());
                }
            }
            Self::Only(allowed) => {
                for rule in allowed {
                    if !linked_allowed.iter().any(|profile| rule.matches(profile)) {
                        let label = rule.describe();
                        if !label.is_empty() {
                            push_unique(&mut alternatives, label);
                        }
                    }
                }
            }
        }
        alternatives
    }
}

/// Sanitize `endpoint` to its scheme and network origin (`scheme://domain[:port]`)
/// so refusals name the blocked destination without leaking query strings,
/// credentials, or paths.
#[must_use]
pub(super) fn sanitize_destination(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    match tilo_net::Url::parse(trimmed) {
        Ok(url) => {
            if let Some(domain) = normalized_domain(&url) {
                return match url.port() {
                    Some(port) => format!("{}://{domain}:{port}", url.scheme()),
                    None => format!("{}://{domain}", url.scheme()),
                };
            }
        }
        Err(error) => {
            tracing::warn!(%error, "provider endpoint is not a valid URL");
        }
    }
    trimmed
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn push_unique(items: &mut Vec<String>, candidate: String) {
    if !items
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&candidate))
    {
        items.push(candidate);
    }
}

fn normalized_domain(url: &tilo_net::Url) -> Option<String> {
    url.host_str()
        .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
}

fn endpoint_matches(endpoint: &str, pattern: &str, location: EndpointLocation) -> bool {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Ok(actual) = tilo_net::Url::parse(endpoint.trim()) else {
        return false;
    };
    let Some(actual_domain) = normalized_domain(&actual) else {
        return false;
    };
    if trimmed.contains("://") {
        let Ok(expected) = tilo_net::Url::parse(trimmed) else {
            return false;
        };
        let Some(expected_domain) = normalized_domain(&expected) else {
            return false;
        };
        if actual.scheme() != expected.scheme()
            || actual_domain != expected_domain
            || actual.port_or_known_default() != expected.port_or_known_default()
        {
            return false;
        }
        let expected_path = expected.path().trim_end_matches('/');
        if expected_path.is_empty() {
            return true;
        }
        let actual_path = actual.path().trim_end_matches('/');
        actual_path == expected_path
            || actual_path
                .strip_prefix(expected_path)
                .is_some_and(|rest| rest.starts_with('/'))
    } else {
        let Ok(expected) = tilo_net::Url::parse(&format!("https://{trimmed}")) else {
            return false;
        };
        let Some(expected_domain) = normalized_domain(&expected) else {
            return false;
        };
        if actual_domain != expected_domain {
            return false;
        }
        if let Some(port) = expected.port()
            && actual.port_or_known_default() != Some(port)
        {
            return false;
        }
        // A bare domain rule for an internet endpoint requires TLS so a
        // plaintext downgrade cannot satisfy an internet destination rule.
        if location == EndpointLocation::Hosted && actual.scheme() != "https" {
            return false;
        }
        true
    }
}

/// Per-workspace destination policies kept on disk and cached in memory.
#[derive(Default)]
pub(super) struct Store {
    policies: RwLock<BTreeMap<WorkspaceId, DestinationPolicy>>,
}

impl Store {
    /// Set `policy` for `workspace`, persisting it atomically under `dir`.
    /// Setting [`DestinationPolicy::Any`] clears any stored restriction.
    pub(super) fn set(
        &self,
        dir: &Path,
        workspace: WorkspaceId,
        policy: DestinationPolicy,
    ) -> io::Result<()> {
        let mut policies = self.policies.write_recover();
        let path = policy_path(dir, &workspace);
        if policy == DestinationPolicy::Any {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            policies.remove(&workspace);
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(&policy).map_err(io::Error::other)?;
        tilo_files::native::atomic::write(&path, &bytes)?;
        policies.insert(workspace, policy);
        Ok(())
    }

    /// Read the active policy for `workspace`.
    ///
    /// Returns [`DestinationPolicy::Any`] only when no policy file exists on
    /// disk. Any I/O or parse failure on an existing policy file returns `Err`
    /// so callers fail closed rather than leaking workspace content.
    pub(super) fn get(&self, dir: &Path, workspace: &WorkspaceId) -> io::Result<DestinationPolicy> {
        if let Some(cached) = self.policies.read_recover().get(workspace).cloned() {
            return Ok(cached);
        }
        let path = policy_path(dir, workspace);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DestinationPolicy::Any);
            }
            Err(error) => return Err(error),
        };
        let policy: DestinationPolicy = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.policies
            .write_recover()
            .insert(*workspace, policy.clone());
        Ok(policy)
    }
}

fn policy_path(dir: &Path, workspace: &WorkspaceId) -> PathBuf {
    dir.join(DIRECTORY).join(format!("{workspace}.json"))
}
