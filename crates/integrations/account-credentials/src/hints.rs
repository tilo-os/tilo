//! Encrypted, device-private suggestions from an explicitly selected source.
//!
//! The caller supplies a fresh source binding for every selection/read, including
//! re-selection of the same source. Nothing here authorizes an account or stores
//! hints in the shared install profile. Only identity, endpoint and closed source
//! metadata cross this boundary; credentials and authorization have no wire field.

pub mod media;

use serde::{Deserialize, Serialize};
use tilo_seal::{Context, Label, SymmetricKey, envelope};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Provider {
    #[default]
    Google,
    Microsoft,
    Dropbox,
    Nextcloud,
}

/// Private suggestion. Never serialize this type into public setup state.
#[derive(Clone)]
pub struct AccountHint {
    pub provider: Provider,
    pub email: String,
    pub server: String,
    pub sources: HintSources,
}

/// Bounded provenance, not scopes, consent or evidence of a current login.
/// An unclassified web account must never imply Calendar authorization.
#[derive(Clone, Copy, Default, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HintSources {
    pub onedrive: bool,
    pub outlook: bool,
    pub google_web: bool,
}

impl HintSources {
    pub fn merge(&mut self, other: Self) {
        self.onedrive |= other.onedrive;
        self.outlook |= other.outlook;
        self.google_web |= other.google_web;
    }
}

/// Merge only exact nonempty identities under the same provider and endpoint.
/// Do not collapse aliases, personal/work addresses or unidentified profiles.
#[must_use]
pub fn group_hints(hints: Vec<AccountHint>) -> Vec<AccountHint> {
    let mut grouped: Vec<AccountHint> = Vec::new();
    for hint in hints {
        if let Some(previous) = grouped.iter_mut().find(|previous| {
            !hint.email.trim().is_empty()
                && previous.provider == hint.provider
                && previous.email == hint.email
                && previous.server == hint.server
        }) {
            previous.sources.merge(hint.sources);
        } else {
            grouped.push(hint);
        }
    }
    grouped
}

const MAX_HINTS: usize = 32;
const MAX_FIELD_BYTES: usize = 2048;
const MAX_PLAINTEXT_BYTES: usize = MAX_HINTS * (MAX_FIELD_BYTES * 4 + 256) + 64;
const MAX_SEALED_BYTES: usize = MAX_PLAINTEXT_BYTES + 56;
const DOMAIN: &[u8] = b"tilo-installer/online-account-hints/v1\0";

#[derive(Clone)]
pub struct SealedHints(Vec<u8>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HintsError {
    InvalidBinding,
    InvalidHints,
    InvalidEnvelope,
    Authentication,
    Sealing,
}

impl std::fmt::Display for HintsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidBinding => "invalid account hint source binding",
            Self::InvalidHints => "invalid account hints",
            Self::InvalidEnvelope => "invalid encrypted account hints",
            Self::Authentication => "account hints could not be authenticated",
            Self::Sealing => "account hints could not be sealed",
        })
    }
}

impl std::error::Error for HintsError {}

