//! Manifest schema validation.

use semver::Version;

use crate::id::Kind;
use crate::manifest::{
    ContinuityFacet, DEFAULT_PLATFORMS, EntrypointType, Manifest, NetMode, SandboxNet,
};

/// File a `kind = "format-provider"` payload must ship: the provider's core
/// WebAssembly module, which the format host checks against its provider ABI
/// when loading it.
pub const FORMAT_PROVIDER_MODULE: &str = "provider.wasm";

/// The payload file carrying an app's model: the WebAssembly module that
/// applies the app's actions to its resources, one per app version and the
/// same bytes on every device (`docs/architecture/apps.md` §1). A package
/// without one is an app whose resources no key holder can apply.
pub const MODEL_MODULE: &str = "model.wasm";

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
    #[error(
        "package.icon {0:?} must be an SVG or PNG file in the payload, named by a relative \
         path without `.` or `..` segments"
    )]
    BadIcon(String),
    #[error(
        "package.previously names {0:?}, which is not an earlier id of the app: it is empty, its own id or given twice"
    )]
    BadPreviously(String),
    #[error(
        "entrypoint.runtime {0:?} must also be listed in package.optional-runtimes, so the \
         runtime installs with the app like every other one it names"
    )]
    EntrypointRuntimeUndeclared(String),
    #[error("only kind=app packages execute a runtime's binary through entrypoint.runtime")]
    EntrypointRuntimeOnNonApp,
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
    #[error("entrypoint.script {0:?} needs entrypoint.runtime, the runtime whose binary runs it")]
    ScriptWithoutRuntime(String),
    #[error("only kind=runtime packages declare `package.provides`")]
    ProvidesOnNonRuntime,
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
    DuplicateRequiredRuntime(String),
    #[error("package.optional-runtimes lists more than {MAX_REQUIRED_RUNTIMES} runtimes")]
    TooManyRequiredRuntimes,
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
    #[error("resources.private names {0:?}, which resources.kinds does not")]
    UndeclaredPrivateKind(String),
    #[error("offers: {0}")]
    BadOffer(String),
    #[error("sandbox.services: {0}")]
    BadService(String),
    #[error("sandbox.portals names {0:?}, which is not a portal")]
    UnknownPortal(String),
    #[error("sandbox.net.hosts {entry:?} is not a declaration: {reason}")]
    BadNetHost { entry: String, reason: &'static str },
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
    #[error("a language pack {0}")]
    LanguagePack(String),
}

/// Validate the manifest against schema rules that don't require
/// touching the payload; payload-side checks live in
/// [`crate::pack::pack`] and [`validate_manifest_with_payload`].
///
/// # Errors
///
/// Returns the specific [`ManifestError`] variant for the first invariant
/// violated. Validation proceeds through platforms and capabilities,
/// continuity and accessibility, package version, required runtimes and
/// runtime entrypoint, runtime dependency and provides, service protocol,
/// kind-specific entrypoint, image metadata, provider/resource/service
/// declarations, publisher identity and signature, then target compatibility.
pub fn validate_manifest(m: &Manifest) -> Result<(), ManifestError> {
    validate_platforms(m).and_then(|()| validate_capabilities(m))?;
    validate_net_hosts(&m.sandbox.net)?;

    validate_store_listing(m)?;
    validate_icon(m)?;
    validate_previously(m)?;

    validate_continuity(m)?;

    validate_atspi_acceptance(m)?;

    Version::parse(&m.package.version)
        .map_err(|e| ManifestError::BadVersion(m.package.version.clone(), e.to_string()))?;

    validate_required_runtimes(m)?;
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

    validate_kind_entrypoint(m)?;
    validate_entrypoint_runtime(m)?;
    validate_image(m)?;
    validate_language_pack(m)?;
    validate_declarations(m)?;

    m.publisher
        .identity_bytes()
        .map_err(|e| ManifestError::BadPublisherIdentity(e.to_string()))?;
    m.publisher
        .signature_bytes()
        .map_err(|e| ManifestError::BadPublisherSignature(e.to_string()))?;
    validate_target(m)
}

