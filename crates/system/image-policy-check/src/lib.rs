//! `tilo-image-policy-check`: proves that `containers-policy.json` makes every
//! Tilo image scope require a release signature this image trusts, and refuses
//! everything it does not name.
//!
//! ```text
//! tilo-image-policy-check [POLICY [TRUST_DIR]]
//!     POLICY     default /etc/containers/policy.json
//!     TRUST_DIR  default /usr/share/tilo/release (trust.env, root.crt)
//! tilo-image-policy-check --unsigned POLICY [BOOTC_STATUS]
//!     every way POLICY lets an image in unsigned, and whether the booted
//!     image's repository (from `bootc status --json` saved at BOOTC_STATUS)
//!     takes unsigned updates; one fact per line
//! ```
//!
//! Exits 0 only when the parsed policy proves the requirement. Anything this
//! check cannot prove (an unreadable file, malformed JSON, a duplicate key, an
//! unknown requirement field, missing release trust) is a failure, never a
//! pass: exit 1 with the reason on stdout, exit 2 for a usage error. The boot
//! security audit (`tilo-security-audit`) runs it; it only reads, never
//! repairs.
//!
//! The release requirement depends on the installed trust. Without
//! `TRUST_DIR/root.crt` it is the pinned release key (`keyPath`); with it, a
//! certificate chain to that offline root (`fulcio`: the e-mail identity
//! `os-images@<domain>` and the OIDC-issuer value of the current epoch from
//! `TRUST_DIR/trust.env`). An image that carries the root but whose policy
//! still pins the bare key has been downgraded, and fails. Either way the
//! signature must carry a transparency-log entry (`rekorPublicKeyPath`).
//!
//! The rules mirror containers-policy.json(5): requirements in a scope's list
//! are `AND`ed, and the most specific docker scope wins. So every required
//! scope must exist and hold only release requirements, and no scope at or
//! beneath a Tilo namespace (a tag, a digest, a sibling repository, the
//! namespace itself) may accept anything less than that or an outright
//! reject. The repositories Tilo publishes but a machine must never run
//! unsigned are checked by their effective rule, whichever scope supplies it.
//!
//! The default must be a reject, and `insecureAcceptAnything` may appear only
//! where `tools/signing/release_trust.py` (which writes the policy, with the
//! reasons) allows it: public registries a person pulls from by choice, and
//! local transports that fetch nothing.

mod json;

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

pub use json::{Json, parse};

pub const DEFAULT_POLICY: &str = "/etc/containers/policy.json";
pub const DEFAULT_TRUST_DIR: &str = "/usr/share/tilo/release";
pub const PINNED_KEY: &str = "/usr/share/tilo/cosign.pub";
pub const INSTALLED_ROOT: &str = "/usr/share/tilo/release/root.crt";
pub const INSTALLED_REKOR: &str = "/usr/share/tilo/release/rekor.pub";
/// The operational-certificate purpose OS images and the legacy guest are
/// signed under.
pub const PURPOSE: &str = "os-images";
/// The scopes Tilo OS images are pulled and updated from.
pub const REQUIRED_SCOPES: [&str; 2] = ["registry.tilo.so/tilo/tilo", "code.tilo.so/tilo/tilo"];
/// Every scope under these namespaces is Tilo-published: it may be refused
/// outright, or it must require the release signature.
pub const TILO_NAMESPACES: [&str; 2] = ["registry.tilo.so/tilo/", "code.tilo.so/tilo/"];
/// Broader scopes that would also govern a Tilo repository with no scope of
/// its own.
pub const TILO_NAMESPACE_ROOTS: [&str; 4] = [
    "registry.tilo.so",
    "registry.tilo.so/tilo",
    "code.tilo.so",
    "code.tilo.so/tilo",
];
/// Tilo repositories whose effective rule must be the release signature or a
/// reject, whichever scope supplies it.
pub const GUARDED_REPOSITORIES: [&str; 7] = [
    "registry.tilo.so/tilo/legacy-guest",
    "registry.tilo.so/tilo/tilo-vm",
    "registry.tilo.so/tilo/tilo-ci",
    "registry.tilo.so/tilo/tilo-dev",
    "code.tilo.so/tilo/tilo-vm",
    "code.tilo.so/tilo/tilo-ci",
    "code.tilo.so/tilo/tilo-dev",
];
/// Docker scopes that may accept anything, and the scopes beneath them: the
/// public registries a person pulls from by choice. Mirrors
/// `USER_CHOSEN_REGISTRIES` in `tools/signing/release_trust.py`, which gives
/// the reason.
pub const USER_CHOSEN_REGISTRIES: [&str; 9] = [
    "docker.io",
    "quay.io",
    "ghcr.io",
    "gcr.io",
    "mcr.microsoft.com",
    "public.ecr.aws",
    "registry.fedoraproject.org",
    "registry.access.redhat.com",
    "registry.gitlab.com",
];
/// Transports whose `""` scope may accept anything: they fetch nothing.
/// Mirrors `LOCAL_TRANSPORTS` in `tools/signing/release_trust.py`.
pub const LOCAL_TRANSPORTS: [&str; 5] = [
    "containers-storage",
    "dir",
    "docker-archive",
    "oci",
    "oci-archive",
];
/// Identities that bind a signature to the image's own repository.
/// `remapIdentity`, `exactReference` and `exactRepository` can accept a
/// signature made for some other image, so they do not prove the requirement.
const ACCEPTED_IDENTITIES: [&str; 3] = ["matchRepository", "matchExact", "matchRepoDigestOrExact"];
const KEY_FIELDS: [&str; 4] = ["type", "keyPath", "rekorPublicKeyPath", "signedIdentity"];
const CHAIN_FIELDS: [&str; 4] = ["type", "fulcio", "rekorPublicKeyPath", "signedIdentity"];
const FULCIO_FIELDS: [&str; 3] = ["caPath", "oidcIssuer", "subjectEmail"];

