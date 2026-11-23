//! Shared wire types and validation for prepared setup receipts and sealed
//! transfer expectations.
//!
//! Both the prepared-setup helper (producer) and the onboarding session
//! (consumer) serialize and deserialize these exact declarations. Every wire
//! struct refuses unknown fields.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(feature = "receipt")]
use sha2::{Digest, Sha256};
#[cfg(feature = "receipt")]
use std::collections::BTreeSet;
use std::{fmt::Debug, io};

/// Plaintext chunk size used by the sealed portable archive codec.
pub const CHUNK: usize = 1024 * 1024;
const CHUNK_BYTES: u64 = 1024 * 1024;

/// Maximum logical browser profile data carried in one browser transfer.
pub const BROWSER_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum archive plaintext bytes carried in one browser transfer.
pub const BROWSER_MAX_ARCHIVE_BYTES: u64 = BROWSER_MAX_BYTES + 1024 * 1024;
/// Maximum archive entries carried in one browser transfer.
pub const BROWSER_MAX_ENTRIES: usize = 128;
/// Maximum archive metadata bytes carried in one browser transfer.
pub const BROWSER_MAX_METADATA: usize = 1024 * 1024;

#[cfg(feature = "receipt")]
const MAX_RECEIPT_ACCOUNTS: usize = 64;
#[cfg(feature = "receipt")]
const MAX_RECEIPT_MAIL: usize = 64;
#[cfg(feature = "receipt")]
const MAX_SERVICE_SESSIONS_BYTES: u64 = 8 * 1024 * 1024 + 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_installer_binding(value: &str) -> bool {
    (1..=256).contains(&value.chars().count()) && !value.chars().any(|c| c < ' ' || c == '\u{7f}')
}

/// Canonical lowercase non-nil RFC 4122 GUID (`8-4-4-4-12` hex digits).
#[must_use]
pub fn is_operation_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}

/// Purpose recorded in a sealed portable archive's authenticated header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Home,
    PreparedSetup,
    PreparationAccounts,
    FlatpakApplications,
    BrowserProfiles,
}

/// Bounds must come from the explicitly allocated medium/storage, not an estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub metadata_bytes: usize,
}

impl Limits {
    /// Validate that every bound is positive, within hard ceilings, and cannot overflow framing.
    pub fn validate(self) -> io::Result<()> {
        if self.plaintext_bytes == 0
            || self.entries == 0
            || self.entries > 1_000_000
            || self.metadata_bytes == 0
            || self.metadata_bytes > 512 * 1024 * 1024
        {
            return Err(invalid("Portable transfer review bounds are invalid"));
        }
        self.sealed_bound()
            .ok_or_else(|| invalid("Portable transfer allocation overflows"))?;
        Ok(())
    }

    /// Upper bound on ciphertext bytes for an archive within these plaintext limits.
    #[must_use]
    pub fn sealed_bound(self) -> Option<u64> {
        self.plaintext_bytes.checked_add(2048)?.checked_add(
            self.plaintext_bytes
                .div_ceil(CHUNK_BYTES)
                .checked_mul(256)?,
        )
    }
}

/// A kind of data carried as one sealed unit.
pub trait UnitKind: Clone + Debug + PartialEq + Eq + Serialize + DeserializeOwned {
    const PURPOSE: Purpose;
    /// The unit's file is `{PREFIX}-{operation}.sealed`.
    const PREFIX: &'static str;
    /// Refusal of an expectation that breaks any bound.
    const INVALID: &'static str;

    fn operation_valid(operation: &str) -> bool {
        is_operation_id(operation)
    }

    /// Bounds of this kind beyond the shared ones.
    fn within_bounds(expectation: &Expectation<Self>) -> bool;
}

/// What a destination admits: one expectation per sealed unit. The shared
/// fields and bounds are checked once here; each kind adds only its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound = "")]
pub struct Expectation<K: UnitKind> {
    pub operation: String,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub ciphertext_sha256: [u8; 32],
    pub limits: Limits,
    pub kind: K,
}

impl<K: UnitKind> Expectation<K> {
    /// The unit's file name for `operation`.
    pub fn filename(operation: &str) -> io::Result<String> {
        if !K::operation_valid(operation) {
            return Err(invalid(K::INVALID));
        }
        Ok(format!("{}-{operation}.sealed", K::PREFIX))
    }

    #[must_use]
    pub fn purpose(&self) -> Purpose {
        K::PURPOSE
    }

    fn validate_with_binding(&self, binding_ok: impl FnOnce(&str) -> bool) -> io::Result<()> {
        if Self::filename(&self.operation).ok().as_deref() != Some(self.filename.as_str())
            || !binding_ok(&self.setup_binding)
            || self.sealed_bytes == 0
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > self.limits.plaintext_bytes
            || self.entries == 0
            || self.entries > self.limits.entries
            || self.limits.entries > 1_000_000
            || self.limits.metadata_bytes == 0
            || self.limits.metadata_bytes > 512 * 1024 * 1024
            || self
                .limits
                .sealed_bound()
                .is_none_or(|bound| self.sealed_bytes > bound)
            || !K::within_bounds(self)
        {
            return Err(invalid(K::INVALID));
        }
        Ok(())
    }

    /// Refuse malformed operations, bindings, and sizes beyond the unit's bounds.
    pub fn validate(&self) -> io::Result<()> {
        self.validate_with_binding(valid_binding)
    }