/// A manifest host entry's domain, when that domain is a public suffix
/// (`*.co.uk`, `*.github.io`): every name under it is every name someone
/// else may register, not a service's own hosts. The manifest read refuses
/// such an entry, and the launcher's egress proxy drops it from the
/// declaration through this same function.
#[must_use]
pub fn public_suffix_wildcard(entry: &str) -> Option<&str> {
    let entry = entry.trim().trim_end_matches('.');
    let domain = entry
        .strip_prefix("*.")
        .or_else(|| entry.strip_prefix('.'))?;
    let lower = domain.to_ascii_lowercase();
    let suffix = psl::suffix_str(&lower)?;
    suffix.eq_ignore_ascii_case(domain).then_some(domain)
}

/// `[sandbox.net].hosts`: a host name, or `*.domain` for every name under a
/// domain. The wildcard stands only as the leftmost label, covers only names
/// under the domain (the domain itself needs its own entry), and cannot name
/// a public suffix or an address literal. Only an `allowlist` reads them.
pub fn validate_net_hosts(net: &SandboxNet) -> Result<(), ManifestError> {
    use collab_types::caps::{HostPattern, NetHostError};

    if !matches!(net.mode, NetMode::Allowlist) {
        return Ok(());
    }
    for entry in &net.hosts {
        let refuse = |reason| ManifestError::BadNetHost {
            entry: entry.clone(),
            reason,
        };
        let host = entry.trim().trim_end_matches('.');
        if host.is_empty() {
            return Err(refuse("the entry is empty"));
        }
        if host == "*" {
            return Err(refuse(
                "a bare * names every host; declare `mode = \"host\"` instead",
            ));
        }
        if let Some(domain) = host.strip_prefix("*.") {
            if domain.contains('*') {
                return Err(refuse(
                    "a wildcard may stand only as the leftmost label (*.example.com)",
                ));
            }
            if domain.is_empty() {
                return Err(refuse("*. names no domain"));
            }
            if public_suffix_wildcard(host).is_some() {
                return Err(refuse(
                    "the domain is a public suffix; name the hosts under it",
                ));
            }
            if HostPattern::subdomains_only(domain).is_err() {
                return Err(refuse("the domain after *. is not a valid host name"));
            }
        } else {
            if host.contains('*') {
                return Err(refuse(
                    "a wildcard may stand only as the leftmost label (*.example.com)",
                ));
            }
            if host.starts_with('.') {
                return Err(refuse(
                    "a domain with everything under it is written *.example.com",
                ));
            }
            match HostPattern::parse(host) {
                Ok(_) => {}
                Err(NetHostError::Address(_)) => {
                    return Err(refuse(
                        "an address literal is not a host; network is granted by name",
                    ));
                }
                Err(_) => return Err(refuse("it is not a valid host name")),
            }
        }
    }
    Ok(())
}

/// The ids an app was published under before are other ids, each named once.
fn validate_previously(m: &Manifest) -> Result<(), ManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for old in &m.package.previously {
        if old.trim().is_empty() || *old == m.package.id || !seen.insert(old.as_str()) {
            return Err(ManifestError::BadPreviously(old.clone()));
        }
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
        Kind::Service
        | Kind::Plugin
        | Kind::Runtime
        | Kind::EnvRuntime
        | Kind::Tool
        | Kind::LanguagePack => {}
    }
    Ok(())
}

fn validate_language_pack(m: &Manifest) -> Result<(), ManifestError> {
    let reserved = m.package.id.starts_with(crate::id::LANGUAGE_PACK_ID_PREFIX);
    if m.package.kind != Kind::LanguagePack {
        return if reserved {
            Err(ManifestError::LanguagePack(format!(
                "id prefix {} is for kind=language-pack only",
                crate::id::LANGUAGE_PACK_ID_PREFIX
            )))
        } else {
            Ok(())
        };
    }
    if crate::id::language_pack_locale(&m.package.id).is_none() {
        return Err(ManifestError::LanguagePack(format!(
            "is named {}<locale>, the locale in lower case, not {:?}",
            crate::id::LANGUAGE_PACK_ID_PREFIX,
            m.package.id
        )));
    }
    let present = [
        ("[entrypoint]", m.entrypoint.is_some()),
        ("[cli]", m.cli.is_some()),
        ("[target]", m.target.is_some()),
        ("package.runtime", m.package.runtime.is_some()),
        (
            "package.optional-runtimes",
            !m.package.required_runtimes.is_empty(),
        ),
        ("[package.isa]", !m.package.isa.is_empty()),
        (
            "[providers]",
            !(m.providers.mime.is_empty()
                && m.providers.ext.is_empty()
                && m.providers.schemes.is_empty()
                && m.providers.resources.is_empty()),
        ),
    ];
    if let Some((block, _)) = present.iter().find(|(_, present)| *present) {
        return Err(ManifestError::LanguagePack(format!(
            "is data for every architecture and is never launched; remove `{block}`"
        )));
    }
    Ok(())
}