/// What this image's release signature is anchored in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// The pinned release key, `PINNED_KEY`.
    Key,
    /// A certificate for `identity`, of the epoch `issuer` names, issued by
    /// the offline root `INSTALLED_ROOT`.
    Chain { issuer: String, identity: String },
}

impl Trust {
    /// Reads `trust.env` text (strict `TILO_RELEASE_*=value` lines, `#`
    /// comments) for an image that carries the root (`chain`) or not.
    ///
    /// # Errors
    /// Any line or value it cannot use.
    pub fn parse(text: &str, chain: bool) -> Result<Self, String> {
        let mut values: Vec<(&str, &str)> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            if line.trim().is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((name, value)) = line.split_once('=') else {
                return Err(format!("unexpected line {}", json::repr_str(line)));
            };
            let valid_name = name.strip_prefix("TILO_RELEASE_").is_some_and(|rest| {
                !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
            });
            if !valid_name {
                return Err(format!("unexpected line {}", json::repr_str(line)));
            }
            values.retain(|(known, _)| *known != name);
            values.push((name, value));
        }
        let get = |name: &str| {
            values
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| *value)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("no {name}"))
        };
        let epoch_text = get("TILO_RELEASE_EPOCH")?;
        let epoch = epoch_text
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| epoch_text.parse::<u64>().ok())
            .flatten()
            .filter(|epoch| *epoch >= 1)
            .ok_or_else(|| "TILO_RELEASE_EPOCH must be a positive integer".to_owned())?;
        let prefix = get("TILO_RELEASE_ISSUER_PREFIX")?;
        let domain = get("TILO_RELEASE_IDENTITY_DOMAIN")?;
        Ok(if chain {
            Self::Chain {
                issuer: format!("{prefix}{epoch}"),
                identity: format!("{PURPOSE}@{domain}"),
            }
        } else {
            Self::Key
        })
    }

    /// The trust installed in `dir`: `dir/trust.env`, and chain mode when
    /// `dir/root.crt` exists.
    ///
    /// # Errors
    /// An unreadable or unusable `trust.env`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("trust.env");
        let shown = path.display().to_string();
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "{shown}: cannot read release trust: {}",
                os_message(&error, &shown)
            )
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            format!(
                "{shown}: cannot read release trust: {}",
                decode_message(&bytes, &error)
            )
        })?;
        let root = dir.join("root.crt");
        let chain = root.symlink_metadata().is_ok();
        Self::parse(text, chain).map_err(|message| format!("{shown}: {message}"))
    }

    fn describe(&self) -> String {
        match self {
            Self::Key => format!("the {PINNED_KEY} sigstore signature, logged in Rekor"),
            Self::Chain { issuer, identity } => format!(
                "a Rekor-logged sigstore signature by an operational certificate for {identity} of {issuer} under {INSTALLED_ROOT}"
            ),
        }
    }
}

