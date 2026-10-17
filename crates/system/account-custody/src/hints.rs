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

/// Provider identity without account credentials.
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
// JSON can double every permitted byte through quote/backslash escaping.
const MAX_PLAINTEXT_BYTES: usize = MAX_HINTS * (MAX_FIELD_BYTES * 4 + 256) + 64;
const MAX_SEALED_BYTES: usize = MAX_PLAINTEXT_BYTES + 56;
const DOMAIN: &[u8] = b"tilo-installer/online-account-hints/v1\0";

/// Deliberately contains neither plaintext nor a source identifier, and has no
/// serialization or debug implementation. Parsing does not authenticate it.
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
    ///
    /// # Errors
    /// Rejects oversized, malformed, or non-raw-key envelopes.
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

// Local wire types keep serde off the form's provider and decrypted hint types.
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

/// Encrypt suggestions under a local key and the explicit selection generation.
///
/// # Errors
/// Rejects invalid source bindings or hints, and reports encryption failures.
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

/// Authenticate before parsing; reject the whole payload rather than truncating
/// or filtering it. Unknown/duplicate fields and trailing JSON are rejected.
///
/// # Errors
/// Rejects invalid source bindings, failed authentication, or invalid hint payloads.
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
    /// # Errors
    /// Rejects malformed, oversized or non-password envelopes.
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

/// Seal the canonical hint wire format for a later live session.
/// # Errors
/// Rejects short passwords, invalid bindings or hints, and encryption failures.
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