/// The payload of language pack `m`: only `<locale>/<catalogue>.ftl`, the
/// locale directory naming the pack's locale (in any case).
fn validate_language_pack_payload(
    m: &Manifest,
    payload_files: &[std::path::PathBuf],
) -> Result<(), ManifestError> {
    let Some(locale) = crate::id::language_pack_locale(&m.package.id) else {
        return Ok(());
    };
    if payload_files.is_empty() {
        return Err(ManifestError::LanguagePack("carries no catalogue".into()));
    }
    for path in payload_files {
        let text = path.to_str().map(|path| path.replace('\\', "/"));
        let fits = text
            .as_deref()
            .and_then(|path| path.split_once('/'))
            .is_some_and(|(directory, file)| {
                directory.eq_ignore_ascii_case(locale)
                    && file.strip_suffix(".ftl").is_some_and(|name| {
                        !name.is_empty()
                            && name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    })
            });
        if !fits {
            return Err(ManifestError::LanguagePack(format!(
                "carries only `{locale}/<catalogue>.ftl`, not {}",
                path.display()
            )));
        }
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
                && m.providers.schemes.is_empty()
                && m.providers.resources.is_empty()),
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

fn validate_declarations(m: &Manifest) -> Result<(), ManifestError> {
    for ext in &m.providers.ext {
        if !valid_extension(ext) {
            return Err(ManifestError::BadProviderExtension(ext.clone()));
        }
    }
    for mime in &m.providers.mime {
        if !tilo_mime::valid_mime(mime) {
            return Err(ManifestError::BadProviderMime(mime.clone()));
        }
    }
    for scheme in &m.providers.schemes {
        if !valid_scheme(scheme) {
            return Err(ManifestError::BadProviderScheme(scheme.clone()));
        }
    }
    for kind in m.resources.kinds.iter().chain(&m.providers.resources) {
        if !valid_resource_kind(kind) {
            return Err(ManifestError::BadResourceKind(kind.clone()));
        }
    }
    if let Some(kind) = m
        .resources
        .private
        .iter()
        .find(|kind| !m.resources.kinds.contains(kind))
    {
        return Err(ManifestError::UndeclaredPrivateKind(kind.clone()));
    }
    validate_offers(&m.offers)?;
    if let Some(name) = m.sandbox.portals.keys().find(|name| {
        !crate::manifest::PORTALS
            .iter()
            .any(|(known, _)| known == name)
    }) {
        return Err(ManifestError::UnknownPortal(name.clone()));
    }
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

fn validate_icon(m: &Manifest) -> Result<(), ManifestError> {
    let Some(icon) = &m.package.icon else {
        return Ok(());
    };
    let image = std::path::Path::new(icon)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("png"));
    if crate::package::is_safe_path(icon) && image && !icon.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(ManifestError::BadIcon(icon.clone()))
    }
}

fn validate_entrypoint_runtime(m: &Manifest) -> Result<(), ManifestError> {
    let Some(entrypoint) = &m.entrypoint else {
        return Ok(());
    };
    let Some(runtime) = &entrypoint.runtime else {
        return match &entrypoint.script {
            Some(script) => Err(ManifestError::ScriptWithoutRuntime(
                script.display().to_string(),
            )),
            None => Ok(()),
        };
    };
    if m.package.kind != Kind::App {
        return Err(ManifestError::EntrypointRuntimeOnNonApp);
    }
    validate_runtime_string(runtime)?;
    if !m
        .package
        .required_runtimes
        .iter()
        .any(|declared| declared == runtime)
    {
        return Err(ManifestError::EntrypointRuntimeUndeclared(runtime.clone()));
    }
    for bin in [
        Some(&entrypoint.bin),
        entrypoint.gui_bin.as_ref(),
        entrypoint.cli_bin.as_ref(),
        entrypoint.script.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter(|bin| !bin.as_os_str().is_empty())
    {
        if bin
            .to_str()
            .is_none_or(|path| !crate::package::is_safe_path(path))
        {
            return Err(ManifestError::RuntimeEntrypoint(bin.display().to_string()));
        }
    }
    Ok(())
}

pub const MAX_SCREENSHOTS: usize = 8;

pub const MAX_TAGLINE_CHARS: usize = 80;

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

pub(crate) fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    !scheme.is_empty()
        && scheme.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !matches!(scheme, "file" | "data" | "javascript")
}

