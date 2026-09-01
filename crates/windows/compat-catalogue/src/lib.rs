//! Policy data for launching Windows applications on qOS.
//!
//! This crate deliberately does not launch a process, create a prefix, or
//! implement permissions.  It produces a fully specified launch plan for the
//! existing qOS security/launcher services.  In particular, a
//! [`SecurityProfileRef`] is only a reference to that service's profile; it is
//! not a second capability or sandbox language.
//!
//! Resolution is intentionally conservative:
//!
//! * exact catalogue identity wins over weaker metadata matches;
//! * an unknown application gets an isolated Wine plan;
//! * Proton is selectable only from a catalogue recipe; and
//! * a VM can only be selected by an explicit caller request.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Current on-disk catalogue schema.
pub const CATALOGUE_SCHEMA_VERSION: u16 = 1;
/// Current per-record schema.
pub const RECORD_VERSION: u32 = 1;
/// There is exactly one managed Windows guest per qOS installation.
pub const WINDOWS_VM_RUNTIME_ID: &str = "windows-vm";

/// Stable reference into qOS's existing security policy registry.
///
/// The catalogue stores this name so UI and launcher can show the effective
/// policy. It never interprets the name or grants access itself.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecurityProfileRef(String);

impl SecurityProfileRef {
    /// Construct a profile reference. Empty and path-like values are rejected
    /// because they would make policy ownership ambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogueError::InvalidSecurityProfile`] for an empty,
    /// oversized, path-like, or whitespace-containing key.
    pub fn new(value: impl Into<String>) -> Result<Self, CatalogueError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value.starts_with('.')
            || value.contains('/')
            || value.contains('\\')
            || value.chars().any(char::is_whitespace)
        {
            return Err(CatalogueError::InvalidSecurityProfile(value));
        }
        Ok(Self(value))
    }

    /// Return the registry key used by qOS's security broker.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The compatibility execution layer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// A normal Wine prefix, managed as an isolated qOS capsule.
    Wine,
    /// Proton through UMU/Steam Runtime. Never inferred for arbitrary apps.
    Proton,
    /// The one managed Windows guest. Never selected implicitly.
    Vm,
}

/// CPU architecture used by a Wine/Proton capsule.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    Win32,
    #[default]
    Win64,
}

/// Exact runner selected for a launch.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Runner {
    /// Human and log-facing runner family (for example `wine` or `umu-proton`).
    pub family: String,
    /// Immutable runtime identifier, such as `wine-11.0` or `proton-ge-10-20`.
    pub version: String,
}

impl Runner {
    /// Create a validated runner reference.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogueError::InvalidValue`] when either token is empty,
    /// too long, or contains a control character.
    pub fn new(
        family: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, CatalogueError> {
        let family = family.into();
        let version = version.into();
        validate_token("runner family", &family)?;
        validate_token("runner version", &version)?;
        Ok(Self { family, version })
    }

    fn wine_default() -> Self {
        Self {
            family: String::from("wine"),
            version: String::from("system"),
        }
    }

    fn validate(&self) -> Result<(), CatalogueError> {
        validate_token("runner family", &self.family)?;
        validate_token("runner version", &self.version)
    }
}

/// Settings passed to the capsule or UMU adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunnerSettings {
    /// 32-bit or 64-bit Windows userspace.
    #[serde(default)]
    pub architecture: Architecture,
    /// Windows compatibility version exposed by the runner.
    #[serde(default = "default_windows_version")]
    pub windows_version: String,
    /// Runner-specific environment overrides. The launcher validates which
    /// variables are safe; this crate only preserves catalogue data.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// DXVK/VKD3D and related graphics switches, interpreted by the runner.
    #[serde(default)]
    pub graphics: BTreeMap<String, String>,
    /// Optional qOS runner feature names (for example `dotnet48`).
    #[serde(default)]
    pub components: BTreeSet<String>,
}

impl Default for RunnerSettings {
    fn default() -> Self {
        Self {
            architecture: Architecture::default(),
            windows_version: default_windows_version(),
            environment: BTreeMap::new(),
            graphics: BTreeMap::new(),
            components: BTreeSet::new(),
        }
    }
}

/// A reference to the qOS-owned capsule or the single managed VM.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Runtime {
    /// Stable local ID, not a filesystem path.
    pub id: String,
    /// Exact layer used by the runtime.
    pub layer: Layer,
}

impl Runtime {
    fn default_wine() -> Self {
        Self {
            id: String::from("wine-default"),
            layer: Layer::Wine,
        }
    }
}

