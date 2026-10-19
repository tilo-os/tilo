//! Manifest schema validation.
//!
//! Second pass after TOML decode: structural fields are guaranteed to
//! exist, so this checks cross-field invariants:
//!
//! - `package.version` parses as semver.
//! - `package.runtime` is `<name>/<semver>`, absent for `kind = "runtime"`.
//! - `package.platform-reason` is present exactly when `package.platforms`
//!   narrows the default targets; capability names are non-empty and unique.
//! - `kind = "app"` has `[entrypoint]` with `bin` set.
//! - `kind = "format-provider"` has non-empty `providers.mime` and
//!   ships [`FORMAT_PROVIDER_MODULE`] (payload-side counterpart in
//!   [`validate_manifest_with_payload`]; this only enforces the mime half).
//! - `publisher.identity` parses as `ed25519:<32-byte-hex>`.
//! - `publisher.signature`, if present, parses as 64-byte hex.

use semver::Version;

use crate::id::Kind;
use crate::manifest::{ContinuityFacet, DEFAULT_PLATFORMS, EntrypointType, Manifest};

/// File a `kind = "format-provider"` payload must ship: the provider's core
/// WebAssembly module, which the format host checks against its provider ABI
/// when loading it.
pub const FORMAT_PROVIDER_MODULE: &str = "provider.wasm";

/// Validation failure reasons; each variant is one concrete invariant a manifest can violate, surfaced verbatim by the CLI.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("package.version {0:?} is not valid semver: {1}")]
    BadVersion(String, String),
    #[error("package.runtime {0:?} must be `<name>/<semver>`")]
    BadRuntimeForm(String),
    #[error("package.runtime version {0:?} is not valid semver: {1}")]
    BadRuntimeVersion(String, String),
    #[error("package.platforms must name at least one platform")]
    EmptyPlatforms,
    #[error("package.platforms repeats {0:?}")]
    DuplicatePlatform(String),
    #[error(
        "package.platforms leaves out {missing} and needs a one-line package.platform-reason, \
         for example \"Writes OS images to block devices.\""
    )]
    MissingPlatformReason { missing: String },
    #[error(
        "package.platform-reason is only for an app that narrows package.platforms; \
         remove it, the app runs on linux, macos and web"
    )]
    UnneededPlatformReason,
    #[error("package.platform-reason must be one non-empty line")]
    BadPlatformReason,
    #[error("package.taglines.{0} must be one non-empty line of at most 80 characters")]
    BadTagline(String),
    #[error(
        "package.screenshots entry {0:?} must be a PNG, JPEG or WebP file directly under \
         `screenshots/`, listed once"
    )]
    BadScreenshot(String),
    #[error("package.screenshots lists more than {MAX_SCREENSHOTS} screenshots")]
    TooManyScreenshots,
    #[error("capabilities.{list} has an empty or padded capability name {name:?}")]
    BadCapabilityName { list: &'static str, name: String },
    #[error("capabilities lists {0:?} more than once")]
    DuplicateCapability(String),
    #[error(
        "capabilities declares {0:?}, which lowers the app sandbox, and needs a one-line \
         reason under [capabilities.reasons], for example {0} = \"Batches disk reads.\""
    )]
    MissingCapabilityReason(String),
    #[error("capabilities.reasons explains {0:?}, which capabilities does not declare")]
    UndeclaredCapabilityReason(String),
    #[error("capabilities.reasons.{0} must be one non-empty line")]
    BadCapabilityReason(String),
    #[error("kind=runtime must not declare a runtime dependency")]
    RuntimeKindHasRuntime,
    #[error("runtime entrypoint {0:?} must name a safe relative payload file")]
    RuntimeEntrypoint(String),
    #[error("only kind=runtime packages declare `package.provides`")]
    ProvidesOnNonRuntime,
    #[error("package.service-protocol requires a bounded canonical name and positive version")]
    BadServiceProtocol,
    #[error(
        "a native kind=runtime package (one with `[package.isa]`) requires `package.provides`, \
         the runtime it is, e.g. `tilo-runtime/<release>+native.<32 hex>`"
    )]
    RuntimeWithoutProvides,
    #[error(
        "package.provides {0:?} is neither `<name>/<release>+native.<32 hex>` nor \
         `<name>/<version>`"
    )]
    BadProvides(String),
    #[error("package.optional-runtimes lists {0:?} more than once, or as package.runtime as well")]
    DuplicateOptionalRuntime(String),
    #[error("package.optional-runtimes lists more than {MAX_OPTIONAL_RUNTIMES} runtimes")]
    TooManyOptionalRuntimes,
    #[error("kind=app requires `[entrypoint]` with `bin` set")]
    AppMissingEntrypoint,
    #[error("kind=format-provider requires non-empty `providers.mime`")]
    FormatProviderMissingMime,
    #[error("kind=format-provider requires `provider.wasm` in the payload")]
    FormatProviderMissingModule,
    #[error("invalid provider file extension {0:?}")]
    BadProviderExtension(String),
    #[error("invalid provider MIME type {0:?}")]
    BadProviderMime(String),
    #[error("invalid or reserved provider URI scheme {0:?}")]
    BadProviderScheme(String),
    #[error("invalid resource kind {0:?}")]
    BadResourceKind(String),
    #[error("offers: {0}")]
    BadOffer(String),
    #[error("sandbox.services: {0}")]
    BadService(String),
    #[error("publisher.identity: {0}")]
    BadPublisherIdentity(String),
    #[error("publisher.signature: {0}")]
    BadPublisherSignature(String),
    #[error("continuity.required repeats facet {0:?}")]
    DuplicateContinuityFacet(ContinuityFacet),
    #[error("continuity.required semantic state also requires presentation state")]
    SemanticContinuityWithoutPresentation,
    #[error("continuity.required semantic state requires continuity.semantic-schema")]
    SemanticContinuityWithoutSchema,
    #[error("kind=app requires presentation in continuity.required")]
    AppContinuityMissingPresentation,
    #[error("accessibility.atspi_acceptance.{field} must not be empty")]
    EmptyAtspiAcceptanceField { field: &'static str },
    #[error("target: {0}")]
    BadTarget(String),
    #[error("target.module {0:?} must be a safe payload path (an `.apk` or `.aab` for android)")]
    BadTargetModule(String),
    #[error("a build with a `[target]` declares no native `[package.isa]`")]
    TargetWithNativeIsa,
    #[error("only kind=app packages have a `[target]`")]
    TargetKindUnsupported,
    #[error("target.runtime {target:?} is not a build of package.runtime {declared:?}")]
    TargetRuntimeMismatch { declared: String, target: String },
    #[error("the app's GUI entrypoint must be its target.module {0:?}")]
    TargetEntrypointMismatch(String),
    #[error("target.module {0:?} is not in the payload")]
    TargetModuleMissing(String),
    #[error("kind={0} requires an `[image]` block")]
    ImageMissing(Kind),
    #[error("only kind=env-runtime and kind=tool packages have an `[image]` block")]
    ImageOnNonImage,
    #[error("kind=tool requires `image.runtime`, the exact env-runtime package id it was built on")]
    ToolWithoutRuntime,
    #[error("kind=env-runtime is the base of a stack and names no `image.runtime`")]
    EnvRuntimeWithRuntime,
    #[error("kind={kind} is an image and is never launched; remove `{block}`")]
    ImageLaunchable { kind: Kind, block: &'static str },
    #[error("kind={0} requires `[package.isa]` for the architecture its binaries are built for")]
    ImageWithoutArch(Kind),
    #[error("image.packages entry {0:?} is empty, padded or repeated")]
    BadImagePackage(String),
    #[error("{0}")]
    ImageTree(String),
    #[error(
        "package `{package}` declares `sandbox.fs.write = [{entry:?}]`, and an app does not \
         choose where it writes.\n  \
         The launcher gives every app one directory — per room, per package — and points HOME, \
         XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_STATE_HOME and XDG_CACHE_HOME inside it. Remove \
         the `[sandbox.fs]` block; anything the app writes under those paths already lands \
         there.\n  \
         Access to a path outside it is a capability the user grants at runtime, not a line in \
         a manifest."
    )]
    FsWriteDeclared { package: String, entry: String },
    #[error(
        "package `{package}` declares `sandbox.fs.read = [{entry:?}]`, and an app does not \
         choose what it reads.\n  \
         Every app gets the same system read profile (loader and C library, fonts, GPU loader \
         manifests, CA bundle, timezone data). Remove the `[sandbox.fs]` block.\n  \
         Anything the profile does not cover — a user's documents, /usr/bin, /var/workspace, a \
         device node — is a capability the user grants at runtime, not a line in a manifest."
    )]
    FsReadDeclared { package: String, entry: String },
}