    /// Installer-extractor validation: preserves the extractor's 1..=256 character
    /// and ASCII-control binding rule alongside all shared bounds.
    pub fn validate_installer(&self) -> io::Result<()> {
        self.validate_with_binding(valid_installer_binding)
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}

/// A sealed Home unit carries the reviewed tree, with sockets recreated empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Home {
    pub runtime_sockets: u64,
}

impl UnitKind for Home {
    const PURPOSE: Purpose = Purpose::Home;
    const PREFIX: &'static str = "home";
    const INVALID: &'static str =
        "Portable home expectation has invalid bounds or operation binding";

    fn operation_valid(operation: &str) -> bool {
        !operation.is_empty()
            && operation.len() <= 96
            && operation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }

    fn within_bounds(expectation: &HomeExpectation) -> bool {
        u64::try_from(expectation.limits.entries)
            .is_ok_and(|entries| expectation.kind.runtime_sockets <= entries)
    }
}

/// Sealed home transfer expectation.
pub type HomeExpectation = Expectation<Home>;

/// A sealed applications unit: the signed system apps and their closure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applications {
    pub consumer_protocol: u32,
    pub manifest_sha256: [u8; 32],
}

impl UnitKind for Applications {
    const PURPOSE: Purpose = Purpose::FlatpakApplications;
    const PREFIX: &'static str = "applications";
    const INVALID: &'static str = "Invalid portable application expectation";

    fn within_bounds(expectation: &ApplicationsExpectation) -> bool {
        matches!(expectation.kind.consumer_protocol, 1 | 2)
    }
}

/// Sealed portable applications transfer expectation.
pub type ApplicationsExpectation = Expectation<Applications>;

/// A sealed browser unit: the authenticated plan's profiles, within fixed bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Browser {
    pub plan_sha256: [u8; 32],
}

impl UnitKind for Browser {
    const PURPOSE: Purpose = Purpose::BrowserProfiles;
    const PREFIX: &'static str = "browser";
    const INVALID: &'static str = "Invalid browser transfer expectation";

    fn within_bounds(expectation: &BrowserExpectation) -> bool {
        expectation.limits.plaintext_bytes <= BROWSER_MAX_ARCHIVE_BYTES
            && expectation.limits.entries <= BROWSER_MAX_ENTRIES
            && expectation.limits.metadata_bytes <= BROWSER_MAX_METADATA
    }
}

/// Sealed browser profiles transfer expectation.
pub type BrowserExpectation = Expectation<Browser>;

#[cfg(feature = "receipt")]
impl BrowserExpectation {
    /// Refuse a different setup, identity, selection or authenticated plan digest.
    pub fn validate_plan(&self, plan: &browser_continuity::Plan) -> io::Result<()> {
        self.validate()?;
        let bytes = plan.encode().map_err(io::Error::other)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if plan.profiles.is_empty()
            || plan.setup_binding != self.setup_binding
            || digest != self.kind.plan_sha256
        {
            return Err(invalid(
                "Browser transfer differs from its authenticated plan",
            ));
        }
        Ok(())
    }
}

/// Selected browser data authenticated with the same identity and setup as installation.
#[cfg(feature = "receipt")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHandoff {
    pub plan: browser_continuity::Plan,
    pub payload: BrowserExpectation,
}

#[cfg(feature = "receipt")]
impl BrowserHandoff {
    /// Refuse a malformed expectation or a plan that differs from its authenticated digest.
    pub fn validate(&self) -> io::Result<()> {
        self.payload.validate_plan(&self.plan)
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}

/// Portable home selection written into the prepared-setup manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableHomePlan {
    pub schema: u32,
    pub selection: PortableHomeSelection,
    #[serde(default)]
    pub applications: bool,
}

/// Scope of a portable home transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortableHomeSelection {
    FullHome,
}

/// Portable home declaration emitted in the prepared-setup helper receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableHomeDeclaration {
    pub schema: u32,
    pub setup_binding: String,
    pub selection: PortableHomeSelection,
    #[serde(default)]
    pub applications: bool,
}

impl PortableHomeDeclaration {
    /// Refuse unsupported schemas or malformed setup bindings.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1 || !valid_binding(&self.setup_binding) {
            return Err(invalid("Invalid portable home declaration"));
        }
        Ok(())
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}

#[cfg(feature = "receipt")]
const fn default_consumer_protocol() -> u32 {
    1
}

/// Public ciphertext-only declaration, authenticated by the prepared setup manifest.
#[cfg(feature = "receipt")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSessionsDeclaration {
    pub schema: u32,
    #[serde(
        default = "default_consumer_protocol",
        skip_serializing_if = "is_default_consumer_protocol"
    )]
    pub consumer_protocol: u32,
    pub sessions: Vec<SelectedSession>,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: [u8; 32],
}

#[cfg(feature = "receipt")]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if passes a reference to the field"
)]
const fn is_default_consumer_protocol(value: &u32) -> bool {
    *value == 1
}

/// One selected website session in [`ServiceSessionsDeclaration`].
#[cfg(feature = "receipt")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedSession {
    pub id: String,
    pub service: browser_continuity::sessions::Service,
}