/// Metadata used to identify an installed application.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplicationQuery {
    /// qOS package/store identity, when known.
    #[serde(default)]
    pub package_id: Option<String>,
    /// Windows `AppUserModelId` or other stable application ID.
    #[serde(default)]
    pub app_id: Option<String>,
    /// Steam/UMU numeric game ID.
    #[serde(default)]
    pub game_id: Option<String>,
    /// Executable filename or relative path.
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

impl ApplicationQuery {
    /// Build a query from an executable path.
    pub fn executable(path: impl Into<String>) -> Self {
        Self {
            executable: Some(path.into()),
            ..Self::default()
        }
    }

    fn normalized(&self) -> NormalizedQuery {
        NormalizedQuery {
            package_id: normalize_optional(self.package_id.as_deref()),
            app_id: normalize_optional(self.app_id.as_deref()),
            game_id: normalize_optional(self.game_id.as_deref()),
            executable: normalize_executable(self.executable.as_deref()),
            publisher: normalize_optional(self.publisher.as_deref()),
            product: normalize_optional(self.product.as_deref()),
        }
    }
}

/// A catalogue identity matcher. Fields are `ANDed`, and a record may contain
/// several matchers (OR). Keeping this explicit makes precedence inspectable
/// in the UI and avoids fuzzy matching surprises.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchKey {
    #[serde(default)]
    pub package_id: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub game_id: Option<String>,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

impl MatchKey {
    /// Match the key after applying qOS's case/whitespace/path normalization.
    pub fn matches(&self, query: &ApplicationQuery) -> bool {
        let key = self.normalized();
        key.has_identity() && key.matches(&query.normalized())
    }

    fn normalized(&self) -> NormalizedKey {
        NormalizedKey {
            package_id: normalize_optional(self.package_id.as_deref()),
            app_id: normalize_optional(self.app_id.as_deref()),
            game_id: normalize_optional(self.game_id.as_deref()),
            executable: normalize_executable(self.executable.as_deref()),
            publisher: normalize_optional(self.publisher.as_deref()),
            product: normalize_optional(self.product.as_deref()),
        }
    }
}

/// A catalog record's launch recipe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub layer: Layer,
    pub runner: Runner,
    #[serde(default)]
    pub settings: RunnerSettings,
    /// Existing qOS security profile; no permissions are defined here.
    pub security_profile: SecurityProfileRef,
    /// Proton requires a recipe so arbitrary apps cannot drift into Proton.
    #[serde(default)]
    pub proton_recipe: Option<String>,
}

impl Recipe {
    fn validate(&self, id: &str) -> Result<(), CatalogueError> {
        self.runner.validate()?;
        validate_settings(&self.settings)?;
        validate_security_profile(&self.security_profile)?;
        if self.layer == Layer::Proton {
            let Some(recipe) = self.proton_recipe.as_deref() else {
                return Err(CatalogueError::ProtonRecipeRequired(id.to_owned()));
            };
            validate_token("Proton recipe", recipe)?;
            if self.runner.family != "umu-proton" && self.runner.family != "proton" {
                return Err(CatalogueError::InvalidProtonRunner {
                    id: id.to_owned(),
                    family: self.runner.family.clone(),
                });
            }
        } else if self.proton_recipe.is_some() {
            return Err(CatalogueError::UnexpectedProtonRecipe(id.to_owned()));
        }
        if self.layer == Layer::Vm && self.runner.family != "windows-vm" {
            return Err(CatalogueError::InvalidVmRunner {
                id: id.to_owned(),
                family: self.runner.family.clone(),
            });
        }
        if self.layer == Layer::Wine && self.runner.family != "wine" {
            return Err(CatalogueError::InvalidWineRunner {
                id: id.to_owned(),
                family: self.runner.family.clone(),
            });
        }
        Ok(())
    }
}

/// One versioned catalogue entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    #[serde(default = "default_record_version")]
    pub version: u32,
    #[serde(default)]
    pub matches: Vec<MatchKey>,
    pub recipe: Recipe,
}

/// Versioned persisted catalogue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    pub schema_version: u16,
    #[serde(default)]
    pub records: Vec<Record>,
}