fn valid_resource_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind.bytes().enumerate().all(|(i, byte)| {
            (byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && (i != 0 || byte.is_ascii_alphabetic())
        })
}

/// The most actions one app may offer while it is closed. Its offers are the app's own
/// catalogue, which for a big app (Settings' is 237) reaches hundreds, so the bound stands
/// far above any catalogue: it only keeps one app from being mostly offers, which a
/// manifest's own 1 MiB bound would otherwise allow.
pub const MAX_OFFERS: usize = 1024;

/// Resource prefixes an app cannot offer under: `app.` names the host's own
/// application resources (opening and closing an app), a `tilo-ui-bound:` id belongs to
/// one window of one app process, and a `uuid:` or `file:` address is minted or named on
/// one device, so none of them means anything before the app is open elsewhere.
const RESERVED_OFFER_RESOURCES: [&str; 4] = ["app.", "tilo-ui-bound:", "uuid:", "file:"];

/// The tab id of the workspace's default resource (`DocumentId::workspace_default`), the
/// base of every address a device derives before an app is open.
const WORKSPACE_DEFAULT_TAB: &str = "0000000000000000";

/// Whether `resource` is an address an app offers under steadily: one it names itself
/// (`calendar.calendar`), one of its items (`chat.messages:<item>`), or one derived from
/// the workspace's default resource
/// (`chain:derived:pages@derived:default@tab:<default>`), which every device derives the
/// same way and which the app shows when a launch names no resource, as a palette
/// invoking an offer launches it. A window's own, a minted one and a file's are another
/// device's.
fn stable_offer_resource(resource: &str) -> bool {
    let allowed = |byte: u8| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'@')
    };
    resource.len() <= 128
        && resource.starts_with(|c: char| c.is_ascii_alphabetic())
        && resource.bytes().all(allowed)
        && !RESERVED_OFFER_RESOURCES
            .iter()
            .any(|reserved| resource.starts_with(reserved))
        && resource.match_indices("tab:").all(|(at, _)| {
            resource
                .get(at..)
                .and_then(|tail| tail.strip_prefix("tab:"))
                .and_then(|tail| tail.split(|c: char| !c.is_ascii_hexdigit()).next())
                == Some(WORKSPACE_DEFAULT_TAB)
        })
}