#[cfg(feature = "receipt")]
impl ServiceSessionsDeclaration {
    /// Refuse unsupported declarations, duplicate IDs/services or oversized ciphertext.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || !matches!(self.consumer_protocol, 1 | 2)
            || self.sessions.is_empty()
            || self.sessions.len() > 2
            || self.sealed_bytes == 0
            || self.sealed_bytes > MAX_SERVICE_SESSIONS_BYTES
        {
            return Err(invalid("Selected website session is unavailable"));
        }
        let mut ids = BTreeSet::new();
        let mut services = BTreeSet::new();
        for session in &self.sessions {
            if !browser_continuity::sessions::valid_id(&session.id)
                || !ids.insert(&session.id)
                || !services.insert(session.service.as_str())
            {
                return Err(invalid("Selected website session is unavailable"));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}

/// What the destination received from a Thunderbird mail import, in opaque ids and counts;
/// never a path, address or content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdImportReceipt {
    pub accounts: Vec<ThunderbirdAccountReceipt>,
    pub address_books: usize,
    pub calendars: usize,
}

/// One imported Thunderbird account summary in [`ThunderbirdImportReceipt`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdAccountReceipt {
    pub source_id: String,
    pub created: bool,
    pub needs_reconnect: bool,
    pub folders: Vec<ThunderbirdFolderReceipt>,
}

/// One imported Thunderbird folder summary in [`ThunderbirdAccountReceipt`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdFolderReceipt {
    pub source_id: String,
    pub messages: u64,
    pub bytes: u64,
    pub local_only: bool,
}

/// Receipt written by `tilo-prepared-setup` on stdout and parsed by the onboarding session.
///
/// Generic over the current-device comparison declaration `C` so this crate stays
/// independent of `tilo-identity`; both producer and consumer instantiate it with
/// `tilo_identity::enrolled_transport::CurrentDeviceDeclaration`.
#[cfg(feature = "receipt")]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "C: Serialize", deserialize = "C: Deserialize<'de>")
)]
pub struct PreparedSetupReceipt<C> {
    pub identity_public_key: [u8; 32],
    pub account_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_identity: Option<C>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_sessions: Option<ServiceSessionsDeclaration>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_home: Option<PortableHomeDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_flatpak: Option<ApplicationsExpectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserHandoff>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mail: Vec<ThunderbirdImportReceipt>,
}