fn is_accept(requirement: &Json) -> bool {
    requirement.get("type").and_then(Json::as_str) == Some("insecureAcceptAnything")
}

/// Why `value` under `name` is not `expected`, or `None`.
fn pinned(name: &str, value: Option<&Json>, expected: &str) -> Option<String> {
    (value.and_then(Json::as_str) != Some(expected)).then(|| {
        format!(
            "{name} is {}, not {}",
            json::repr(value),
            json::repr_str(expected)
        )
    })
}

/// Why the anchor fields (`keyPath` or `fulcio`) do not name this image's
/// release trust, or `None`.
fn anchor_problem(key_path: Option<&Json>, fulcio: Option<&Json>, trust: &Trust) -> Option<String> {
    match trust {
        Trust::Key => {
            if let Some(problem) = pinned("keyPath", key_path, PINNED_KEY) {
                return Some(problem);
            }
        }
        Trust::Chain { issuer, identity } => {
            let Some(Json::Object(fulcio)) = fulcio else {
                return Some("fulcio must be an object".to_owned());
            };
            let field = |key: &str| fulcio.iter().find(|(k, _)| k == key).map(|(_, v)| v);
            let mut extra: Vec<&str> = fulcio
                .iter()
                .map(|(k, _)| k.as_str())
                .filter(|k| !FULCIO_FIELDS.contains(k))
                .collect();
            extra.sort_unstable();
            extra.dedup();
            if !extra.is_empty() {
                let shown: Vec<String> = extra.iter().map(|k| json::repr_str(k)).collect();
                return Some(format!(
                    "fulcio carries unexpected fields [{}]",
                    shown.join(", ")
                ));
            }
            for (name, expected) in [
                ("fulcio.caPath", INSTALLED_ROOT),
                ("fulcio.oidcIssuer", issuer.as_str()),
                ("fulcio.subjectEmail", identity.as_str()),
            ] {
                let key = name.trim_start_matches("fulcio.");
                if let Some(problem) = pinned(name, field(key), expected) {
                    return Some(problem);
                }
            }
        }
    }
    None
}

/// Why a requirement is not the release signature, or `None`.
///
/// # Errors
/// A `signedIdentity` type that is a list or an object, which the original
/// could not even compare (an unhashable value).
fn signed_requirement_problem(requirement: &Json, trust: &Trust) -> Result<Option<String>, String> {
    let Json::Object(fields) = requirement else {
        return Ok(Some("requirement is not an object".to_owned()));
    };
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let kind = get("type");
    if kind.and_then(Json::as_str) != Some("sigstoreSigned") {
        return Ok(Some(format!(
            "requirement type is {}, not 'sigstoreSigned'",
            json::repr(kind)
        )));
    }
    match trust {
        Trust::Chain { .. } if get("keyPath").is_some() => {
            return Ok(Some(format!(
                "pins a bare key, but this image's release trust is the root {INSTALLED_ROOT}"
            )));
        }
        Trust::Key if get("fulcio").is_some() => {
            return Ok(Some(format!(
                "chains to a root certificate, but this image installs no {INSTALLED_ROOT}"
            )));
        }
        _ => {}
    }
    let allowed: &[&str] = match trust {
        Trust::Key => &KEY_FIELDS,
        Trust::Chain { .. } => &CHAIN_FIELDS,
    };
    let mut extra: Vec<&str> = fields
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !allowed.contains(k))
        .collect();
    extra.sort_unstable();
    extra.dedup();
    if !extra.is_empty() {
        let shown: Vec<String> = extra.iter().map(|k| json::repr_str(k)).collect();
        return Ok(Some(format!(
            "sigstoreSigned carries unexpected fields [{}]",
            shown.join(", ")
        )));
    }
    if let Some(problem) = anchor_problem(get("keyPath"), get("fulcio"), trust) {
        return Ok(Some(problem));
    }
    if get("rekorPublicKeyPath").is_none() {
        return Ok(Some(
            "no rekorPublicKeyPath, so the signature need not be in the transparency log"
                .to_owned(),
        ));
    }
    if let Some(problem) = pinned(
        "rekorPublicKeyPath",
        get("rekorPublicKeyPath"),
        INSTALLED_REKOR,
    ) {
        return Ok(Some(problem));
    }
    let identity = match get("signedIdentity") {
        Some(Json::Object(identity))
            if identity.len() == 1 && identity.iter().all(|(k, _)| k == "type") =>
        {
            identity
        }
        _ => {
            return Ok(Some(
                "signedIdentity must be an object naming only its type".to_owned(),
            ));
        }
    };
    let kind = identity.first().map(|(_, v)| v);
    if matches!(kind, Some(Json::Array(_) | Json::Object(_))) {
        return Err(format!(
            "unhashable signedIdentity type {}",
            json::repr(kind)
        ));
    }
    if !kind
        .and_then(Json::as_str)
        .is_some_and(|k| ACCEPTED_IDENTITIES.contains(&k))
    {
        return Ok(Some(format!(
            "signedIdentity type {} does not bind the image's own repository",
            json::repr(kind)
        )));
    }
    Ok(None)
}