impl Catalogue {
    /// Validate all data before it can influence launching. Unknown schema,
    /// duplicate IDs, malformed keys and malformed recipes fail closed.
    ///
    /// # Errors
    ///
    /// Returns a specific [`CatalogueError`] for any unsupported or malformed
    /// catalogue field.
    pub fn validate(&self) -> Result<(), CatalogueError> {
        if self.schema_version != CATALOGUE_SCHEMA_VERSION {
            return Err(CatalogueError::UnsupportedSchema(self.schema_version));
        }
        let mut ids = BTreeSet::new();
        for record in &self.records {
            validate_token("record id", &record.id)?;
            if record.version != RECORD_VERSION {
                return Err(CatalogueError::UnsupportedRecordVersion {
                    id: record.id.clone(),
                    version: record.version,
                });
            }
            if !ids.insert(record.id.clone()) {
                return Err(CatalogueError::DuplicateRecord(record.id.clone()));
            }
            if record.matches.is_empty() {
                return Err(CatalogueError::NoMatchKeys(record.id.clone()));
            }
            for key in &record.matches {
                validate_key(key, &record.id)?;
            }
            record.recipe.validate(&record.id)?;
        }
        Ok(())
    }

    /// Parse and validate a JSON catalogue before returning it.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogueError::InvalidJson`] for malformed JSON, or a
    /// validation error when parsed data is unsupported or unsafe.
    pub fn from_json(input: &str) -> Result<Self, CatalogueError> {
        let catalogue: Self = serde_json::from_str(input)
            .map_err(|source| CatalogueError::InvalidJson(source.to_string()))?;
        catalogue.validate()?;
        Ok(catalogue)
    }

    /// Resolve Wine/Proton routes. VM records are ignored unless the caller
    /// explicitly opts into VM fallback using [`Resolver::resolve`].
    ///
    /// # Errors
    ///
    /// Returns a validation error if any catalogue entry is unsupported or
    /// malformed.
    pub fn resolver(&self) -> Result<Resolver<'_>, CatalogueError> {
        self.validate()?;
        Ok(Resolver { catalogue: self })
    }
}

/// Whether the caller explicitly authorizes using the one Windows VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmFallback {
    /// Never select the VM, including when a VM recipe matches.
    Disabled,
    /// The user explicitly requested VM fallback for this launch.
    Explicit,
}

/// Deterministic resolver over a validated catalogue.
pub struct Resolver<'a> {
    catalogue: &'a Catalogue,
}

/// Result of selecting an application route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchPlan {
    pub record_id: Option<String>,
    pub match_kind: MatchKind,
    pub runtime: Runtime,
    pub runner: Runner,
    pub settings: RunnerSettings,
    pub security_profile: SecurityProfileRef,
    pub proton_recipe: Option<String>,
}

/// Why a catalogue record won.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchKind {
    DefaultWine,
    ProductPublisher,
    Executable,
    GameId,
    AppId,
    PackageId,
}

impl MatchKind {
    fn score(self) -> u8 {
        match self {
            Self::DefaultWine => 0,
            Self::ProductPublisher => 1,
            Self::Executable => 2,
            Self::GameId => 3,
            Self::AppId => 4,
            Self::PackageId => 5,
        }
    }
}

impl Resolver<'_> {
    /// Resolve a query. VM selection requires [`VmFallback::Explicit`].
    pub fn resolve(&self, query: &ApplicationQuery, vm_fallback: VmFallback) -> LaunchPlan {
        let query = query.normalized();
        let mut best: Option<(&Record, MatchKind, usize)> = None;
        for (index, record) in self.catalogue.records.iter().enumerate() {
            let Some(kind) = record_match_kind(record, &query) else {
                continue;
            };
            if record.recipe.layer == Layer::Vm && vm_fallback != VmFallback::Explicit {
                continue;
            }
            let candidate = (record, kind, index);
            let should_replace = best.is_none_or(|(current, current_kind, current_index)| {
                (
                    kind.score(),
                    std::cmp::Reverse(record.id.as_str()),
                    std::cmp::Reverse(index),
                ) > (
                    current_kind.score(),
                    std::cmp::Reverse(current.id.as_str()),
                    std::cmp::Reverse(current_index),
                )
            });
            if should_replace {
                best = Some(candidate);
            }
        }

        let Some((record, kind, _)) = best else {
            return default_wine_plan();
        };
        plan_from_record(record, kind)
    }
}