#[cfg(feature = "receipt")]
impl<C> PreparedSetupReceipt<C> {
    /// Validate every carried declaration's bounds and identity binding.
    pub fn validate(&self) -> io::Result<()> {
        if self.account_count > MAX_RECEIPT_ACCOUNTS || self.mail.len() > MAX_RECEIPT_MAIL {
            return Err(invalid("Prepared setup receipt exceeds bounds"));
        }
        if let Some(sessions) = &self.service_sessions {
            sessions.validate()?;
        }
        if let Some(home) = &self.portable_home {
            home.validate()?;
        }
        if let Some(apps) = &self.portable_flatpak {
            apps.validate()?;
        }
        if let Some(browser) = &self.browser {
            browser.validate()?;
            if browser.plan.identity_public_key != self.identity_public_key {
                return Err(invalid("Browser plan identity differs from setup receipt"));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
    }
}

/// Read-only deserializers for files already written to a person's disk by
/// earlier builds (2026-10-12 through 2026-10-28, including `mine-tip`) as well
/// as the present nested `kind` shape. Every accepted shape stays closed under
/// `deny_unknown_fields`, and writers always emit the present nested shape.
pub mod stored {
    use super::{
        Applications, ApplicationsExpectation, Browser, BrowserExpectation, Home, HomeExpectation,
        Limits, Purpose,
    };
    use serde::{Deserialize, Deserializer, de};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FlatHome {
        schema: u32,
        operation: String,
        purpose: Purpose,
        filename: String,
        setup_binding: String,
        sealed_bytes: u64,
        plaintext_bytes: u64,
        entries: usize,
        runtime_sockets: u64,
        ciphertext_sha256: [u8; 32],
        limits: Limits,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FlatApplications {
        schema: u32,
        operation: String,
        purpose: Purpose,
        filename: String,
        setup_binding: String,
        sealed_bytes: u64,
        plaintext_bytes: u64,
        entries: usize,
        ciphertext_sha256: [u8; 32],
        manifest_sha256: [u8; 32],
        limits: Limits,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FlatBrowser {
        schema: u32,
        operation: String,
        purpose: Purpose,
        filename: String,
        setup_binding: String,
        sealed_bytes: u64,
        plaintext_bytes: u64,
        entries: usize,
        ciphertext_sha256: [u8; 32],
        plan_sha256: [u8; 32],
        limits: Limits,
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HomeShape {
        Current(HomeExpectation),
        Flat(FlatHome),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ApplicationsShape {
        Current(ApplicationsExpectation),
        Flat(FlatApplications),
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BrowserShape {
        Current(BrowserExpectation),
        Flat(FlatBrowser),
    }

    /// Read-only parser accepting both the present nested and earlier flat
    /// on-disk `HomeExpectation` shapes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StoredHomeExpectation(pub HomeExpectation);

    impl<'de> Deserialize<'de> for StoredHomeExpectation {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            match HomeShape::deserialize(deserializer)? {
                HomeShape::Current(current) => Ok(Self(current)),
                HomeShape::Flat(flat) => {
                    if flat.schema != 1 || flat.purpose != Purpose::Home {
                        return Err(de::Error::custom("unsupported home expectation schema"));
                    }
                    Ok(Self(HomeExpectation {
                        operation: flat.operation,
                        filename: flat.filename,
                        setup_binding: flat.setup_binding,
                        sealed_bytes: flat.sealed_bytes,
                        plaintext_bytes: flat.plaintext_bytes,
                        entries: flat.entries,
                        ciphertext_sha256: flat.ciphertext_sha256,
                        limits: flat.limits,
                        kind: Home {
                            runtime_sockets: flat.runtime_sockets,
                        },
                    }))
                }
            }
        }
    }

    /// Read-only parser accepting both the present nested and earlier flat
    /// on-disk `ApplicationsExpectation` shapes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StoredApplicationsExpectation(pub ApplicationsExpectation);

    impl<'de> Deserialize<'de> for StoredApplicationsExpectation {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            match ApplicationsShape::deserialize(deserializer)? {
                ApplicationsShape::Current(current) => Ok(Self(current)),
                ApplicationsShape::Flat(flat) => {
                    if flat.schema != 1 || flat.purpose != Purpose::FlatpakApplications {
                        return Err(de::Error::custom(
                            "unsupported applications expectation schema",
                        ));
                    }
                    Ok(Self(ApplicationsExpectation {
                        operation: flat.operation,
                        filename: flat.filename,
                        setup_binding: flat.setup_binding,
                        sealed_bytes: flat.sealed_bytes,
                        plaintext_bytes: flat.plaintext_bytes,
                        entries: flat.entries,
                        ciphertext_sha256: flat.ciphertext_sha256,
                        limits: flat.limits,
                        kind: Applications {
                            consumer_protocol: 1,
                            manifest_sha256: flat.manifest_sha256,
                        },
                    }))
                }
            }
        }
    }

    /// Read-only parser accepting both the present nested and earlier flat
    /// on-disk `BrowserExpectation` shapes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StoredBrowserExpectation(pub BrowserExpectation);

    impl<'de> Deserialize<'de> for StoredBrowserExpectation {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            match BrowserShape::deserialize(deserializer)? {
                BrowserShape::Current(current) => Ok(Self(current)),
                BrowserShape::Flat(flat) => {
                    if flat.schema != 1 || flat.purpose != Purpose::BrowserProfiles {
                        return Err(de::Error::custom("unsupported browser expectation schema"));
                    }
                    Ok(Self(BrowserExpectation {
                        operation: flat.operation,
                        filename: flat.filename,
                        setup_binding: flat.setup_binding,
                        sealed_bytes: flat.sealed_bytes,
                        plaintext_bytes: flat.plaintext_bytes,
                        entries: flat.entries,
                        ciphertext_sha256: flat.ciphertext_sha256,
                        limits: flat.limits,
                        kind: Browser {
                            plan_sha256: flat.plan_sha256,
                        },
                    }))
                }
            }
        }
    }

    #[cfg(feature = "receipt")]
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredBrowserFields {
        plan: browser_continuity::Plan,
        payload: StoredBrowserExpectation,
    }

    /// Read-only parser accepting a stored browser declaration with either a
    /// nested or earlier flat `payload`.
    #[cfg(feature = "receipt")]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StoredBrowserDeclaration(pub super::BrowserHandoff);

    #[cfg(feature = "receipt")]
    impl<'de> Deserialize<'de> for StoredBrowserDeclaration {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let fields = StoredBrowserFields::deserialize(deserializer)?;
            Ok(Self(super::BrowserHandoff {
                plan: fields.plan,
                payload: fields.payload.0,
            }))
        }
    }

    /// `serde(deserialize_with)` adapter for an optional stored
    /// `ApplicationsExpectation`.
    pub fn deserialize_option_applications<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ApplicationsExpectation>, D::Error> {
        Option::<StoredApplicationsExpectation>::deserialize(deserializer)
            .map(|value| value.map(|stored| stored.0))
    }

    /// `serde(deserialize_with)` adapter for an optional stored browser
    /// declaration.
    #[cfg(feature = "receipt")]
    pub fn deserialize_option_browser<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<super::BrowserHandoff>, D::Error> {
        Option::<StoredBrowserDeclaration>::deserialize(deserializer)
            .map(|value| value.map(|stored| stored.0))
    }

    #[cfg(feature = "receipt")]
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, bound(deserialize = "C: Deserialize<'de>"))]
    struct StoredPreparedSetupReceiptFields<C> {
        identity_public_key: [u8; 32],
        account_count: usize,
        #[serde(default)]
        current_identity: Option<C>,
        #[serde(default)]
        service_sessions: Option<super::ServiceSessionsDeclaration>,
        #[serde(default)]
        device_label: String,
        #[serde(default)]
        portable_home: Option<super::PortableHomeDeclaration>,
        #[serde(default, deserialize_with = "deserialize_option_applications")]
        portable_flatpak: Option<ApplicationsExpectation>,
        #[serde(default, deserialize_with = "deserialize_option_browser")]
        browser: Option<super::BrowserHandoff>,
        #[serde(default)]
        mail: Vec<super::ThunderbirdImportReceipt>,
    }

    /// Read-only parser for `tilo-prepared-setup` receipts accepting both the
    /// present nested `kind` shape and the earlier flat shape for
    /// `portable_flatpak` and `browser.payload`.
    #[cfg(feature = "receipt")]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct StoredPreparedSetupReceipt<C>(pub super::PreparedSetupReceipt<C>);

    #[cfg(feature = "receipt")]
    impl<'de, C: Deserialize<'de>> Deserialize<'de> for StoredPreparedSetupReceipt<C> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let fields = StoredPreparedSetupReceiptFields::<C>::deserialize(deserializer)?;
            Ok(Self(super::PreparedSetupReceipt {
                identity_public_key: fields.identity_public_key,
                account_count: fields.account_count,
                current_identity: fields.current_identity,
                service_sessions: fields.service_sessions,
                device_label: fields.device_label,
                portable_home: fields.portable_home,
                portable_flatpak: fields.portable_flatpak,
                browser: fields.browser,
                mail: fields.mail,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_OP: &str = "11111111-1111-4111-8111-111111111111";
    const BROWSER_OP: &str = "22222222-2222-4222-8222-222222222222";

    fn sample_limits() -> Limits {
        Limits {
            plaintext_bytes: 1_048_576,
            entries: 64,
            metadata_bytes: 65_536,
        }
    }

    fn sample_home_expectation() -> HomeExpectation {
        HomeExpectation {
            operation: "home_op-1".into(),
            filename: HomeExpectation::filename("home_op-1").unwrap(),
            setup_binding: "setup-binding".into(),
            sealed_bytes: 512,
            plaintext_bytes: 256,
            entries: 4,
            ciphertext_sha256: [1; 32],
            limits: sample_limits(),
            kind: Home { runtime_sockets: 1 },
        }
    }

    fn sample_applications_expectation(protocol: u32) -> ApplicationsExpectation {
        ApplicationsExpectation {
            operation: APP_OP.into(),
            filename: ApplicationsExpectation::filename(APP_OP).unwrap(),
            setup_binding: "setup-binding".into(),
            sealed_bytes: 512,
            plaintext_bytes: 256,
            entries: 4,
            ciphertext_sha256: [4; 32],
            limits: sample_limits(),
            kind: Applications {
                consumer_protocol: protocol,
                manifest_sha256: [5; 32],
            },
        }
    }

    fn sample_browser_expectation(plan_sha256: [u8; 32]) -> BrowserExpectation {
        BrowserExpectation {
            operation: BROWSER_OP.into(),
            filename: BrowserExpectation::filename(BROWSER_OP).unwrap(),
            setup_binding: "setup-binding".into(),
            sealed_bytes: 512,
            plaintext_bytes: 256,
            entries: 2,
            ciphertext_sha256: [6; 32],
            limits: sample_limits(),
            kind: Browser { plan_sha256 },
        }
    }

    #[cfg(feature = "receipt")]
    fn sample_browser_declaration(identity: [u8; 32]) -> BrowserHandoff {
        let plan = browser_continuity::Plan::new(
            "setup-binding".into(),
            identity,
            vec![
                browser_continuity::Profile::new(
                    "a".repeat(64),
                    browser_continuity::Provider::Chromium,
                    vec!["browser".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let plan_sha256: [u8; 32] = Sha256::digest(plan.encode().unwrap()).into();
        BrowserHandoff {
            plan,
            payload: sample_browser_expectation(plan_sha256),
        }
    }

    #[cfg(feature = "receipt")]
    fn sample_service_sessions(protocol: u32) -> ServiceSessionsDeclaration {
        ServiceSessionsDeclaration {
            schema: 1,
            consumer_protocol: protocol,
            sessions: vec![SelectedSession {
                id: "discord-session".into(),
                service: browser_continuity::sessions::Service::Discord,
            }],
            sealed_bytes: 1024,
            ciphertext_sha256: [3; 32],
        }
    }

    fn sample_mail_receipt() -> ThunderbirdImportReceipt {
        ThunderbirdImportReceipt {
            accounts: vec![ThunderbirdAccountReceipt {
                source_id: "account-1".into(),
                created: true,
                needs_reconnect: false,
                folders: vec![ThunderbirdFolderReceipt {
                    source_id: "folder-1".into(),
                    messages: 12,
                    bytes: 4096,
                    local_only: false,
                }],
            }],
            address_books: 1,
            calendars: 2,
        }
    }

    #[cfg(feature = "receipt")]
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StubCurrentDevice {
        schema: u32,
        record_name: String,
    }

    fn assert_refuses_unknown<T: Serialize + DeserializeOwned>(value: &T) {
        let mut json = serde_json::to_value(value).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("unknown_field".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<T>(json).is_err());
    }

    #[test]
    fn sealed_expectations_round_trip_and_refuse_unknown_fields() {
        let home = sample_home_expectation();
        home.validate().unwrap();
        let home_json = serde_json::to_value(&home).unwrap();
        assert!(home_json.get("schema").is_none());
        assert!(home_json.get("purpose").is_none());
        assert_eq!(home_json["kind"]["runtime_sockets"], 1);
        assert_eq!(
            serde_json::from_value::<HomeExpectation>(home_json).unwrap(),
            home
        );
        assert_refuses_unknown(&home);
        assert_refuses_unknown(&home.kind);
        assert_refuses_unknown(&home.limits);

        for protocol in [1, 2] {
            let apps = sample_applications_expectation(protocol);
            apps.validate().unwrap();
            let apps_json = serde_json::to_value(&apps).unwrap();
            assert!(apps_json.get("schema").is_none());
            assert!(apps_json.get("purpose").is_none());
            assert_eq!(apps_json["kind"]["consumer_protocol"], protocol);
            assert_eq!(
                serde_json::from_value::<ApplicationsExpectation>(apps_json).unwrap(),
                apps
            );
            assert_refuses_unknown(&apps);
            assert_refuses_unknown(&apps.kind);
        }

        let browser = sample_browser_expectation([9; 32]);
        browser.validate().unwrap();
        let browser_json = serde_json::to_value(&browser).unwrap();
        assert!(browser_json.get("schema").is_none());
        assert!(browser_json.get("purpose").is_none());
        assert_eq!(
            browser_json["kind"]["plan_sha256"],
            serde_json::json!(browser.kind.plan_sha256)
        );
        assert_eq!(
            serde_json::from_value::<BrowserExpectation>(browser_json).unwrap(),
            browser
        );
        assert_refuses_unknown(&browser);
        assert_refuses_unknown(&browser.kind);
    }

    #[test]
    fn stored_expectation_readers_accept_nested_and_flat_and_refuse_unknown_fields() {
        let home = sample_home_expectation();
        assert_eq!(
            serde_json::from_value::<stored::StoredHomeExpectation>(
                serde_json::to_value(&home).unwrap()
            )
            .unwrap()
            .0,
            home
        );
        let mut flat_home = serde_json::json!({
            "schema": 1,
            "operation": home.operation,
            "purpose": "home",
            "filename": home.filename,
            "setup_binding": home.setup_binding,
            "sealed_bytes": home.sealed_bytes,
            "plaintext_bytes": home.plaintext_bytes,
            "entries": home.entries,
            "runtime_sockets": home.kind.runtime_sockets,
            "ciphertext_sha256": home.ciphertext_sha256,
            "limits": home.limits,
        });
        assert_eq!(
            serde_json::from_value::<stored::StoredHomeExpectation>(flat_home.clone())
                .unwrap()
                .0,
            home
        );
        flat_home["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<stored::StoredHomeExpectation>(flat_home).is_err());

        let apps = sample_applications_expectation(1);
        let mut flat_apps = serde_json::json!({
            "schema": 1,
            "operation": apps.operation,
            "purpose": "flatpak_applications",
            "filename": apps.filename,
            "setup_binding": apps.setup_binding,
            "sealed_bytes": apps.sealed_bytes,
            "plaintext_bytes": apps.plaintext_bytes,
            "entries": apps.entries,
            "ciphertext_sha256": apps.ciphertext_sha256,
            "manifest_sha256": apps.kind.manifest_sha256,
            "limits": apps.limits,
        });
        assert_eq!(
            serde_json::from_value::<stored::StoredApplicationsExpectation>(flat_apps.clone())
                .unwrap()
                .0,
            apps
        );
        flat_apps["schema"] = serde_json::json!(2);
        assert!(
            serde_json::from_value::<stored::StoredApplicationsExpectation>(flat_apps.clone())
                .is_err()
        );
        flat_apps["schema"] = serde_json::json!(1);
        flat_apps["purpose"] = serde_json::json!("home");
        assert!(
            serde_json::from_value::<stored::StoredApplicationsExpectation>(flat_apps.clone())
                .is_err()
        );
        flat_apps["purpose"] = serde_json::json!("flatpak_applications");
        for protocol in [1, 2] {
            let mut with_protocol = flat_apps.clone();
            with_protocol["consumer_protocol"] = serde_json::json!(protocol);
            assert!(
                serde_json::from_value::<stored::StoredApplicationsExpectation>(with_protocol)
                    .is_err(),
                "flat applications must refuse consumer_protocol={protocol}"
            );
        }
        flat_apps["extra"] = serde_json::json!(1);
        assert!(
            serde_json::from_value::<stored::StoredApplicationsExpectation>(flat_apps).is_err()
        );

        let browser = sample_browser_expectation([9; 32]);
        let mut flat_browser = serde_json::json!({
            "schema": 1,
            "operation": browser.operation,
            "purpose": "browser_profiles",
            "filename": browser.filename,
            "setup_binding": browser.setup_binding,
            "sealed_bytes": browser.sealed_bytes,
            "plaintext_bytes": browser.plaintext_bytes,
            "entries": browser.entries,
            "ciphertext_sha256": browser.ciphertext_sha256,
            "plan_sha256": browser.kind.plan_sha256,
            "limits": browser.limits,
        });
        assert_eq!(
            serde_json::from_value::<stored::StoredBrowserExpectation>(flat_browser.clone())
                .unwrap()
                .0,
            browser
        );
        flat_browser["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<stored::StoredBrowserExpectation>(flat_browser).is_err());
    }

    #[test]
    fn session_and_installer_binding_rules_preserve_distinct_boundaries() {
        let mut home = sample_home_expectation();
        for (binding, session_ok, installer_ok) in [
            ("é".repeat(128), true, true),
            ("é".repeat(256), false, true),
            ("é".repeat(257), false, false),
            ("before\u{85}after".to_owned(), false, true),
            ("before\u{7f}after".to_owned(), false, false),
            ("before\x1fafter".to_owned(), false, false),
        ] {
            home.setup_binding = binding.clone();
            assert_eq!(
                home.validate().is_ok(),
                session_ok,
                "session binding rule for len={} chars={}",
                binding.len(),
                binding.chars().count()
            );
            assert_eq!(
                home.validate_installer().is_ok(),
                installer_ok,
                "installer binding rule for len={} chars={}",
                binding.len(),
                binding.chars().count()
            );
        }
    }

    #[test]
    fn home_and_mail_round_trip_and_refuse_unknown_fields() {
        let home_plan = PortableHomePlan {
            schema: 1,
            selection: PortableHomeSelection::FullHome,
            applications: true,
        };
        let home_plan_json = serde_json::to_value(&home_plan).unwrap();
        assert_eq!(home_plan_json["selection"], "full_home");
        assert_eq!(
            serde_json::from_value::<PortableHomePlan>(home_plan_json).unwrap(),
            home_plan
        );
        assert_refuses_unknown(&home_plan);

        let portable_home = PortableHomeDeclaration {
            schema: 1,
            setup_binding: "setup-binding".into(),
            selection: PortableHomeSelection::FullHome,
            applications: true,
        };
        portable_home.validate().unwrap();
        let portable_home_json = serde_json::to_value(&portable_home).unwrap();
        assert_eq!(portable_home_json["selection"], "full_home");
        assert_eq!(
            serde_json::from_value::<PortableHomeDeclaration>(portable_home_json).unwrap(),
            portable_home
        );
        assert_refuses_unknown(&portable_home);

        let mail = sample_mail_receipt();
        let mail_json = serde_json::to_value(&mail).unwrap();
        assert_eq!(
            serde_json::from_value::<ThunderbirdImportReceipt>(mail_json).unwrap(),
            mail
        );
        assert_refuses_unknown(&mail);
        assert_refuses_unknown(&mail.accounts[0]);
        assert_refuses_unknown(&mail.accounts[0].folders[0]);
    }

    #[cfg(feature = "receipt")]
    #[test]
    fn sessions_and_browser_declarations_round_trip_and_refuse_unknown_fields() {
        let sessions_v1 = sample_service_sessions(1);
        sessions_v1.validate().unwrap();
        let v1_json = serde_json::to_value(&sessions_v1).unwrap();
        assert!(
            v1_json.get("consumer_protocol").is_none(),
            "protocol 1 must stay omitted on the wire"
        );
        assert_eq!(
            serde_json::from_value::<ServiceSessionsDeclaration>(v1_json).unwrap(),
            sessions_v1
        );

        let sessions_v2 = sample_service_sessions(2);
        sessions_v2.validate().unwrap();
        let v2_json = serde_json::to_value(&sessions_v2).unwrap();
        assert_eq!(v2_json["consumer_protocol"], 2);
        assert_eq!(
            serde_json::from_value::<ServiceSessionsDeclaration>(v2_json).unwrap(),
            sessions_v2
        );
        assert_refuses_unknown(&sessions_v2);
        assert_refuses_unknown(&sessions_v2.sessions[0]);

        let key = [7u8; 32];
        let browser = sample_browser_declaration(key);
        browser.validate().unwrap();
        let browser_json = serde_json::to_value(&browser).unwrap();
        assert!(browser_json["payload"].get("schema").is_none());
        assert!(browser_json["payload"].get("purpose").is_none());
        assert_eq!(
            serde_json::from_value::<BrowserHandoff>(browser_json).unwrap(),
            browser
        );
        assert_refuses_unknown(&browser);
    }

    #[cfg(feature = "receipt")]
    #[test]
    fn prepared_setup_receipt_round_trips_and_refuses_unknown_fields() {
        let key = [7u8; 32];
        let receipt = PreparedSetupReceipt {
            identity_public_key: key,
            account_count: 1,
            current_identity: Some(StubCurrentDevice {
                schema: 1,
                record_name: "primary.json".into(),
            }),
            service_sessions: Some(sample_service_sessions(2)),
            device_label: "Ada's laptop".into(),
            portable_home: Some(PortableHomeDeclaration {
                schema: 1,
                setup_binding: "setup-binding".into(),
                selection: PortableHomeSelection::FullHome,
                applications: true,
            }),
            portable_flatpak: Some(sample_applications_expectation(2)),
            browser: Some(sample_browser_declaration(key)),
            mail: vec![sample_mail_receipt()],
        };
        receipt.validate().unwrap();
        let bytes = serde_json::to_vec(&receipt).unwrap();
        let parsed: PreparedSetupReceipt<StubCurrentDevice> =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, receipt);
        assert_refuses_unknown(&receipt);

        let minimal = PreparedSetupReceipt::<StubCurrentDevice> {
            identity_public_key: key,
            account_count: 0,
            current_identity: None,
            service_sessions: None,
            device_label: String::new(),
            portable_home: None,
            portable_flatpak: None,
            browser: None,
            mail: Vec::new(),
        };
        let minimal_json = serde_json::to_value(&minimal).unwrap();
        assert_eq!(
            minimal_json,
            serde_json::json!({
                "identity_public_key": key,
                "account_count": 0,
            })
        );
        assert_eq!(
            serde_json::from_value::<PreparedSetupReceipt<StubCurrentDevice>>(minimal_json)
                .unwrap(),
            minimal
        );
    }

    #[cfg(feature = "receipt")]
    #[test]
    fn stored_prepared_setup_receipt_accepts_kind_and_flat_and_refuses_flat_protocol() {
        let key = [7u8; 32];
        let apps = sample_applications_expectation(1);
        let browser = sample_browser_declaration(key);
        let expected = PreparedSetupReceipt {
            identity_public_key: key,
            account_count: 1,
            current_identity: None,
            service_sessions: Some(sample_service_sessions(1)),
            device_label: "Ada's laptop".into(),
            portable_home: Some(PortableHomeDeclaration {
                schema: 1,
                setup_binding: "setup-binding".into(),
                selection: PortableHomeSelection::FullHome,
                applications: true,
            }),
            portable_flatpak: Some(apps.clone()),
            browser: Some(browser.clone()),
            mail: vec![sample_mail_receipt()],
        };
        let from_kind = serde_json::from_slice::<
            stored::StoredPreparedSetupReceipt<StubCurrentDevice>,
        >(&serde_json::to_vec(&expected).unwrap())
        .unwrap()
        .0;
        assert_eq!(from_kind, expected);

        let mut flat_receipt = serde_json::json!({
            "identity_public_key": key,
            "account_count": 1,
            "service_sessions": expected.service_sessions,
            "device_label": "Ada's laptop",
            "portable_home": expected.portable_home,
            "portable_flatpak": {
                "schema": 1,
                "operation": apps.operation,
                "purpose": "flatpak_applications",
                "filename": apps.filename,
                "setup_binding": apps.setup_binding,
                "sealed_bytes": apps.sealed_bytes,
                "plaintext_bytes": apps.plaintext_bytes,
                "entries": apps.entries,
                "ciphertext_sha256": apps.ciphertext_sha256,
                "manifest_sha256": apps.kind.manifest_sha256,
                "limits": apps.limits,
            },
            "browser": {
                "plan": browser.plan,
                "payload": {
                    "schema": 1,
                    "operation": browser.payload.operation,
                    "purpose": "browser_profiles",
                    "filename": browser.payload.filename,
                    "setup_binding": browser.payload.setup_binding,
                    "sealed_bytes": browser.payload.sealed_bytes,
                    "plaintext_bytes": browser.payload.plaintext_bytes,
                    "entries": browser.payload.entries,
                    "ciphertext_sha256": browser.payload.ciphertext_sha256,
                    "plan_sha256": browser.payload.kind.plan_sha256,
                    "limits": browser.payload.limits,
                }
            },
            "mail": expected.mail,
        });
        let from_flat = serde_json::from_value::<
            stored::StoredPreparedSetupReceipt<StubCurrentDevice>,
        >(flat_receipt.clone())
        .unwrap()
        .0;
        assert_eq!(from_flat, expected);

        flat_receipt["portable_flatpak"]["consumer_protocol"] = serde_json::json!(2);
        assert!(
            serde_json::from_value::<stored::StoredPreparedSetupReceipt<StubCurrentDevice>>(
                flat_receipt
            )
            .is_err()
        );
    }

    #[test]
    fn flat_expectation_shapes_and_out_of_bound_expectations_are_refused() {
        let apps_ciphertext = [4u8; 32];
        let apps_manifest = [5u8; 32];
        let browser_ciphertext = [6u8; 32];
        let plan_digest = [9u8; 32];
        let flat_apps = serde_json::json!({
            "schema": 1,
            "operation": APP_OP,
            "purpose": "flatpak_applications",
            "filename": format!("applications-{APP_OP}.sealed"),
            "setup_binding": "setup-binding",
            "sealed_bytes": 512,
            "plaintext_bytes": 256,
            "entries": 4,
            "ciphertext_sha256": apps_ciphertext,
            "manifest_sha256": apps_manifest,
            "limits": {
                "plaintext_bytes": 1_048_576,
                "entries": 64,
                "metadata_bytes": 65_536
            }
        });
        assert!(serde_json::from_value::<ApplicationsExpectation>(flat_apps).is_err());

        let flat_browser = serde_json::json!({
            "schema": 1,
            "operation": BROWSER_OP,
            "purpose": "browser_profiles",
            "filename": format!("browser-{BROWSER_OP}.sealed"),
            "setup_binding": "setup-binding",
            "sealed_bytes": 512,
            "plaintext_bytes": 256,
            "entries": 2,
            "ciphertext_sha256": browser_ciphertext,
            "plan_sha256": plan_digest,
            "limits": {
                "plaintext_bytes": 1_048_576,
                "entries": 64,
                "metadata_bytes": 65_536
            }
        });
        assert!(serde_json::from_value::<BrowserExpectation>(flat_browser).is_err());

        let mut bad_apps = sample_applications_expectation(3);
        assert!(!bad_apps.valid());
        bad_apps.kind.consumer_protocol = 1;
        bad_apps.operation = "00000000-0000-0000-0000-000000000000".into();
        bad_apps.filename = "applications-00000000-0000-0000-0000-000000000000.sealed".into();
        assert!(!bad_apps.valid());

        let mut oversized_apps = sample_applications_expectation(1);
        oversized_apps.sealed_bytes = oversized_apps.limits.sealed_bound().unwrap() + 1;
        assert!(!oversized_apps.valid());
    }

    #[cfg(feature = "receipt")]
    #[test]
    fn out_of_bound_sessions_and_mismatched_browser_identity_are_refused() {
        let key = [7u8; 32];
        let mut bad_sessions = sample_service_sessions(3);
        assert!(!bad_sessions.valid());
        bad_sessions.consumer_protocol = 2;
        bad_sessions.sessions.push(SelectedSession {
            id: "second-session".into(),
            service: browser_continuity::sessions::Service::Discord,
        });
        assert!(!bad_sessions.valid());

        let wrong_browser_identity = PreparedSetupReceipt::<StubCurrentDevice> {
            identity_public_key: [8u8; 32],
            account_count: 1,
            current_identity: None,
            service_sessions: None,
            device_label: String::new(),
            portable_home: None,
            portable_flatpak: None,
            browser: Some(sample_browser_declaration(key)),
            mail: Vec::new(),
        };
        assert!(!wrong_browser_identity.valid());
    }
}
