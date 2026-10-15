//! `tilo-image-policy-check`: proves that `containers-policy.json` requires
//! Tilo's pinned Cosign signature.
//!
//! ```text
//! tilo-image-policy-check [POLICY]   (default /etc/containers/policy.json)
//! ```
//!
//! Exits 0 only when the parsed policy makes every Tilo-published image scope
//! require a sigstore signature from `/usr/share/tilo/cosign.pub`. Anything
//! this check cannot prove (an unreadable file, malformed JSON, a duplicate
//! key, an unknown requirement field) is a failure, never a pass: exit 1 with
//! the reason on stdout, exit 2 for a usage error. The boot security audit
//! (`tilo-security-audit`) runs it; it only reads, never repairs.
//!
//! The rules mirror containers-policy.json(5): requirements in a scope's list
//! are `AND`ed, and the most specific docker scope wins. So every required scope
//! must exist and hold only pinned-key `sigstoreSigned` requirements, and no
//! scope at or beneath a Tilo namespace (a tag, a digest, a sibling
//! repository, the namespace itself) may accept anything less than that or an
//! outright reject.
//!
//! The repositories Tilo publishes but a machine must never run unsigned (the
//! legacy guest and the development and CI images) are checked by their
//! effective rule, not by their own scope's presence: removing a repository's
//! scope hands it to the nearest namespace scope or the default, and a default
//! of `insecureAcceptAnything` would then accept it unsigned.

mod json;

use std::fmt::Write as _;
use std::io::Write;

pub use json::{Json, parse};

pub const DEFAULT_POLICY: &str = "/etc/containers/policy.json";
pub const PINNED_KEY: &str = "/usr/share/tilo/cosign.pub";
/// The scopes Tilo OS images are pulled and updated from.
pub const REQUIRED_SCOPES: [&str; 2] = ["registry.tilo.so/tilo/tilo", "code.tilo.so/tilo/tilo"];
/// Every scope under these namespaces is Tilo-published: it may be refused
/// outright, or it must require the pinned signature.
pub const TILO_NAMESPACES: [&str; 2] = ["registry.tilo.so/tilo/", "code.tilo.so/tilo/"];
/// Broader scopes that would also govern a Tilo repository with no scope of
/// its own. Unrelated registries keep the policy's default.
pub const TILO_NAMESPACE_ROOTS: [&str; 4] = [
    "registry.tilo.so",
    "registry.tilo.so/tilo",
    "code.tilo.so",
    "code.tilo.so/tilo",
];
/// Tilo repositories whose effective rule must be the pinned signature or a
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
/// Identities that bind a signature to the image's own repository.
/// `remapIdentity`, `exactReference` and `exactRepository` can accept a
/// signature made for some other image, so they do not prove the requirement.
const ACCEPTED_IDENTITIES: [&str; 3] = ["matchRepository", "matchExact", "matchRepoDigestOrExact"];
const SIGNED_FIELDS: [&str; 3] = ["type", "keyPath", "signedIdentity"];

/// Why a requirement is not the pinned sigstore signature, or `None`.
///
/// # Errors
/// A `signedIdentity` type that is a list or an object, which the original
/// could not even compare (an unhashable value).
fn signed_requirement_problem(requirement: &Json) -> Result<Option<String>, String> {
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
    let mut extra: Vec<&str> = fields
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !SIGNED_FIELDS.contains(k))
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
    let key = get("keyPath");
    if key.and_then(Json::as_str) != Some(PINNED_KEY) {
        return Ok(Some(format!(
            "keyPath is {}, not {}",
            json::repr(key),
            json::repr_str(PINNED_KEY)
        )));
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

fn scope_problem(
    requirements: Option<&Json>,
    allow_reject: bool,
) -> Result<Option<String>, String> {
    let items = match requirements {
        Some(Json::Array(items)) if !items.is_empty() => items,
        _ => return Ok(Some("requirement list is missing or empty".to_owned())),
    };
    let is_reject = |item: &Json| {
        matches!(item, Json::Object(fields)
            if fields.len() == 1 && fields.iter().all(|(k, v)| k == "type" && v.as_str() == Some("reject")))
    };
    if allow_reject && items.iter().all(is_reject) {
        return Ok(None);
    }
    for requirement in items {
        if let Some(problem) = signed_requirement_problem(requirement)? {
            return Ok(Some(problem));
        }
    }
    Ok(None)
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

/// Checks the policy text.
///
/// # Errors
/// The first thing that keeps the policy from proving the requirement.
pub fn check(text: &str) -> Result<(), Failure> {
    let policy = parse(text).map_err(Failure::Policy)?;
    let Json::Object(_) = policy else {
        return Err(Failure::Policy("policy is not a JSON object".to_owned()));
    };
    let Some(Json::Object(_)) = policy.get("transports") else {
        return Err(Failure::Policy(
            "policy has no transports object".to_owned(),
        ));
    };
    let Some(Json::Object(docker)) = policy.get("transports").and_then(|t| t.get("docker")) else {
        return Err(Failure::Policy(
            "policy has no transports.docker object".to_owned(),
        ));
    };
    let lookup = |scope: &str| {
        docker
            .iter()
            .find(|(name, _)| name == scope)
            .map(|(_, v)| v)
    };
    for scope in REQUIRED_SCOPES {
        let Some(requirements) = lookup(scope) else {
            return Err(Failure::Policy(format!(
                "{scope}: no docker scope, so the default rule applies"
            )));
        };
        if let Some(problem) = scope_problem(Some(requirements), false).map_err(Failure::Crash)? {
            return Err(Failure::Policy(format!("{scope}: {problem}")));
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
        if let Some(problem) = scope_problem(Some(requirements), true).map_err(Failure::Crash)? {
            return Err(Failure::Policy(format!("{scope}: {problem}")));
        }
    }
    for repository in GUARDED_REPOSITORIES {
        let (scope, requirements) = effective_rule(&policy, docker, repository);
        if let Some(problem) = scope_problem(requirements, true).map_err(Failure::Crash)? {
            return Err(Failure::Policy(format!(
                "{repository}: governed by {scope}: {problem}"
            )));
        }
    }
    Ok(())
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
    if args.len() > 1 {
        let _ = writeln!(err, "usage: tilo-image-policy-check [POLICY]");
        return 2;
    }
    let path = args.first().map_or(DEFAULT_POLICY, String::as_str);
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
            Ok(text) => match check(&text.replace("\r\n", "\n").replace('\r', "\n")) {
                Ok(()) => {
                    let _ = write!(
                        report,
                        "{path}: Tilo image scopes require the {PINNED_KEY} sigstore signature"
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
    };
    let _ = writeln!(out, "{report}");
    code
}