fn validate_offers(offers: &[crate::manifest::OfferedAction]) -> Result<(), ManifestError> {
    if offers.len() > MAX_OFFERS {
        return Err(ManifestError::BadOffer(format!(
            "more than {MAX_OFFERS} offered actions"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for offer in offers {
        let address = format!("{}#{}", offer.resource, offer.action);
        if !stable_offer_resource(&offer.resource) {
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

pub const MAX_REQUIRED_RUNTIMES: usize = 4;

fn validate_required_runtimes(m: &Manifest) -> Result<(), ManifestError> {
    let required = &m.package.required_runtimes;
    if required.is_empty() {
        return Ok(());
    }
    if m.package.kind == Kind::Runtime {
        return Err(ManifestError::RuntimeKindHasRuntime);
    }
    if required.len() > MAX_REQUIRED_RUNTIMES {
        return Err(ManifestError::TooManyRequiredRuntimes);
    }
    let mut seen = std::collections::BTreeSet::new();
    for raw in required {
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
            return Err(ManifestError::DuplicateRequiredRuntime(raw.clone()));
        }
    }
    Ok(())
}

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
    if m.package.kind == Kind::LanguagePack {
        validate_language_pack_payload(m, payload_files)?;
    }
    if let Some(target) = &m.target
        && !payload_files.iter().any(|path| {
            path.to_str()
                .is_some_and(|path| path.replace('\\', "/") == target.module)
        })
    {
        return Err(ManifestError::TargetModuleMissing(target.module.clone()));
    }
    if let Some(icon) = &m.package.icon
        && !payload_files.iter().any(|path| {
            path.to_str()
                .is_some_and(|path| path.replace('\\', "/") == icon.as_str())
        })
    {
        return Err(ManifestError::BadIcon(icon.clone()));
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

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#
        );
        Manifest::from_toml_str(&src).unwrap()
    }

    fn language_pack(id: &str, extra: &str) -> Manifest {
        let src = format!(
            r#"
[package]
id = "{id}"
version = "2026.10.20"
kind = "language-pack"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""
{extra}
[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#
        );
        Manifest::from_toml_str(&src).unwrap()
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

    tilo_test_env::scenario! {
        fn runtime_and_provider_manifests_validate_their_kind;

        phase a_language_pack_is_one_locales_catalogues_and_nothing_else;
        // A language pack is one locale's catalogues: named by the locale, for
        // every architecture, nothing in it launched; its payload is only that
        // locale's `.ftl` files, and its id prefix is no other kind's.
        {
            let pack = language_pack("so.tilo.language-pack.de-de", "");
            assert!(validate_language_pack(&pack).is_ok());
            assert_eq!(crate::language_pack_locale(&pack.package.id), Some("de-de"));
            let catalogues = [
                std::path::PathBuf::from("de-DE/settings.ftl"),
                std::path::PathBuf::from("de-DE/tilo-ui.ftl"),
            ];
            assert!(validate_language_pack_payload(&pack, &catalogues).is_ok());
            for wrong in ["fr-FR/settings.ftl", "de-DE/bin/tool", "settings.ftl"] {
                assert!(
                    validate_language_pack_payload(&pack, &[std::path::PathBuf::from(wrong)]).is_err(),
                    "{wrong}"
                );
            }
            assert!(validate_language_pack_payload(&pack, &[]).is_err());

            for wrong_id in [
                "so.tilo.language-pack.",
                "so.tilo.language-pack.de-DE",
                "so.tilo.de-de",
            ] {
                assert!(
                    validate_language_pack(&language_pack(wrong_id, "")).is_err(),
                    "{wrong_id}"
                );
            }
            let launched = language_pack(
                "so.tilo.language-pack.de-de",
                "[entrypoint]\nbin = \"bin/x\"\n",
            );
            assert!(validate_language_pack(&launched).is_err());
            let arch_bound = language_pack(
                "so.tilo.language-pack.de-de",
                "[package.isa]\nx86_64 = \"v3\"\n",
            );
            assert!(validate_language_pack(&arch_bound).is_err());

            let mut app = app_with_listing("");
            app.package.id = "so.tilo.language-pack.de-de".into();
            assert!(validate_language_pack(&app).is_err());
        }

        phase an_entrypoint_runs_a_runtime_binary_only_when_the_runtime_is_declared;
        {
            let declared = |optional: &str| {
                let mut m = app_with_listing(optional);
                if let Some(entrypoint) = m.entrypoint.as_mut() {
                    entrypoint.runtime = Some("tilolang/0.1.0".into());
                    entrypoint.bin = "bin/tilolang".into();
                }
                m
            };
            assert_eq!(
                validate_manifest(&declared("optional-runtimes = [\"tilolang/0.1.0\"]")),
                Ok(())
            );
            assert_eq!(
                validate_manifest(&declared("")),
                Err(ManifestError::EntrypointRuntimeUndeclared(
                    "tilolang/0.1.0".into()
                ))
            );
            assert_eq!(
                validate_manifest(&declared("optional-runtimes = [\"tilolang/0.2.0\"]")),
                Err(ManifestError::EntrypointRuntimeUndeclared(
                    "tilolang/0.1.0".into()
                ))
            );
            let mut outside = declared("optional-runtimes = [\"tilolang/0.1.0\"]");
            if let Some(entrypoint) = outside.entrypoint.as_mut() {
                entrypoint.bin = "../bin/sh".into();
            }
            assert_eq!(
                validate_manifest(&outside),
                Err(ManifestError::RuntimeEntrypoint("../bin/sh".into()))
            );
            let mut malformed = declared("optional-runtimes = [\"tilolang/0.1.0\"]");
            if let Some(entrypoint) = malformed.entrypoint.as_mut() {
                entrypoint.runtime = Some("tilolang".into());
            }
            assert_eq!(
                validate_manifest(&malformed),
                Err(ManifestError::BadRuntimeForm("tilolang".into()))
            );
            // The schema round-trips: what a packer writes, an installer reads.
            let m = declared("optional-runtimes = [\"tilolang/0.1.0\"]\nicon = \"icon.svg\"");
            let text = toml::to_string(&m).unwrap();
            let back = Manifest::from_toml_str(&text).unwrap();
            assert_eq!(back.package.icon.as_deref(), Some("icon.svg"));
            assert_eq!(
                back.entrypoint
                    .and_then(|entrypoint| entrypoint.runtime)
                    .as_deref(),
                Some("tilolang/0.1.0")
            );
        }

        phase format_provider_payload_needs_provider_module;
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

        phase runtime_kind_with_runtime_field_rejected;
        {
            let mut m = minimal_runtime_manifest();
            m.package.runtime = Some("tilo-runtime/2025.06".into());
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::RuntimeKindHasRuntime)
            );
        }

        phase an_apps_runtime_reference_parses_before_its_entrypoint_is_checked;
        {
            // Both spellings a runtime reference has are accepted: a calendar
            // release, and the exact native build a device session produced.
            for runtime in [
                "tilo-runtime/2025.06",
                "tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef",
            ] {
                let mut m = minimal_runtime_manifest();
                m.package.kind = Kind::App;
                m.package.runtime = Some(runtime.into());
                let e = validate_manifest(&m).unwrap_err();
                assert!(
                    matches!(e, ManifestError::AppMissingEntrypoint),
                    "{runtime}: expected app-entrypoint error, got {e:?}"
                );
            }
        }

        phase a_native_runtime_package_provides_one_build;
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

        phase browser_manifest_names_its_engine_runtime;
        {
            let source =
                include_str!("../../../engines/chromium-host/browser.tilo-app.toml");
            let manifest = Manifest::from_toml_str(source).expect("browser TOML");
            assert_eq!(validate_manifest(&manifest), Ok(()));
            assert!(
                manifest.package.runtime.is_some(),
                "the browser names its runtime"
            );
            assert!(
                manifest
                    .package
                    .required_runtimes
                    .iter()
                    .any(|runtime| runtime.contains("chromium")),
                "the browser names the engine it renders with: {:?}",
                manifest.package.required_runtimes
            );
        }

        phase required_runtimes_are_runtime_references;
        {
            let mut m = minimal_runtime_manifest();
            // phase: a_runtime_package_names_no_further_runtimes
            m.package.required_runtimes = vec!["tilo-runtime/2026.10.0-chromium".into()];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::RuntimeKindHasRuntime)
            );
            m.package.kind = Kind::Service;
            m.package.runtime = Some("tilo-runtime/2026.07".into());
            assert_eq!(validate_manifest(&m), Ok(()));
            // phase: a_malformed_reference_is_refused
            m.package.required_runtimes = vec!["chromium".into()];
            assert!(matches!(
                validate_manifest(&m),
                Err(ManifestError::BadRuntimeForm(_))
            ));
            // phase: one_bundle_version_is_named_once
            m.package.required_runtimes = vec![
                "tilo-runtime/2026.10.0-chromium".into(),
                "engine/2026.10.0-chromium".into(),
            ];
            assert!(matches!(
                validate_manifest(&m),
                Err(ManifestError::DuplicateRequiredRuntime(_))
            ));
            m.package.required_runtimes = vec!["tilo-runtime/2026.07".into()];
            assert!(matches!(
                validate_manifest(&m),
                Err(ManifestError::DuplicateRequiredRuntime(_))
            ));
            // phase: the_list_is_bounded
            m.package.required_runtimes = (0..=MAX_REQUIRED_RUNTIMES)
                .map(|minor| format!("tilo-runtime/2026.{}.0", minor + 10))
                .collect();
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::TooManyRequiredRuntimes)
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_manifests_listing_services_offers_and_resource_kinds_are_validated;

        phase the_icon_is_one_svg_or_png_of_the_payload;
        {
            let fine = app_with_listing("icon = \"icon.svg\"");
            assert_eq!(validate_manifest(&fine), Ok(()));
            assert_eq!(
                validate_manifest_with_payload(&fine, &["icon.svg".into()]),
                Ok(())
            );
            assert_eq!(
                validate_manifest_with_payload(&fine, &["bin/example".into()]),
                Err(ManifestError::BadIcon("icon.svg".into())),
                "an icon the payload does not carry"
            );
            for bad in ["/icon.svg", "../icon.svg", "icon.exe", "a/./icon.png", ""] {
                let m = app_with_listing(&format!("icon = {bad:?}"));
                assert_eq!(
                    validate_manifest(&m),
                    Err(ManifestError::BadIcon(bad.into())),
                    "{bad:?}"
                );
            }
            assert_eq!(
                validate_manifest(&app_with_listing("icon = \"share/Icon.PNG\"")),
                Ok(())
            );
        }

        phase taglines_are_one_short_line_and_screenshots_live_under_their_directory;
        {
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

        phase validation_stages_preserve_first_error_order;
        {
            let mut manifest = minimal_runtime_manifest();
            manifest.continuity.required = vec![ContinuityFacet::Semantic];
            manifest.package.version = "invalid".into();
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
            assert_eq!(validate_manifest(&manifest), Ok(()));
        }

        phase required_services_are_the_interfaces_a_package_needs;
        {
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
            m.sandbox.services.clear();
            // A portal name the launcher's table does not know is refused.
            let unknown = "notifications".to_owned();
            m.sandbox.portals.insert(unknown.clone(), crate::manifest::PortalPolicy::Deny);
            assert_eq!(validate_manifest(&m), Err(ManifestError::UnknownPortal(unknown)));
        }

        phase offered_actions_name_stable_addresses_once;
        {
            let offer = |resource: &str, action: &str| crate::manifest::OfferedAction {
                resource: resource.into(),
                resource_kind: None,
                action: action.into(),
                label: "Show tasks".into(),
                description: None,
                kind: crate::manifest::OfferKind::Read,
                operation: crate::manifest::OfferOperation::View,
                confirm: false,
                asks: None,
                keywords: Vec::new(),
                icon: None,
            };
            let mut m = minimal_runtime_manifest();
            // A resource derived from the workspace's default one: every device derives
            // the same address, and a launch naming no resource shows it. Its kind is its
            // own.
            let mut derived = offer(
                "chain:derived:pages@derived:default@tab:0000000000000000",
                "new-note",
            );
            derived.resource_kind = Some("notes.pages".into());
            // An item's stable address, whose kind is the resource type holding it.
            let mut item = offer("chat.messages:general", "open");
            item.resource_kind = Some("chat.messages".into());
            m.offers = vec![
                offer("calendar.calendar", "show-tasks"),
                offer("calendar.calendar", "show-week"),
                derived,
                item,
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
            assert!(refused(vec![
                offer("chain:derived:pages@derived:default@tab:0000000000000001", "new-note")
            ]));
            assert!(refused(vec![offer("uuid:01234567-89ab-cdef-0123-456789abcdef", "open")]));
            assert!(refused(vec![offer("file:/tmp/plan.md", "open")]));
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

        phase declared_resource_kinds_must_be_record_kinds;
        {
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
            m.resources.kinds = vec!["example.note".into()];
            m.resources.private = vec!["example.draft".into()];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::UndeclaredPrivateKind("example.draft".into()))
            );
        }
    }

    tilo_test_env::scenario! {
        fn continuity_declarations_are_validated;

        phase continuity_facets_are_dependent_unique_and_never_absent_from_an_app;
        {
            let mut m = minimal_runtime_manifest();
            m.continuity.required = vec![ContinuityFacet::Semantic];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::SemanticContinuityWithoutPresentation)
            );

            m.continuity.required = vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::SemanticContinuityWithoutSchema)
            );

            m.continuity.required =
                vec![ContinuityFacet::Presentation, ContinuityFacet::Presentation];
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::DuplicateContinuityFacet(
                    ContinuityFacet::Presentation
                ))
            );
        }

        phase apps_cannot_opt_out_of_structural_presentation;
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
                runtime: None,
                script: None,
                origins: Vec::new(),
                connection: false,
                window: crate::manifest::WindowKind::default(),
            });
            m.continuity.required.clear();
            assert_eq!(
                validate_manifest(&m),
                Err(ManifestError::AppContinuityMissingPresentation)
            );
        }
    }
}