/// Validate the manifest against schema rules that don't require
/// touching the payload; payload-side checks live in
/// [`crate::pack::pack`] and [`validate_manifest_with_payload`].
///
/// # Errors
///
/// Returns the specific [`ManifestError`] variant for the first invariant
/// violated. Validation proceeds through filesystem policy (writes before
/// reads), platforms and capabilities, continuity and accessibility, package
/// version, optional runtimes and runtime entrypoint, runtime dependency and
/// provides, service protocol, kind-specific entrypoint, image metadata,
/// provider/resource/service declarations, publisher identity and signature,
/// then target compatibility.
pub fn validate_manifest(m: &Manifest) -> Result<(), ManifestError> {
    validate_filesystem_policy(m)?;

    validate_platforms(m).and_then(|()| validate_capabilities(m))?;

    validate_store_listing(m)?;

    validate_continuity(m)?;

    validate_atspi_acceptance(m)?;

    Version::parse(&m.package.version)
        .map_err(|e| ManifestError::BadVersion(m.package.version.clone(), e.to_string()))?;

    validate_optional_runtimes(m)?;
    if m.package.kind == Kind::Runtime
        && let Some(entrypoint) = &m.entrypoint
        && entrypoint
            .bin
            .to_str()
            .is_none_or(|path| !crate::package::is_safe_path(path))
    {
        return Err(ManifestError::RuntimeEntrypoint(
            entrypoint.bin.display().to_string(),
        ));
    }

    match (&m.package.kind, &m.package.runtime) {
        (Kind::Runtime, Some(_)) => return Err(ManifestError::RuntimeKindHasRuntime),
        (_, Some(rt)) => validate_runtime_string(rt)?,
        (_, None) => {
            // Schema allows runtime to be omitted for non-runtime kinds (this
            // also covers Kind::Runtime with no runtime field); the loader
            // applies a default, not an error here.
        }
    }
    validate_provides(m)?;
    validate_service_protocol(m)?;

    validate_kind_entrypoint(m)?;
    validate_image(m)?;
    validate_declarations(m)?;

    m.publisher
        .identity_bytes()
        .map_err(|e| ManifestError::BadPublisherIdentity(e.to_string()))?;
    m.publisher
        .signature_bytes()
        .map_err(|e| ManifestError::BadPublisherSignature(e.to_string()))?;
    validate_target(m)
}

/// Publishers cannot grant themselves filesystem access.
fn validate_filesystem_policy(m: &Manifest) -> Result<(), ManifestError> {
    // Filesystem policy first — the check most likely to reject a
    // package a publisher believed was fine, so it's the line they read
    // rather than the fourth error after a semver nit. Writes before
    // reads: a package asking for `/` is the more alarming of the two.
    if let Some(entry) = m.sandbox.fs.write.first() {
        return Err(ManifestError::FsWriteDeclared {
            package: m.package.id.clone(),
            entry: entry.clone(),
        });
    }
    if let Some(entry) = m.sandbox.fs.read.first() {
        return Err(ManifestError::FsReadDeclared {
            package: m.package.id.clone(),
            entry: entry.clone(),
        });
    }
    Ok(())
}