/// Convert UMU's launcher metadata into the same normalized query used by the
/// catalogue. Unknown fields are deliberately ignored for forward compatibility.
///
/// # Errors
///
/// Returns an error when metadata is not an object or contains no usable
/// application identity.
pub fn query_from_umu_metadata(metadata: &Value) -> Result<ApplicationQuery, CatalogueError> {
    let object = metadata
        .as_object()
        .ok_or(CatalogueError::UmuMetadataNotObject)?;
    let get = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| object.get(*name))
            .and_then(|value| match value {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
    };
    let query = ApplicationQuery {
        package_id: get(&["package_id", "packageId"]),
        app_id: get(&["app_id", "appId", "application_id"]),
        // UMU calls the Steam app identity `appid`; it is a game ID, not an
        // AppUserModelId. Keeping the two namespaces distinct preserves
        // deterministic precedence in the resolver.
        game_id: get(&["game_id", "gameId", "appid"]),
        executable: get(&["executable", "exe", "command"]),
        publisher: get(&["publisher", "developer"]),
        product: get(&["product", "name", "title"]),
    };
    if query.package_id.is_none()
        && query.app_id.is_none()
        && query.game_id.is_none()
        && query.executable.is_none()
        && query.publisher.is_none()
        && query.product.is_none()
    {
        return Err(CatalogueError::UmuMetadataEmpty);
    }
    Ok(query)
}

fn plan_from_record(record: &Record, kind: MatchKind) -> LaunchPlan {
    LaunchPlan {
        record_id: Some(record.id.clone()),
        match_kind: kind,
        runtime: Runtime {
            id: if record.recipe.layer == Layer::Vm {
                String::from(WINDOWS_VM_RUNTIME_ID)
            } else {
                record.id.clone()
            },
            layer: record.recipe.layer,
        },
        runner: record.recipe.runner.clone(),
        settings: record.recipe.settings.clone(),
        security_profile: record.recipe.security_profile.clone(),
        proton_recipe: record.recipe.proton_recipe.clone(),
    }
}

fn default_wine_plan() -> LaunchPlan {
    LaunchPlan {
        record_id: None,
        match_kind: MatchKind::DefaultWine,
        runtime: Runtime::default_wine(),
        runner: Runner::wine_default(),
        settings: RunnerSettings::default(),
        security_profile: SecurityProfileRef(String::from("windows-app")),
        proton_recipe: None,
    }
}

#[derive(Clone, Debug, Default)]
struct NormalizedQuery {
    package_id: Option<String>,
    app_id: Option<String>,
    game_id: Option<String>,
    executable: Option<String>,
    publisher: Option<String>,
    product: Option<String>,
}

type NormalizedKey = NormalizedQuery;

impl NormalizedKey {
    fn has_identity(&self) -> bool {
        self.package_id.is_some()
            || self.app_id.is_some()
            || self.game_id.is_some()
            || self.executable.is_some()
            || self.publisher.is_some()
            || self.product.is_some()
    }

    fn matches(&self, query: &NormalizedQuery) -> bool {
        fn equal_if_present(key: Option<&String>, value: Option<&String>) -> bool {
            key.is_none_or(|key| value == Some(key))
        }
        equal_if_present(self.package_id.as_ref(), query.package_id.as_ref())
            && equal_if_present(self.app_id.as_ref(), query.app_id.as_ref())
            && equal_if_present(self.game_id.as_ref(), query.game_id.as_ref())
            && equal_if_present(self.executable.as_ref(), query.executable.as_ref())
            && equal_if_present(self.publisher.as_ref(), query.publisher.as_ref())
            && equal_if_present(self.product.as_ref(), query.product.as_ref())
    }
}

fn record_match_kind(record: &Record, query: &NormalizedQuery) -> Option<MatchKind> {
    record
        .matches
        .iter()
        .filter_map(|key| {
            let key = key.normalized();
            if !key.matches(query) {
                return None;
            }
            if key.package_id.is_some() {
                Some(MatchKind::PackageId)
            } else if key.app_id.is_some() {
                Some(MatchKind::AppId)
            } else if key.game_id.is_some() {
                Some(MatchKind::GameId)
            } else if key.executable.is_some() {
                Some(MatchKind::Executable)
            } else if key.publisher.is_some() || key.product.is_some() {
                Some(MatchKind::ProductPublisher)
            } else {
                None
            }
        })
        .max_by_key(|kind| kind.score())
}

fn validate_key(key: &MatchKey, record_id: &str) -> Result<(), CatalogueError> {
    let fields = [
        ("package id", key.package_id.as_deref()),
        ("application id", key.app_id.as_deref()),
        ("game id", key.game_id.as_deref()),
        ("executable", key.executable.as_deref()),
        ("publisher", key.publisher.as_deref()),
        ("product", key.product.as_deref()),
    ];
    if fields.iter().all(|(_, value)| value.is_none()) {
        return Err(CatalogueError::EmptyMatchKey(record_id.to_owned()));
    }
    for (name, value) in fields {
        if let Some(value) = value {
            validate_token(name, value)?;
        }
    }
    Ok(())
}