fn is_reject(item: &Json) -> bool {
    matches!(item, Json::Object(fields)
        if fields.len() == 1 && fields.iter().all(|(k, v)| k == "type" && v.as_str() == Some("reject")))
}

fn scope_problem(
    requirements: Option<&Json>,
    allow_reject: bool,
    trust: &Trust,
) -> Result<Option<String>, String> {
    let items = match requirements {
        Some(Json::Array(items)) if !items.is_empty() => items,
        _ => return Ok(Some("requirement list is missing or empty".to_owned())),
    };
    if allow_reject && items.iter().all(is_reject) {
        return Ok(None);
    }
    for requirement in items {
        if let Some(problem) = signed_requirement_problem(requirement, trust)? {
            return Ok(Some(problem));
        }
    }
    Ok(None)
}

/// Whether `scope` of `transport` may accept anything.
fn may_accept(transport: &str, scope: &str) -> bool {
    if transport == "docker" {
        USER_CHOSEN_REGISTRIES.iter().any(|registry| {
            scope == *registry
                || scope
                    .strip_prefix(registry)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    } else {
        scope.is_empty() && LOCAL_TRANSPORTS.contains(&transport)
    }
}

/// The scope name and requirements containers/image applies to
/// `repository`: its own scope, else the nearest enclosing namespace scope,
/// else the host's, else the policy default.
fn effective_rule<'a>(
    policy: &'a Json,
    docker: &'a [(String, Json)],
    repository: &str,
) -> (String, Option<&'a Json>) {
    let mut candidate = repository;
    loop {
        if let Some((_, requirements)) = docker.iter().find(|(scope, _)| scope == candidate) {
            return (candidate.to_owned(), Some(requirements));
        }
        match candidate.rsplit_once('/') {
            Some((parent, _)) => candidate = parent,
            None => break,
        }
    }
    ("the default rule".to_owned(), policy.get("default"))
}

/// What a policy failed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The policy does not prove the requirement (`PolicyError`).
    Policy(String),
    /// A value the original could not compare (it raised `TypeError`).
    Crash(String),
}

impl Failure {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Policy(message) | Self::Crash(message) => message,
        }
    }
}

/// Checks the policy text against the installed release trust.
///
/// # Errors
/// The first thing that keeps the policy from proving the requirement.
pub fn check(text: &str, trust: &Trust) -> Result<(), Failure> {
    match problems(text, trust)?.into_iter().next() {
        Some(problem) => Err(Failure::Policy(problem)),
        None => Ok(()),
    }
}