/// Continuity facets must be unique and include the state their dependencies require.
fn validate_continuity(m: &Manifest) -> Result<(), ManifestError> {
    let mut facets = std::collections::BTreeSet::new();
    for facet in &m.continuity.required {
        if !facets.insert(*facet) {
            return Err(ManifestError::DuplicateContinuityFacet(*facet));
        }
    }
    if facets.contains(&ContinuityFacet::Semantic)
        && !facets.contains(&ContinuityFacet::Presentation)
    {
        return Err(ManifestError::SemanticContinuityWithoutPresentation);
    }
    if facets.contains(&ContinuityFacet::Semantic) && m.continuity.semantic_schema.is_none() {
        return Err(ManifestError::SemanticContinuityWithoutSchema);
    }
    if m.package.kind == Kind::App && !facets.contains(&ContinuityFacet::Presentation) {
        return Err(ManifestError::AppContinuityMissingPresentation);
    }
    Ok(())
}

/// Service wire contracts use a bounded canonical name and a positive version.
fn validate_service_protocol(m: &Manifest) -> Result<(), ManifestError> {
    if m.package.service_protocol.as_ref().is_some_and(|protocol| {
        protocol.version == 0
            || protocol.name.is_empty()
            || protocol.name.len() > 128
            || !protocol.name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
            || protocol
                .name
                .as_bytes()
                .first()
                .is_none_or(|byte| !byte.is_ascii_lowercase())
            || !protocol
                .name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    }) {
        return Err(ManifestError::BadServiceProtocol);
    }
    Ok(())
}

/// Each launchable package kind must declare the entrypoint or provider it needs.
fn validate_kind_entrypoint(m: &Manifest) -> Result<(), ManifestError> {
    match m.package.kind {
        Kind::App => {
            let ep = m
                .entrypoint
                .as_ref()
                .ok_or(ManifestError::AppMissingEntrypoint)?;
            // Which paths are load-bearing depends on the entrypoint's flavour:
            // `Gui`/`Cli` fall back to top-level `bin`; `Both` requires
            // `gui_bin`/`cli_bin` (or legacy `bin` as fallback).
            let has_bin = match ep.r#type {
                EntrypointType::Gui | EntrypointType::Cli => !ep.bin.as_os_str().is_empty(),
                EntrypointType::Both => {
                    ep.gui_bin
                        .as_ref()
                        .is_some_and(|p| !p.as_os_str().is_empty())
                        || ep
                            .cli_bin
                            .as_ref()
                            .is_some_and(|p| !p.as_os_str().is_empty())
                        || !ep.bin.as_os_str().is_empty()
                }
            };
            if !has_bin {
                return Err(ManifestError::AppMissingEntrypoint);
            }
            // A [cli] block only takes effect if the entrypoint opts in — not
            // rejected on a Gui entrypoint (forward-compatible schema); the
            // installer just won't install shims from it.
            if matches!(ep.r#type, EntrypointType::Cli | EntrypointType::Both)
                && m.cli.as_ref().is_none_or(|c| c.commands.is_empty())
            {
                // A CLI entrypoint with no commands is fine — still runnable via `tilo-install run <id>`, no error.
            }
        }
        Kind::FormatProvider => {
            if m.providers.mime.is_empty() {
                return Err(ManifestError::FormatProviderMissingMime);
            }
        }
        Kind::Service | Kind::Plugin | Kind::Runtime | Kind::EnvRuntime | Kind::Tool => {}
    }
    Ok(())
}

/// An image is a directory tree for an environment's `/usr` and `/etc`:
/// bound to its runtime when it is a tool, built for a named architecture,
/// and with nothing a launcher could start.
fn validate_image(m: &Manifest) -> Result<(), ManifestError> {
    let kind = m.package.kind;
    let Some(image) = &m.image else {
        return if kind.is_image() {
            Err(ManifestError::ImageMissing(kind))
        } else {
            Ok(())
        };
    };
    if !kind.is_image() {
        return Err(ManifestError::ImageOnNonImage);
    }
    match (kind, image.runtime) {
        (Kind::Tool, None) => return Err(ManifestError::ToolWithoutRuntime),
        (Kind::EnvRuntime, Some(_)) => return Err(ManifestError::EnvRuntimeWithRuntime),
        _ => {}
    }
    let launchable = [
        ("[entrypoint]", m.entrypoint.is_some()),
        ("[cli]", m.cli.is_some()),
        ("[target]", m.target.is_some()),
        ("package.runtime", m.package.runtime.is_some()),
        (
            "[providers]",
            !(m.providers.mime.is_empty()
                && m.providers.ext.is_empty()
                && m.providers.schemes.is_empty()),
        ),
    ];
    if let Some((block, _)) = launchable.iter().find(|(_, present)| *present) {
        return Err(ManifestError::ImageLaunchable { kind, block });
    }
    if m.package.isa.x86_64.is_none() && m.package.isa.aarch64.is_none() {
        return Err(ManifestError::ImageWithoutArch(kind));
    }
    let mut seen = std::collections::BTreeSet::new();
    for package in &image.packages {
        if package.trim().is_empty() || package.trim() != package || !seen.insert(package) {
            return Err(ManifestError::BadImagePackage(package.clone()));
        }
    }
    Ok(())
}

/// What the app claims to open and the resource kinds its records hold.
fn validate_declarations(m: &Manifest) -> Result<(), ManifestError> {
    for ext in &m.providers.ext {
        if !valid_extension(ext) {
            return Err(ManifestError::BadProviderExtension(ext.clone()));
        }
    }
    for mime in &m.providers.mime {
        if !valid_mime(mime) {
            return Err(ManifestError::BadProviderMime(mime.clone()));
        }
    }
    for scheme in &m.providers.schemes {
        if !valid_scheme(scheme) {
            return Err(ManifestError::BadProviderScheme(scheme.clone()));
        }
    }
    for kind in &m.resources.kinds {
        if !valid_resource_kind(kind) {
            return Err(ManifestError::BadResourceKind(kind.clone()));
        }
    }
    validate_offers(&m.offers)?;
    for (name, req) in &m.sandbox.services {
        registry_types::InterfaceRequirement::new(name, req.version)
            .map_err(|error| ManifestError::BadService(error.to_string()))?;
    }
    Ok(())
}