fn validate_security_profile(profile: &SecurityProfileRef) -> Result<(), CatalogueError> {
    let value = profile.as_str();
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('.')
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_whitespace)
    {
        return Err(CatalogueError::InvalidSecurityProfile(value.to_owned()));
    }
    Ok(())
}

fn validate_settings(settings: &RunnerSettings) -> Result<(), CatalogueError> {
    validate_token("Windows version", &settings.windows_version)?;
    for (key, value) in &settings.environment {
        validate_token("environment key", key)?;
        validate_token("environment value", value)?;
    }
    for (key, value) in &settings.graphics {
        validate_token("graphics setting", key)?;
        validate_token("graphics value", value)?;
    }
    for component in &settings.components {
        validate_token("runner component", component)?;
    }
    Ok(())
}

fn validate_token(kind: &str, value: &str) -> Result<(), CatalogueError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(CatalogueError::InvalidValue {
            kind: kind.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
}

fn normalize_executable(value: Option<&str>) -> Option<String> {
    let value = normalize_optional(value)?;
    let value = value.replace('\\', "/");
    Some(
        value
            .rsplit('/')
            .next()
            .map_or_else(|| value.clone(), str::to_owned),
    )
}

fn default_record_version() -> u32 {
    1
}

fn default_windows_version() -> String {
    String::from("win10")
}

/// Errors are intentionally descriptive but do not expose host paths or
/// privileged operations; callers can show them directly in catalogue UI.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CatalogueError {
    #[error("unsupported catalogue schema {0}; refusing to load it")]
    UnsupportedSchema(u16),
    #[error("invalid JSON catalogue: {0}")]
    InvalidJson(String),
    #[error("invalid {kind}: {value:?}")]
    InvalidValue { kind: String, value: String },
    #[error("invalid security profile {0:?}")]
    InvalidSecurityProfile(String),
    #[error("duplicate catalogue record {0:?}")]
    DuplicateRecord(String),
    #[error("catalogue record {0:?} has no match keys")]
    NoMatchKeys(String),
    #[error("catalogue record {0:?} has an empty match key")]
    EmptyMatchKey(String),
    #[error("catalogue record {id:?} uses unsupported version {version}")]
    UnsupportedRecordVersion { id: String, version: u32 },
    #[error("Wine record {id:?} uses unsupported runner family {family:?}")]
    InvalidWineRunner { id: String, family: String },
    #[error("Proton record {0:?} must name a recipe")]
    ProtonRecipeRequired(String),
    #[error("non-Proton record {0:?} contains a Proton recipe")]
    UnexpectedProtonRecipe(String),
    #[error("Proton record {id:?} uses unsupported runner family {family:?}")]
    InvalidProtonRunner { id: String, family: String },
    #[error("VM record {id:?} must use the windows-vm runner, not {family:?}")]
    InvalidVmRunner { id: String, family: String },
    #[error("UMU metadata must be a JSON object")]
    UmuMetadataNotObject,
    #[error("UMU metadata did not contain a usable application identity")]
    UmuMetadataEmpty,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> SecurityProfileRef {
        SecurityProfileRef::new("windows-app").expect("valid fixture profile")
    }

    fn wine_record(id: &str, key: MatchKey) -> Record {
        Record {
            id: id.to_owned(),
            version: 1,
            matches: vec![key],
            recipe: Recipe {
                layer: Layer::Wine,
                runner: Runner::new("wine", "11").expect("valid fixture runner"),
                settings: RunnerSettings::default(),
                security_profile: profile(),
                proton_recipe: None,
            },
        }
    }

    fn catalogue(records: Vec<Record>) -> Catalogue {
        Catalogue {
            schema_version: CATALOGUE_SCHEMA_VERSION,
            records,
        }
    }

    #[test]
    fn normalizes_case_paths_and_whitespace() {
        let key = MatchKey {
            executable: Some(String::from("  Program\\APP.EXE  ")),
            ..MatchKey::default()
        };
        assert!(key.matches(&ApplicationQuery::executable("/tmp/program/app.exe")));
    }

    #[test]
    fn exact_identity_beats_executable_and_ties_are_deterministic() {
        let executable = wine_record(
            "z-executable",
            MatchKey {
                executable: Some(String::from("app.exe")),
                ..MatchKey::default()
            },
        );
        let exact = wine_record(
            "a-exact",
            MatchKey {
                app_id: Some(String::from("com.example.app")),
                executable: Some(String::from("app.exe")),
                ..MatchKey::default()
            },
        );
        let catalogue = catalogue(vec![executable, exact]);
        let resolver = catalogue.resolver().expect("valid catalogue");
        let plan = resolver.resolve(
            &ApplicationQuery {
                app_id: Some(String::from("COM.EXAMPLE.APP")),
                executable: Some(String::from("app.exe")),
                ..ApplicationQuery::default()
            },
            VmFallback::Disabled,
        );
        assert_eq!(plan.record_id.as_deref(), Some("a-exact"));
        assert_eq!(plan.match_kind, MatchKind::AppId);
    }

    #[test]
    fn unknown_application_defaults_to_wine() {
        let catalogue = catalogue(Vec::new());
        let resolver = catalogue.resolver().expect("valid catalogue");
        let plan = resolver.resolve(
            &ApplicationQuery::executable("thing.exe"),
            VmFallback::Disabled,
        );
        assert_eq!(plan.runtime.layer, Layer::Wine);
        assert_eq!(plan.match_kind, MatchKind::DefaultWine);
        assert_eq!(plan.runner.family, "wine");
    }

    #[test]
    fn proton_requires_recipe_and_umu_runner() {
        let mut record = wine_record(
            "game",
            MatchKey {
                game_id: Some(String::from("42")),
                ..MatchKey::default()
            },
        );
        record.recipe.layer = Layer::Proton;
        assert_eq!(
            catalogue(vec![record.clone()]).validate(),
            Err(CatalogueError::ProtonRecipeRequired(String::from("game")))
        );
        record.recipe.proton_recipe = Some(String::from("steam-game-42"));
        assert_eq!(
            catalogue(vec![record]).validate(),
            Err(CatalogueError::InvalidProtonRunner {
                id: String::from("game"),
                family: String::from("wine")
            })
        );
    }

    #[test]
    fn vm_is_never_implicit() {
        let mut vm = wine_record(
            "legacy",
            MatchKey {
                product: Some(String::from("Legacy")),
                ..MatchKey::default()
            },
        );
        vm.recipe.layer = Layer::Vm;
        vm.recipe.runner = Runner::new("windows-vm", "managed").expect("valid fixture runner");
        let catalogue = catalogue(vec![vm]);
        let resolver = catalogue.resolver().expect("valid catalogue");
        let query = ApplicationQuery {
            product: Some(String::from("legacy")),
            ..ApplicationQuery::default()
        };
        assert_eq!(
            resolver.resolve(&query, VmFallback::Disabled).match_kind,
            MatchKind::DefaultWine
        );
        assert_eq!(
            resolver.resolve(&query, VmFallback::Explicit).runtime.layer,
            Layer::Vm
        );
        assert_eq!(
            resolver.resolve(&query, VmFallback::Explicit).runtime.id,
            WINDOWS_VM_RUNTIME_ID
        );
    }

    #[test]
    fn malformed_catalogue_fails_closed() {
        let duplicate = wine_record(
            "same",
            MatchKey {
                product: Some(String::from("x")),
                ..MatchKey::default()
            },
        );
        let catalogue = catalogue(vec![duplicate.clone(), duplicate]);
        assert_eq!(
            catalogue.validate(),
            Err(CatalogueError::DuplicateRecord(String::from("same")))
        );
        assert!(Catalogue::from_json(r#"{"schema_version":99,"records":[]}"#).is_err());
    }

    #[test]
    fn umu_metadata_maps_known_aliases() {
        let numeric_metadata = serde_json::json!({ "appid": 123 });
        let numeric_query =
            query_from_umu_metadata(&numeric_metadata).expect("numeric app id is usable metadata");
        assert_eq!(numeric_query.game_id.as_deref(), Some("123"));

        // Numeric app IDs are accepted by UMU in practice, but must be
        // represented as strings before matching catalogue keys.
        let metadata = serde_json::json!({ "appid": "123", "exe": "Game\\game.exe" });
        let query = query_from_umu_metadata(&metadata).expect("usable metadata");
        assert_eq!(query.game_id.as_deref(), Some("123"));
        assert_eq!(query.executable.as_deref(), Some("Game\\game.exe"));
    }
}