/// Everything that keeps the policy from proving the requirement, in the
/// order [`check`] meets them: what the live policy enforces, rather than
/// what it was meant to. Empty when it proves it.
///
/// # Errors
/// A policy this check cannot read at all (malformed, no transports), as
/// [`Failure::Policy`]; a value it cannot compare as [`Failure::Crash`].
pub fn problems(text: &str, trust: &Trust) -> Result<Vec<String>, Failure> {
    let policy = parse(text).map_err(Failure::Policy)?;
    let Json::Object(_) = policy else {
        return Err(Failure::Policy("policy is not a JSON object".to_owned()));
    };
    let Some(Json::Object(transports)) = policy.get("transports") else {
        return Err(Failure::Policy(
            "policy has no transports object".to_owned(),
        ));
    };
    let Some(Json::Object(docker)) = policy.get("transports").and_then(|t| t.get("docker")) else {
        return Err(Failure::Policy(
            "policy has no transports.docker object".to_owned(),
        ));
    };
    let mut problems = Vec::new();
    let lookup = |scope: &str| {
        docker
            .iter()
            .find(|(name, _)| name == scope)
            .map(|(_, v)| v)
    };
    for scope in REQUIRED_SCOPES {
        let Some(requirements) = lookup(scope) else {
            problems.push(format!(
                "{scope}: no docker scope, so the default rule applies"
            ));
            continue;
        };
        if let Some(problem) =
            scope_problem(Some(requirements), false, trust).map_err(Failure::Crash)?
        {
            problems.push(format!("{scope}: {problem}"));
        }
    }
    for (scope, requirements) in docker {
        if REQUIRED_SCOPES.contains(&scope.as_str()) {
            continue;
        }
        let tilo = TILO_NAMESPACES
            .iter()
            .any(|namespace| scope.starts_with(namespace))
            || TILO_NAMESPACE_ROOTS.contains(&scope.as_str());
        if !tilo {
            continue;
        }
        if let Some(problem) =
            scope_problem(Some(requirements), true, trust).map_err(Failure::Crash)?
        {
            problems.push(format!("{scope}: {problem}"));
        }
    }
    for repository in GUARDED_REPOSITORIES {
        let (scope, requirements) = effective_rule(&policy, docker, repository);
        if let Some(problem) = scope_problem(requirements, true, trust).map_err(Failure::Crash)? {
            let problem = format!("{repository}: governed by {scope}: {problem}");
            if !problems
                .iter()
                .any(|known| problem.ends_with(known.as_str()))
            {
                problems.push(problem);
            }
        }
    }
    let default_rejects = matches!(policy.get("default"),
        Some(Json::Array(items)) if items.len() == 1 && items.iter().all(is_reject));
    if !default_rejects {
        problems.push(format!(
            "the default rule is {}, not a reject, so an image no scope names is not refused",
            json::repr(policy.get("default"))
        ));
    }
    for (transport, scopes) in transports {
        let Json::Object(scopes) = scopes else {
            problems.push(format!("transports.{transport} is not an object"));
            continue;
        };
        for (scope, requirements) in scopes {
            let accepts = matches!(requirements, Json::Array(items) if items.iter().any(is_accept));
            if accepts && !may_accept(transport, scope) {
                problems.push(format!(
                    "{transport} scope {}: insecureAcceptAnything, which only user-chosen public registries and local transports may use",
                    json::repr_str(scope)
                ));
            }
        }
    }
    Ok(problems)
}

/// The repository of an image reference: without its tag or digest.
fn repository_of(image: &str) -> &str {
    let image = image.split('@').next().unwrap_or(image);
    match image.rsplit_once(':') {
        Some((head, tag)) if !tag.contains('/') => head,
        _ => image,
    }
}

/// Every way the policy lets an image in unsigned that a production device
/// does not: a default that is not a reject, and each scope that accepts
/// anything where only user-chosen public registries and local transports
/// may. With `image` (the booted image's reference), also what the policy
/// asks of the repository it updates from, when that is not a signature.
/// One line per fact, for the boot audit's development posture.
///
/// # Errors
/// A policy that cannot be parsed.
pub fn unsigned_admissions(text: &str, image: Option<&str>) -> Result<Vec<String>, String> {
    let policy = parse(text)?;
    let mut facts = Vec::new();
    let accepts = |requirements: Option<&Json>| matches!(requirements, Some(Json::Array(items)) if items.iter().any(is_accept));
    let default_rejects = matches!(policy.get("default"),
        Some(Json::Array(items)) if items.len() == 1 && items.iter().all(is_reject));
    if !default_rejects {
        facts.push(format!(
            "the default rule is {}, so an image no scope names is not refused",
            json::repr(policy.get("default"))
        ));
    }
    if let Some(Json::Object(transports)) = policy.get("transports") {
        for (transport, scopes) in transports {
            let Json::Object(scopes) = scopes else {
                continue;
            };
            for (scope, requirements) in scopes {
                if accepts(Some(requirements)) && !may_accept(transport, scope) {
                    facts.push(format!(
                        "{transport} scope {} accepts unsigned images",
                        json::repr_str(scope)
                    ));
                }
            }
        }
    }
    if let Some(image) = image.filter(|image| !image.is_empty()) {
        let repository = repository_of(image);
        let docker: &[(String, Json)] = match policy.get("transports").and_then(|t| t.get("docker"))
        {
            Some(Json::Object(docker)) => docker,
            _ => &[],
        };
        let (scope, requirements) = effective_rule(&policy, docker, repository);
        if accepts(requirements) {
            facts.push(format!(
                "the booted image's repository {repository} accepts unsigned updates ({scope})"
            ));
        }
    }
    Ok(facts)
}