fn validate_target(m: &Manifest) -> Result<(), ManifestError> {
    let Some(target) = &m.target else {
        return Ok(());
    };
    let registry_target = registry_types::RegistryTarget {
        platform: target.platform.clone(),
        architecture: target.architecture.clone(),
        isa: target.isa.clone(),
        runtime: Some(target.runtime.clone()),
        interfaces: Vec::new(),
    };
    registry_target
        .validate()
        .map_err(|error| ManifestError::BadTarget(error.to_string()))?;
    if !registry_types::DESKTOP_PLATFORMS.contains(&target.platform.as_str()) {
        return Err(ManifestError::BadTarget(format!(
            "platform {:?} is native; native builds declare `[package.isa]` instead",
            target.platform
        )));
    }
    let android_package = || {
        std::path::Path::new(&target.module)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                registry_types::ANDROID_PACKAGE_EXTENSIONS.contains(&extension)
            })
    };
    if !crate::package::is_safe_path(&target.module)
        || target.module.len() > 255
        || (target.platform == registry_types::ANDROID_PLATFORM && !android_package())
    {
        return Err(ManifestError::BadTargetModule(target.module.clone()));
    }
    if m.package.isa.x86_64.is_some() || m.package.isa.aarch64.is_some() {
        return Err(ManifestError::TargetWithNativeIsa);
    }
    if m.package.kind != Kind::App {
        return Err(ManifestError::TargetKindUnsupported);
    }
    let declared = m.package.runtime.clone().unwrap_or_default();
    if declared.is_empty() || declared != target.runtime {
        return Err(ManifestError::TargetRuntimeMismatch {
            declared,
            target: target.runtime.clone(),
        });
    }
    let entry = m
        .entrypoint
        .as_ref()
        .and_then(|entry| entry.gui_bin_path())
        .and_then(|path| path.to_str());
    if entry != Some(target.module.as_str()) {
        return Err(ManifestError::TargetEntrypointMismatch(
            target.module.clone(),
        ));
    }
    Ok(())
}

fn validate_platforms(m: &Manifest) -> Result<(), ManifestError> {
    let platforms = m.package.platforms();
    if platforms.is_empty() {
        return Err(ManifestError::EmptyPlatforms);
    }
    let mut seen = std::collections::BTreeSet::new();
    if let Some(repeated) = platforms.iter().find(|platform| !seen.insert(**platform)) {
        return Err(ManifestError::DuplicatePlatform(
            repeated.as_str().to_owned(),
        ));
    }
    match (&m.package.platform_reason, m.package.is_platform_narrowed()) {
        (Some(reason), true) if reason.trim().is_empty() || reason.contains('\n') => {
            Err(ManifestError::BadPlatformReason)
        }
        (Some(_), false) => Err(ManifestError::UnneededPlatformReason),
        (None, true) => Err(ManifestError::MissingPlatformReason {
            missing: DEFAULT_PLATFORMS
                .iter()
                .filter(|platform| !platforms.contains(platform))
                .map(|platform| platform.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }),
        _ => Ok(()),
    }
}

/// Most screenshots a package lists; the Store shows them in one gallery.
pub const MAX_SCREENSHOTS: usize = 8;

/// The longest tag line, in characters: one phrase under the app's name.
pub const MAX_TAGLINE_CHARS: usize = 80;

/// Tag lines are one short line each; screenshots are image files directly
/// under the package's `screenshots/` directory, each listed once.
fn validate_store_listing(m: &Manifest) -> Result<(), ManifestError> {
    for (tag, tagline) in &m.package.taglines {
        let trimmed = tagline.trim();
        if trimmed.is_empty()
            || trimmed.len() != tagline.len()
            || tagline.contains('\n')
            || tagline.chars().count() > MAX_TAGLINE_CHARS
        {
            return Err(ManifestError::BadTagline(tag.clone()));
        }
    }
    if m.package.screenshots.len() > MAX_SCREENSHOTS {
        return Err(ManifestError::TooManyScreenshots);
    }
    let mut seen = std::collections::BTreeSet::new();
    for shot in &m.package.screenshots {
        let valid = shot.strip_prefix("screenshots/").is_some_and(|name| {
            !name.is_empty()
                && !name.starts_with('.')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && std::path::Path::new(name)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "webp"
                        )
                    })
        });
        if !valid || !seen.insert(shot.as_str()) {
            return Err(ManifestError::BadScreenshot(shot.clone()));
        }
    }
    Ok(())
}

fn validate_capabilities(m: &Manifest) -> Result<(), ManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for (list, names) in [
        ("required", &m.capabilities.required),
        ("optional", &m.capabilities.optional),
    ] {
        for name in names {
            if name.trim().is_empty() || name.trim() != name {
                return Err(ManifestError::BadCapabilityName {
                    list,
                    name: name.clone(),
                });
            }
            if !seen.insert(name.as_str()) {
                return Err(ManifestError::DuplicateCapability(name.clone()));
            }
        }
    }
    for (name, reason) in &m.capabilities.reasons {
        if !seen.contains(name.as_str()) {
            return Err(ManifestError::UndeclaredCapabilityReason(name.clone()));
        }
        if reason.trim().is_empty() || reason.contains('\n') {
            return Err(ManifestError::BadCapabilityReason(name.clone()));
        }
    }
    for name in crate::manifest::REASONED_CAPABILITIES {
        if seen.contains(name) && !m.capabilities.reasons.contains_key(*name) {
            return Err(ManifestError::MissingCapabilityReason((*name).to_string()));
        }
    }
    Ok(())
}