impl SealedHints {
    /// Accept a bounded canonical raw-key envelope, without trusting its contents.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HintsError> {
        if bytes.len() > MAX_SEALED_BYTES {
            return Err(HintsError::InvalidEnvelope);
        }
        let parsed = envelope::Envelope::parse(bytes).map_err(|_| HintsError::InvalidEnvelope)?;
        if !matches!(parsed.source, envelope::KeySource::Key(_)) {
            return Err(HintsError::InvalidEnvelope);
        }
        Ok(Self(bytes.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHints {
    hints: Vec<WireHint>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHint {
    provider: WireProvider,
    email: String,
    server: String,
    #[serde(default)]
    sources: HintSources,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireProvider {
    Google,
    Microsoft,
    Dropbox,
    Nextcloud,
}

impl From<Provider> for WireProvider {
    fn from(provider: Provider) -> Self {
        match provider {
            Provider::Google => Self::Google,
            Provider::Microsoft => Self::Microsoft,
            Provider::Dropbox => Self::Dropbox,
            Provider::Nextcloud => Self::Nextcloud,
        }
    }
}

impl From<WireProvider> for Provider {
    fn from(provider: WireProvider) -> Self {
        match provider {
            WireProvider::Google => Self::Google,
            WireProvider::Microsoft => Self::Microsoft,
            WireProvider::Dropbox => Self::Dropbox,
            WireProvider::Nextcloud => Self::Nextcloud,
        }
    }
}

fn binding(source: &str) -> Result<Vec<u8>, HintsError> {
    if source.is_empty() || source.len() > MAX_FIELD_BYTES || source.chars().any(char::is_control) {
        return Err(HintsError::InvalidBinding);
    }
    let mut binding = DOMAIN.to_vec();
    binding.extend_from_slice(source.as_bytes());
    Ok(binding)
}

fn valid_fields(email: &str, server: &str) -> bool {
    if [email, server]
        .iter()
        .any(|field| field.len() > MAX_FIELD_BYTES || field.chars().any(char::is_control))
    {
        return false;
    }
    // The server is an endpoint, never an authorization URL or credential store.
    server.is_empty()
        || url::Url::parse(server).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
}

pub fn seal(
    key: &SymmetricKey,
    source: &str,
    hints: &[AccountHint],
) -> Result<SealedHints, HintsError> {
    let binding = binding(source)?;
    if hints.len() > MAX_HINTS
        || hints
            .iter()
            .any(|hint| !valid_fields(&hint.email, &hint.server))
    {
        return Err(HintsError::InvalidHints);
    }
    let wire = WireHints {
        hints: hints
            .iter()
            .map(|hint| WireHint {
                provider: hint.provider.into(),
                email: hint.email.clone(),
                server: hint.server.clone(),
                sources: hint.sources,
            })
            .collect(),
    };
    let plaintext =
        Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| HintsError::InvalidHints)?);
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(HintsError::InvalidHints);
    }
    envelope::seal(
        key,
        Context::new(Label::MigrationPackage, &binding),
        &plaintext,
    )
    .map(SealedHints)
    .map_err(|_| HintsError::Sealing)
}

pub fn open(
    key: &SymmetricKey,
    source: &str,
    sealed: &SealedHints,
) -> Result<Vec<AccountHint>, HintsError> {
    let binding = binding(source)?;
    let plaintext = envelope::open(
        key,
        Context::new(Label::MigrationPackage, &binding),
        sealed.as_bytes(),
    )
    .map_err(|_| HintsError::Authentication)?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(HintsError::InvalidHints);
    }
    let wire: WireHints =
        serde_json::from_slice(&plaintext).map_err(|_| HintsError::InvalidHints)?;
    if wire.hints.len() > MAX_HINTS
        || wire
            .hints
            .iter()
            .any(|hint| !valid_fields(&hint.email, &hint.server))
    {
        return Err(HintsError::InvalidHints);
    }
    Ok(wire
        .hints
        .into_iter()
        .map(|hint| AccountHint {
            provider: hint.provider.into(),
            email: hint.email,
            server: hint.server,
            sources: hint.sources,
        })
        .collect())
}

/// Password-encrypted suggestions for offline media. No credentials or account
/// authorization are carried. The source binding must be supplied independently.
pub struct PasswordHints(Vec<u8>);
impl PasswordHints {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Parse a bounded password envelope without trusting its contents.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HintsError> {
        if bytes.len() > MAX_SEALED_BYTES + 256 {
            return Err(HintsError::InvalidEnvelope);
        }
        let parsed = envelope::Envelope::parse(bytes).map_err(|_| HintsError::InvalidEnvelope)?;
        if !matches!(parsed.source, envelope::KeySource::Password { .. }) {
            return Err(HintsError::InvalidEnvelope);
        }
        Ok(Self(bytes.to_vec()))
    }
}

pub fn seal_password(
    password: &[u8],
    source: &str,
    hints: &[AccountHint],
) -> Result<PasswordHints, HintsError> {
    if password.len() < 8 {
        return Err(HintsError::InvalidHints);
    }
    let binding = binding(source)?;
    if hints.len() > MAX_HINTS
        || hints
            .iter()
            .any(|hint| !valid_fields(&hint.email, &hint.server))
    {
        return Err(HintsError::InvalidHints);
    }
    let wire = WireHints {
        hints: hints
            .iter()
            .map(|hint| WireHint {
                provider: hint.provider.into(),
                email: hint.email.clone(),
                server: hint.server.clone(),
                sources: hint.sources,
            })
            .collect(),
    };
    let plaintext =
        Zeroizing::new(serde_json::to_vec(&wire).map_err(|_| HintsError::InvalidHints)?);
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(HintsError::InvalidHints);
    }

    envelope::seal_with_password(
        password,
        tilo_seal::PasswordParams::default(),
        Context::new(Label::MigrationPackage, &binding),
        &plaintext,
    )
    .map(PasswordHints)
    .map_err(|_| HintsError::Sealing)
}

pub fn open_password(
    password: &[u8],
    source: &str,
    sealed: &PasswordHints,
) -> Result<Vec<AccountHint>, HintsError> {
    let binding = binding(source)?;
    let plaintext = envelope::open_with_password(
        password,
        Context::new(Label::MigrationPackage, &binding),
        sealed.as_bytes(),
    )
    .map_err(|_| HintsError::Authentication)?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(HintsError::InvalidHints);
    }
    let wire: WireHints =
        serde_json::from_slice(&plaintext).map_err(|_| HintsError::InvalidHints)?;
    if wire.hints.len() > MAX_HINTS
        || wire
            .hints
            .iter()
            .any(|hint| !valid_fields(&hint.email, &hint.server))
    {
        return Err(HintsError::InvalidHints);
    }
    Ok(wire
        .hints
        .into_iter()
        .map(|hint| AccountHint {
            provider: hint.provider.into(),
            email: hint.email,
            server: hint.server,
            sources: hint.sources,
        })
        .collect())
}