/// `str(OSError)` as Python wrote it: `[Errno N] reason: 'path'`.
fn os_message(error: &std::io::Error, path: &str) -> String {
    let text = error.to_string();
    let reason = text.split(" (os error").next().unwrap_or(&text);
    error.raw_os_error().map_or_else(
        || text.clone(),
        |code| format!("[Errno {code}] {reason}: {}", json::repr_str(path)),
    )
}

/// `str(UnicodeDecodeError)` for a UTF-8 decode.
fn decode_message(bytes: &[u8], error: &std::str::Utf8Error) -> String {
    let position = error.valid_up_to();
    let byte = bytes.get(position).copied().unwrap_or(0);
    let reason = match error.error_len() {
        None => "unexpected end of data",
        Some(_) if !(0xC2..=0xF4).contains(&byte) => "invalid start byte",
        Some(_) => "invalid continuation byte",
    };
    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: {reason}")
}

/// The command line, without the program name; writes the report to `out`
/// and the usage to `err`, and returns the exit status.
pub fn main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.first().map(String::as_str) == Some("--unsigned") {
        return unsigned_main(args.get(1..).unwrap_or_default(), out, err);
    }
    if args.len() > 2 {
        let _ = writeln!(err, "usage: tilo-image-policy-check [POLICY [TRUST_DIR]]");
        return 2;
    }
    let path = args.first().map_or(DEFAULT_POLICY, String::as_str);
    let trust_dir = args.get(1).map_or(DEFAULT_TRUST_DIR, String::as_str);
    let mut report = String::new();
    let code = match std::fs::read(path) {
        Err(error) => {
            let _ = write!(
                report,
                "{path}: cannot read policy: {}",
                os_message(&error, path)
            );
            1
        }
        Ok(bytes) => match std::str::from_utf8(&bytes) {
            Err(error) => {
                let _ = write!(
                    report,
                    "{path}: cannot read policy: {}",
                    decode_message(&bytes, &error)
                );
                1
            }
            Ok(text) => match Trust::load(Path::new(trust_dir)) {
                Err(message) => {
                    let _ = write!(report, "{path}: {message}");
                    1
                }
                Ok(trust) => match check(&text.replace("\r\n", "\n").replace('\r', "\n"), &trust) {
                    Ok(()) => {
                        let _ = write!(
                            report,
                            "{path}: Tilo image scopes require {}",
                            trust.describe()
                        );
                        0
                    }
                    Err(Failure::Policy(message)) => {
                        let _ = write!(report, "{path}: {message}");
                        1
                    }
                    Err(Failure::Crash(message)) => {
                        let _ = writeln!(err, "TypeError: {message}");
                        return 1;
                    }
                },
            },
        },
    };
    let _ = writeln!(out, "{report}");
    code
}

/// The booted image's reference in `bootc status --json` output at `path`
/// (`status.booted.image.image.image`), when it has one.
fn booted_image(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let status = parse(&text).ok()?;
    status
        .get("status")?
        .get("booted")?
        .get("image")?
        .get("image")?
        .get("image")?
        .as_str()
        .map(str::to_owned)
}

/// `--unsigned POLICY [BOOTC_STATUS]`: [`unsigned_admissions`] for the image
/// `bootc status --json` (saved at `BOOTC_STATUS`) says is booted, one per line. Exit 0
/// whatever it found (these are facts the audit weighs), 1 when the policy
/// cannot be read, 2 for a usage error.
fn unsigned_main(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if args.is_empty() || args.len() > 2 {
        let _ = writeln!(
            err,
            "usage: tilo-image-policy-check --unsigned POLICY [IMAGE]"
        );
        return 2;
    }
    let path = args.first().map_or(DEFAULT_POLICY, String::as_str);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(
                out,
                "{path}: cannot read policy: {}",
                os_message(&error, path)
            );
            return 1;
        }
    };
    let image = args.get(1).and_then(|status| booted_image(status));
    match unsigned_admissions(&text, image.as_deref()) {
        Ok(facts) => {
            for fact in facts {
                let _ = writeln!(out, "{fact}");
            }
            0
        }
        Err(message) => {
            let _ = writeln!(out, "{path}: {message}");
            1
        }
    }
}