fn validate_atspi_acceptance(m: &Manifest) -> Result<(), ManifestError> {
    let Some(acceptance) = &m.accessibility.atspi_acceptance else {
        return Ok(());
    };
    for (field, value) in [("root", &acceptance.root), ("action", &acceptance.action)] {
        if value.trim().is_empty() {
            return Err(ManifestError::EmptyAtspiAcceptanceField { field });
        }
    }
    for (field, values) in [
        ("required", &acceptance.required),
        ("after_action", &acceptance.after_action),
    ] {
        if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
            return Err(ManifestError::EmptyAtspiAcceptanceField { field });
        }
    }
    Ok(())
}

pub(crate) fn valid_extension(ext: &str) -> bool {
    !ext.is_empty()
        && ext.len() <= 64
        && ext
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_'))
}

pub(crate) fn valid_mime(mime: &str) -> bool {
    let Some((top, subtype)) = mime.split_once('/') else {
        return false;
    };
    mime.len() <= 255
        && !subtype.contains('/')
        && top != "*"
        && subtype != "*"
        && valid_mime_token(top)
        && valid_mime_token(subtype)
}

fn valid_mime_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 127
        && token.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

pub(crate) fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    !scheme.is_empty()
        && scheme.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !matches!(scheme, "file" | "data" | "javascript")
}

/// The shape the toolkit's `RecordKind` accepts, so every declared kind can
/// name a record.
fn valid_resource_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind.bytes().enumerate().all(|(i, byte)| {
            (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && (i != 0 || byte.is_ascii_alphabetic())
        })
}

/// How many actions one package may offer while it is not running.
pub const MAX_OFFERS: usize = 64;

/// Resource prefixes an app cannot offer under: `app.` names the host's own
/// application resources (opening and closing an app), and a
/// `tilo-ui-bound:` id belongs to one window of one run, so it means nothing
/// before the app runs.
const RESERVED_OFFER_RESOURCES: [&str; 2] = ["app.", "tilo-ui-bound:"];