/// Authenticate offline suggestions before presenting them for live sign-in.
/// # Errors
/// Rejects invalid bindings, authentication failures or malformed hints.
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

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "selected-device/read-7";

    fn hint() -> AccountHint {
        AccountHint {
            provider: Provider::Nextcloud,
            email: "private-person@example.org".into(),
            server: "https://private-cloud.example.org".into(),
            sources: HintSources::default(),
        }
    }

    fn raw(key: &SymmetricKey, plaintext: &[u8]) -> SealedHints {
        let binding = binding(SOURCE).unwrap();
        SealedHints::from_bytes(
            &envelope::seal(
                key,
                Context::new(Label::MigrationPackage, &binding),
                plaintext,
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn legacy_hints_default_to_unknown_services_and_typed_sources_round_trip() {
        let key = SymmetricKey::generate().unwrap();
        let legacy = raw(
            &key,
            br#"{"hints":[{"provider":"google","email":"person@example.org","server":""}]}"#,
        );
        assert_eq!(
            open(&key, SOURCE, &legacy).unwrap()[0].sources,
            HintSources::default()
        );
        let mut hint = hint();
        hint.sources = HintSources {
            onedrive: true,
            outlook: true,
            google_web: false,
        };
        let sealed = seal(&key, SOURCE, &[hint.clone()]).unwrap();
        assert_eq!(
            open(&key, SOURCE, &sealed).unwrap()[0].sources,
            hint.sources
        );
        let unknown = raw(&key, br#"{"hints":[{"provider":"google","email":"a","server":"","sources":{"token":"secret"}}]}"#);
        assert!(open(&key, SOURCE, &unknown).is_err());
    }

    #[test]
    fn grouping_combines_provenance_not_aliases_providers_or_unidentified_profiles() {
        let mut files = hint();
        files.sources.onedrive = true;
        let mut mail = files.clone();
        mail.sources = HintSources {
            outlook: true,
            ..Default::default()
        };
        let mut alias = files.clone();
        alias.email = "private-person+work@example.org".into();
        let mut other_provider = files.clone();
        other_provider.provider = Provider::Google;
        let mut other_server = files.clone();
        other_server.server = "https://other.example.org".into();
        let mut unknown = files.clone();
        unknown.email.clear();
        let grouped = group_hints(vec![
            files,
            mail,
            alias,
            other_provider,
            other_server,
            unknown.clone(),
            unknown,
        ]);
        assert_eq!(grouped.len(), 6);
        assert!(grouped[0].sources.onedrive && grouped[0].sources.outlook);
    }

    #[test]
    fn password_media_round_trip_keeps_suggestions_private_and_bound() {
        let sealed = seal_password(b"private-password", SOURCE, &[hint()]).unwrap();
        assert!(
            !sealed
                .as_bytes()
                .windows(hint().email.len())
                .any(|bytes| bytes == hint().email.as_bytes())
        );
        let parsed = PasswordHints::from_bytes(sealed.as_bytes()).unwrap();
        assert_eq!(
            open_password(b"private-password", SOURCE, &parsed).unwrap()[0].email,
            hint().email
        );
        assert!(open_password(b"wrong-password", SOURCE, &parsed).is_err());
        assert!(open_password(b"private-password", "another-generation", &parsed).is_err());
        assert!(SealedHints::from_bytes(sealed.as_bytes()).is_err());
    }

    #[test]
    fn private_round_trip_is_bound_to_key_source_and_domain() {
        let key = SymmetricKey::generate().unwrap();
        let sealed = seal(&key, SOURCE, &[hint()]).unwrap();
        for private in [hint().email, hint().server, SOURCE.into()] {
            assert!(
                !sealed
                    .as_bytes()
                    .windows(private.len())
                    .any(|w| w == private.as_bytes())
            );
        }
        let copied = SealedHints::from_bytes(sealed.as_bytes()).unwrap();
        let hints = open(&key, SOURCE, &copied).unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].provider, Provider::Nextcloud);
        assert_eq!(hints[0].email, hint().email);
        assert_eq!(hints[0].server, hint().server);
        assert!(open(&key, "selected-device/read-8", &sealed).is_err());
        assert!(open(&SymmetricKey::generate().unwrap(), SOURCE, &sealed).is_err());
        assert!(
            envelope::open(
                &key,
                Context::new(Label::MigrationPackage, SOURCE.as_bytes()),
                sealed.as_bytes(),
            )
            .is_err()
        );
        assert_ne!(
            sealed.as_bytes(),
            seal(&key, SOURCE, &[hint()]).unwrap().as_bytes()
        );
        let mut tampered = sealed.as_bytes().to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open(&key, SOURCE, &SealedHints::from_bytes(&tampered).unwrap()).is_err());
    }

    #[test]
    fn malformed_and_non_hint_payloads_are_rejected() {
        let key = SymmetricKey::generate().unwrap();
        for plaintext in [
            br#"{"hints":[],"token":"secret"}"#.as_slice(),
            br#"{"hints":[{"provider":"google","email":"a","server":"","password":"secret"}]}"#,
            br#"{"hints":[],"hints":[]}"#,
            br#"{"hints":[]} {}"#,
            br#"{"hints":[{"provider":"unknown","email":"a","server":""}]}"#,
            br#"{"hints":[{"provider":"google","email":"a\nb","server":""}]}"#,
            b"not json",
        ] {
            assert!(open(&key, SOURCE, &raw(&key, plaintext)).is_err());
        }
        assert!(SealedHints::from_bytes(b"plaintext").is_err());
        assert!(SealedHints::from_bytes(&vec![0; MAX_SEALED_BYTES + 1]).is_err());
        assert!(seal(&key, "", &[]).is_err());
    }

    #[test]
    fn capacities_apply_to_both_sealing_and_authenticated_input() {
        let key = SymmetricKey::generate().unwrap();
        let mut hints = vec![hint(); MAX_HINTS];
        hints[0].email = "x".repeat(MAX_FIELD_BYTES);
        let sealed = seal(&key, SOURCE, &hints).unwrap();
        assert_eq!(open(&key, SOURCE, &sealed).unwrap().len(), MAX_HINTS);
        hints.push(hint());
        assert!(seal(&key, SOURCE, &hints).is_err());
        let wire = WireHints {
            hints: hints
                .iter()
                .map(|h| WireHint {
                    provider: h.provider.into(),
                    email: h.email.clone(),
                    server: h.server.clone(),
                    sources: HintSources::default(),
                })
                .collect(),
        };
        assert!(
            open(
                &key,
                SOURCE,
                &raw(&key, &serde_json::to_vec(&wire).unwrap())
            )
            .is_err()
        );
        for (email, server) in [
            ("x".repeat(MAX_FIELD_BYTES + 1), String::new()),
            ("control\u{7f}".into(), String::new()),
            (String::new(), "https://user:password@example.org".into()),
            (String::new(), "https://example.org/?token=secret".into()),
            (String::new(), "x".repeat(MAX_FIELD_BYTES + 1)),
        ] {
            let wire = WireHints {
                hints: vec![WireHint {
                    provider: WireProvider::Google,
                    email: email.clone(),
                    server: server.clone(),
                    sources: HintSources::default(),
                }],
            };
            assert!(
                open(
                    &key,
                    SOURCE,
                    &raw(&key, &serde_json::to_vec(&wire).unwrap())
                )
                .is_err()
            );
            assert!(
                seal(
                    &key,
                    SOURCE,
                    &[AccountHint {
                        provider: Provider::Google,
                        email,
                        server,
                        sources: HintSources::default(),
                    }]
                )
                .is_err()
            );
        }
    }
}