/// `[[offers]]`: stable addresses, a readable label, and each address once.
fn validate_offers(offers: &[crate::manifest::OfferedAction]) -> Result<(), ManifestError> {
    if offers.len() > MAX_OFFERS {
        return Err(ManifestError::BadOffer(format!(
            "more than {MAX_OFFERS} offered actions"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for offer in offers {
        let address = format!("{}#{}", offer.resource, offer.action);
        let resource_ok = offer.resource.len() <= 128
            && offer
                .resource
                .starts_with(|c: char| c.is_ascii_alphabetic())
            && offer.resource.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
            })
            && !RESERVED_OFFER_RESOURCES
                .iter()
                .any(|reserved| offer.resource.starts_with(reserved));
        if !resource_ok {
            return Err(ManifestError::BadOffer(format!(
                "{address}: resource must be a stable address the app publishes"
            )));
        }
        if !valid_resource_kind(offer.resource_kind()) {
            return Err(ManifestError::BadOffer(format!(
                "{address}: invalid resource-kind {:?}",
                offer.resource_kind()
            )));
        }
        if !valid_resource_kind(&offer.action) {
            return Err(ManifestError::BadOffer(format!(
                "{address}: invalid action id"
            )));
        }
        let line = |text: &str, max: usize| {
            !text.trim().is_empty()
                && text.trim() == text
                && text.len() <= max
                && !text.contains('\n')
        };
        if !line(&offer.label, 80) {
            return Err(ManifestError::BadOffer(format!(
                "{address}: label must be one non-empty line of at most 80 bytes"
            )));
        }
        if offer
            .description
            .as_deref()
            .is_some_and(|description| !line(description, 400))
        {
            return Err(ManifestError::BadOffer(format!(
                "{address}: description must be one non-empty line of at most 400 bytes"
            )));
        }
        if !seen.insert(address.clone()) {
            return Err(ManifestError::BadOffer(format!(
                "{address} is offered twice"
            )));
        }
    }
    Ok(())
}

/// How many further runtime bundles one package may ask the launcher to
/// mount.
pub const MAX_OPTIONAL_RUNTIMES: usize = 4;

fn validate_optional_runtimes(m: &Manifest) -> Result<(), ManifestError> {
    let optional = &m.package.optional_runtimes;
    if optional.is_empty() {
        return Ok(());
    }
    if m.package.kind == Kind::Runtime {
        return Err(ManifestError::RuntimeKindHasRuntime);
    }
    if optional.len() > MAX_OPTIONAL_RUNTIMES {
        return Err(ManifestError::TooManyOptionalRuntimes);
    }
    let mut seen = std::collections::BTreeSet::new();
    for raw in optional {
        validate_runtime_string(raw)?;
        // Bundles are installed and mounted by version alone, so two
        // references naming one version would name one bundle.
        let version = raw
            .split_once('/')
            .map_or(raw.as_str(), |(_, version)| version);
        let base = m
            .package
            .runtime
            .as_deref()
            .and_then(|runtime| runtime.split_once('/'))
            .map(|(_, version)| version);
        if !seen.insert(version) || base == Some(version) {
            return Err(ManifestError::DuplicateOptionalRuntime(raw.clone()));
        }
    }
    Ok(())
}

/// `package.provides` belongs to a native runtime package and names the
/// runtime it is: the exact build of the runtime thin apps link
/// (`<name>/<release>+native.<digest>`), or an engine runtime an app mounts
/// as an optional runtime (`<name>/<version>`).
fn validate_provides(m: &Manifest) -> Result<(), ManifestError> {
    match (&m.package.kind, &m.package.provides) {
        (Kind::Runtime, Some(provides)) => validate_runtime_string(provides)
            .map_err(|_| ManifestError::BadProvides(provides.clone())),
        (Kind::Runtime, None) if !m.package.isa.is_empty() && m.target.is_none() => {
            Err(ManifestError::RuntimeWithoutProvides)
        }
        (_, Some(_)) => Err(ManifestError::ProvidesOnNonRuntime),
        (_, None) => Ok(()),
    }
}

fn validate_runtime_string(raw: &str) -> Result<(), ManifestError> {
    if registry_types::parse_native_runtime(raw).is_ok() {
        return Ok(());
    }
    let (name, ver) = raw
        .split_once('/')
        .ok_or_else(|| ManifestError::BadRuntimeForm(raw.to_string()))?;
    if name.is_empty() || ver.is_empty() {
        return Err(ManifestError::BadRuntimeForm(raw.to_string()));
    }
    // Some real-world runtime versions look like 2025.06, not semver (the
    // spec example uses tilo-runtime/2025.06) — accept strict semver or a
    // YYYY.MM[.MICRO] calendar form.
    if is_calver(ver) || Version::parse(ver).is_ok() {
        Ok(())
    } else {
        Err(ManifestError::BadRuntimeVersion(
            ver.to_string(),
            "not semver and not YYYY.MM[.MICRO] calendar form".into(),
        ))
    }
}

/// Calendar-version recogniser: 2 or 3 dot-separated non-empty digit groups, leading zeros permitted.
fn is_calver(ver: &str) -> bool {
    let parts: Vec<&str> = ver.split('.').collect();
    if !matches!(parts.len(), 2 | 3) {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Payload-aware validation, called after walking the payload
/// directory: checks `format-provider` ships [`FORMAT_PROVIDER_MODULE`].
///
/// # Errors
///
/// Returns whatever [`validate_manifest`] would for the manifest alone, or
/// [`ManifestError::FormatProviderMissingModule`] if `kind = "format-provider"`
/// and `payload_files` has no [`FORMAT_PROVIDER_MODULE`].
pub fn validate_manifest_with_payload(
    m: &Manifest,
    payload_files: &[std::path::PathBuf],
) -> Result<(), ManifestError> {
    validate_manifest(m)?;
    if m.package.kind == Kind::Runtime
        && let Some(entrypoint) = &m.entrypoint
        && !payload_files.iter().any(|path| path == &entrypoint.bin)
    {
        return Err(ManifestError::RuntimeEntrypoint(
            entrypoint.bin.display().to_string(),
        ));
    }
    if matches!(m.package.kind, Kind::FormatProvider)
        && !payload_files
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == FORMAT_PROVIDER_MODULE))
    {
        return Err(ManifestError::FormatProviderMissingModule);
    }
    if m.package.kind.is_image()
        && !payload_files
            .iter()
            .any(|path| path.as_os_str() == crate::image::TREE_FILE)
    {
        return Err(ManifestError::ImageTree(format!(
            "kind={} requires {} at the payload root",
            m.package.kind,
            crate::image::TREE_FILE
        )));
    }
    if let Some(target) = &m.target
        && !payload_files.iter().any(|path| {
            path.to_str()
                .is_some_and(|path| path.replace('\\', "/") == target.module)
        })
    {
        return Err(ManifestError::TargetModuleMissing(target.module.clone()));
    }
    if let Some(missing) = m.package.screenshots.iter().find(|shot| {
        !payload_files.iter().any(|path| {
            path.to_str()
                .is_some_and(|path| path.replace('\\', "/") == shot.as_str())
        })
    }) {
        return Err(ManifestError::BadScreenshot(missing.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_listing(listing: &str) -> Manifest {
        let src = format!(
            r#"
[package]
id = "so.tilo.example"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2026.07"
{listing}

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[entrypoint]
bin = "bin/example"
"#
        );
        Manifest::from_toml_str(&src).unwrap()
    }

    #[test]
    fn taglines_are_one_short_line_and_screenshots_live_under_their_directory() {
        let fine = app_with_listing(
            "screenshots = [\"screenshots/home.png\", \"screenshots/detail.webp\"]\n\
             [package.taglines]\nen = \"Notes you write alone or together\"\n",
        );
        assert!(validate_store_listing(&fine).is_ok());
        assert_eq!(
            fine.package.tagline_for("de-DE"),
            Some("Notes you write alone or together"),
            "a language without its own tag line falls back to English"
        );
        for bad in [
            "screenshots = [\"home.png\"]",
            "screenshots = [\"screenshots/../secret.png\"]",
            "screenshots = [\"screenshots/notes.txt\"]",
            "screenshots = [\"screenshots/a.png\", \"screenshots/a.png\"]",
        ] {
            assert!(
                matches!(
                    validate_store_listing(&app_with_listing(bad)),
                    Err(ManifestError::BadScreenshot(_))
                ),
                "{bad}"
            );
        }
        let long = format!("[package.taglines]\nen = \"{}\"", "x".repeat(81));
        assert!(matches!(
            validate_store_listing(&app_with_listing(&long)),
            Err(ManifestError::BadTagline(_))
        ));
    }

    fn minimal_runtime_manifest() -> Manifest {
        let src = r#"
[package]
id = "so.tilo.runtime"
version = "2025.6.0"
kind = "runtime"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.fs]
read = []
write = []

[sandbox.net]
mode = "off"
        "#;
        Manifest::from_toml_str(src).unwrap()
    }

    fn minimal_format_provider_manifest() -> Manifest {
        let mut m = minimal_runtime_manifest();
        m.package.kind = Kind::FormatProvider;
        m.providers.mime = vec!["text/markdown".into()];
        m
    }

    #[test]
    fn validation_stages_preserve_first_error_order() {
        let mut manifest = minimal_runtime_manifest();
        manifest.sandbox.fs.write = vec!["/".into()];
        manifest.sandbox.fs.read = vec!["/etc/shadow".into()];
        manifest.continuity.required = vec![ContinuityFacet::Semantic];
        manifest.package.version = "invalid".into();
        manifest.package.service_protocol = Some(crate::manifest::ServiceProtocol {
            name: "invalid/contract".into(),
            version: 0,
        });
        assert_eq!(
            validate_manifest(&manifest),
            Err(ManifestError::FsWriteDeclared {
                package: manifest.package.id.clone(),
                entry: "/".into(),
            })
        );
        manifest.sandbox.fs.write.clear();
        assert_eq!(
            validate_manifest(&manifest),
            Err(ManifestError::FsReadDeclared {
                package: manifest.package.id.clone(),
                entry: "/etc/shadow".into(),
            })
        );
        manifest.sandbox.fs.read.clear();
        assert_eq!(
            validate_manifest(&manifest),
            Err(ManifestError::SemanticContinuityWithoutPresentation)
        );
        manifest.continuity.required.clear();
        assert!(matches!(
            validate_manifest(&manifest),
            Err(ManifestError::BadVersion(..))
        ));
        manifest.package.version = "2025.6.0".into();
        assert_eq!(
            validate_manifest(&manifest),
            Err(ManifestError::BadServiceProtocol)
        );
        manifest.package.service_protocol = None;
        assert_eq!(validate_manifest(&manifest), Ok(()));
    }

    #[test]
    fn service_wire_contract_is_bounded_and_preserves_legacy_packages() {
        let mut manifest = minimal_runtime_manifest();
        assert!(manifest.package.service_protocol.is_none());
        assert_eq!(validate_manifest(&manifest), Ok(()));
        manifest.package.service_protocol = Some(crate::manifest::ServiceProtocol {
            name: "tilo.home-capture".into(),
            version: 6,
        });
        assert_eq!(validate_manifest(&manifest), Ok(()));
        let encoded = toml::to_string(&manifest).unwrap();
        let parsed = Manifest::from_toml_str(&encoded).unwrap();
        assert_eq!(
            parsed.package.service_protocol,
            manifest.package.service_protocol
        );
        for name in [
            "",
            "Tilo.home-capture",
            "../capture",
            "tilo/capture",
            "tilo.capture-",
            &"a".repeat(129),
        ] {
            manifest.package.service_protocol.as_mut().unwrap().name = name.into();
            assert_eq!(
                validate_manifest(&manifest),
                Err(ManifestError::BadServiceProtocol)
            );
        }
        manifest.package.service_protocol.as_mut().unwrap().name = "tilo.home-capture".into();
        manifest.package.service_protocol.as_mut().unwrap().version = 0;
        assert_eq!(
            validate_manifest(&manifest),
            Err(ManifestError::BadServiceProtocol)
        );
        assert!(
            toml::from_str::<crate::manifest::ServiceProtocol>(
                "name='tilo.home-capture'\nversion=6\nauthority=true"
            )
            .is_err()
        );
    }

    #[test]
    fn runtime_and_provider_manifests_validate_their_kind() {
        // phase: format_provider_payload_needs_provider_module
        {
            let m = minimal_format_provider_manifest();
            assert_eq!(
                validate_manifest_with_payload(&m, &["provider.wasm".into()]),
                Ok(())
            );
            assert_eq!(
                validate_manifest_with_payload(&m, &["component.wasm".into()]),
                Err(ManifestError::FormatProviderMissingModule)
            );
            assert_eq!(
                validate_manifest_with_payload(&m, &[]),
                Err(ManifestError::FormatProviderMissingModule)
            );
        }
        // phase: runtime_kind_with_runtime_field_rejected
        {
            let mut m = minimal_runtime_manifest();
            m.package.runtime = Some("tilo-runtime/2025.06".into());
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::RuntimeKindHasRuntime)
            );
        }
        // phase: calendar_runtime_version_accepted
        {
            let mut m = minimal_runtime_manifest();
            m.package.kind = Kind::App;
            m.package.runtime = Some("tilo-runtime/2025.06".into());
            // Will still fail because no entrypoint is set, but the
            // runtime-version parse should pass before that — make sure the
            // error is App-specific, not runtime-version.
            let e = validate_manifest(&m).unwrap_err();
            assert!(
                matches!(e, ManifestError::AppMissingEntrypoint),
                "expected app-entrypoint error, got {e:?}"
            );
        }
        // phase: an_app_names_the_exact_runtime_build
        {
            let mut m = minimal_runtime_manifest();
            m.package.kind = Kind::App;
            m.package.runtime =
                Some("tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef".into());
            let e = validate_manifest(&m).unwrap_err();
            assert!(
                matches!(e, ManifestError::AppMissingEntrypoint),
                "expected app-entrypoint error, got {e:?}"
            );
        }
        // phase: a_native_runtime_package_provides_one_build
        {
            let mut m = minimal_runtime_manifest();
            m.package.isa.aarch64 = Some(crate::manifest::AArch64Level::Armv8A);
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::RuntimeWithoutProvides)
            );
            m.package.provides = Some("tilo-runtime".into());
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::BadProvides("tilo-runtime".into()))
            );
            m.package.provides = Some("tilo-runtime/2026.10.0-chromium".into());
            assert_eq!(validate_manifest(&m), Ok(()));
            m.package.provides =
                Some("tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef".into());
            assert_eq!(validate_manifest(&m), Ok(()));
            assert_eq!(
                m.package.native_runtime_binding(),
                m.package.provides.as_deref()
            );
            m.package.kind = Kind::Service;
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::ProvidesOnNonRuntime)
            );
        }
        // phase: browser_manifest_names_its_engine_runtime
        {
            let source = include_str!("../../../../apps/browser/tilo-app.toml");
            let manifest = Manifest::from_toml_str(source).expect("browser TOML");
            assert_eq!(
                manifest.package.runtime.as_deref(),
                Some("tilo-runtime/2026.07")
            );
            assert_eq!(
                manifest.package.optional_runtimes,
                ["tilo-runtime/2026.10.17-chromium"]
            );
            assert_eq!(validate_manifest(&manifest), Ok(()));
        }
    }

    #[test]
    fn optional_runtimes_are_runtime_references() {
        let mut m = minimal_runtime_manifest();
        // phase: a_runtime_package_names_no_further_runtimes
        m.package.optional_runtimes = vec!["tilo-runtime/2026.10.0-chromium".into()];
        assert_eq!(
            validate_manifest(&m),
            Err(ManifestError::RuntimeKindHasRuntime)
        );
        m.package.kind = Kind::Service;
        m.package.runtime = Some("tilo-runtime/2026.07".into());
        assert_eq!(validate_manifest(&m), Ok(()));
        // phase: a_malformed_reference_is_refused
        m.package.optional_runtimes = vec!["chromium".into()];
        assert!(matches!(
            validate_manifest(&m),
            Err(ManifestError::BadRuntimeForm(_))
        ));
        // phase: one_bundle_version_is_named_once
        m.package.optional_runtimes = vec![
            "tilo-runtime/2026.10.0-chromium".into(),
            "engine/2026.10.0-chromium".into(),
        ];
        assert!(matches!(
            validate_manifest(&m),
            Err(ManifestError::DuplicateOptionalRuntime(_))
        ));
        m.package.optional_runtimes = vec!["tilo-runtime/2026.07".into()];
        assert!(matches!(
            validate_manifest(&m),
            Err(ManifestError::DuplicateOptionalRuntime(_))
        ));
        // phase: the_list_is_bounded
        m.package.optional_runtimes = (0..=MAX_OPTIONAL_RUNTIMES)
            .map(|minor| format!("tilo-runtime/2026.{}.0", minor + 10))
            .collect();
        assert_eq!(
            validate_manifest(&m),
            Err(ManifestError::TooManyOptionalRuntimes)
        );
    }

    #[test]
    fn required_services_are_the_interfaces_a_package_needs() {
        use crate::manifest::{Requirement, ServiceReq};
        let service = |requirement, version| ServiceReq {
            protocol: String::new(),
            requirement,
            version,
        };
        let mut m = minimal_runtime_manifest();
        m.sandbox.services = [
            ("system".to_owned(), service(Requirement::Required, 2)),
            ("collab".to_owned(), service(Requirement::Optional, 1)),
            (
                "model-gateway".to_owned(),
                service(Requirement::Required, 1),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(validate_manifest(&m), Ok(()));
        assert_eq!(
            m.required_interface_wire(),
            vec!["model-gateway@1".to_owned(), "system@2".to_owned()]
        );
        m.sandbox
            .services
            .insert("Bad Name".to_owned(), service(Requirement::Optional, 1));
        assert!(matches!(
            validate_manifest(&m),
            Err(ManifestError::BadService(_))
        ));
        m.sandbox.services.remove("Bad Name");
        m.sandbox
            .services
            .insert("system".to_owned(), service(Requirement::Required, 0));
        assert!(matches!(
            validate_manifest(&m),
            Err(ManifestError::BadService(_))
        ));
    }

    #[test]
    fn offered_actions_name_stable_addresses_once() {
        let offer = |resource: &str, action: &str| crate::manifest::OfferedAction {
            resource: resource.into(),
            resource_kind: None,
            action: action.into(),
            label: "Show tasks".into(),
            description: None,
            kind: crate::manifest::OfferKind::Read,
            operation: crate::manifest::OfferOperation::View,
            confirm: false,
        };
        let mut m = minimal_runtime_manifest();
        m.offers = vec![
            offer("calendar.calendar", "show-tasks"),
            offer("calendar.calendar", "show-week"),
        ];
        assert_eq!(validate_manifest(&m), Ok(()));
        let refused = |offers: Vec<crate::manifest::OfferedAction>| {
            let mut m = minimal_runtime_manifest();
            m.offers = offers;
            matches!(validate_manifest(&m), Err(ManifestError::BadOffer(_)))
        };
        assert!(refused(vec![offer("calendar.calendar", "show-tasks"); 2]));
        assert!(refused(vec![offer("app.so.tilo.notes", "open")]));
        assert!(refused(vec![offer("tilo-ui-bound:v1:a:b:c", "invoke")]));
        assert!(refused(vec![offer("", "show-tasks")]));
        assert!(refused(vec![offer("calendar calendar", "show-tasks")]));
        assert!(refused(vec![offer("calendar.calendar", "show tasks")]));
        let mut unlabelled = offer("calendar.calendar", "show-tasks");
        unlabelled.label = " ".into();
        assert!(refused(vec![unlabelled]));
        let mut two_lines = offer("calendar.calendar", "show-tasks");
        two_lines.description = Some("one\ntwo".into());
        assert!(refused(vec![two_lines]));
        assert!(refused(
            (0..=MAX_OFFERS)
                .map(|n| offer("calendar.calendar", &format!("a{n}")))
                .collect()
        ));
    }

    #[test]
    fn declared_resource_kinds_must_be_record_kinds() {
        let mut m = minimal_runtime_manifest();
        m.resources.kinds = vec!["example.note".into(), "notes.note-book_2".into()];
        assert_eq!(validate_manifest(&m), Ok(()));
        for bad in ["", "1note", "note book", "note/page", &"k".repeat(65)] {
            m.resources.kinds = vec![bad.to_owned()];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::BadResourceKind(bad.to_owned()))
            );
        }
    }

    #[test]
    fn continuity_declarations_are_validated() {
        // phase: semantic_continuity_requires_presentation
        {
            let mut m = minimal_runtime_manifest();
            m.continuity.required = vec![ContinuityFacet::Semantic];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::SemanticContinuityWithoutPresentation)
            );
        }
        // phase: semantic_continuity_requires_a_signed_portable_schema
        {
            let mut m = minimal_runtime_manifest();
            m.continuity.required = vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::SemanticContinuityWithoutSchema)
            );
        }
        // phase: continuity_facets_cannot_be_repeated
        {
            let mut m = minimal_runtime_manifest();
            m.continuity.required =
                vec![ContinuityFacet::Presentation, ContinuityFacet::Presentation];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::DuplicateContinuityFacet(
                    ContinuityFacet::Presentation
                ))
            );
        }
        // phase: apps_cannot_opt_out_of_structural_presentation
        {
            let mut m = minimal_runtime_manifest();
            m.package.kind = Kind::App;
            m.entrypoint = Some(crate::manifest::Entrypoint {
                bin: "bin/app".into(),
                r#type: EntrypointType::Gui,
                gui_bin: None,
                cli_bin: None,
                splash_segments: Vec::new(),
                splash_budget_ms: 400,
            });
            m.continuity.required.clear();
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::AppContinuityMissingPresentation)
            );
        }
    }
}
