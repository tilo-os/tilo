//! Which release succeeds the running one: the board tag is only a hint;
//! the pinned Cosign key must sign the exact digest, its provenance must bind
//! the board, architecture, source and generation, and a current freshness
//! statement must name the same release.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use crate::profile::{get, index};
use crate::py;
use crate::{Error, Result, require};

pub const REPOSITORY: &str = "code.tilo.so/tilo/tilo";
pub const PROFILE: &str = "lenovo-83ed";
pub const FRESHNESS: &str = "https://tilo.sh/security/release-freshness/v1";
pub const SLSA: &str = "https://slsa.dev/provenance/v1";
pub const MAX_OUTPUT: usize = 8 * 1024 * 1024;
pub const COSIGN: &str = "/usr/libexec/tilo-cosign";
pub const KEY: &str = "/usr/share/tilo/cosign.pub";
pub const SKOPEO: &str = "/usr/bin/skopeo";

fn full(pattern: &str) -> Option<Regex> {
    Regex::new(&format!("^(?:{pattern})$")).ok()
}

static HEX: LazyLock<Option<Regex>> = LazyLock::new(|| full("[0-9a-f]{64}"));
static SOURCE: LazyLock<Option<Regex>> =
    LazyLock::new(|| full(r"git\+ssh://code\.tilo\.so/tilo/tilo@([0-9a-f]{40})"));
static FIRMWARE: LazyLock<Option<Regex>> =
    LazyLock::new(|| full(r"code\.tilo\.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}"));

fn is_hex(text: &str) -> bool {
    HEX.as_ref().is_some_and(|re| re.is_match(text))
}

/// Runs one verification command and returns its standard output.
pub trait Runner {
    /// # Errors
    /// The command failed, timed out or wrote too much.
    fn run(&self, arguments: &[&str]) -> Result<String>;
}

impl<F: Fn(&[&str]) -> Result<String>> Runner for F {
    fn run(&self, arguments: &[&str]) -> Result<String> {
        self(arguments)
    }
}

fn parse(text: &str) -> Option<Value> {
    serde_json::from_str(text).ok()
}

/// The verification rows in cosign's output: one JSON document, or one per
/// line, arrays flattened; between 1 and 128 objects.
pub fn wrappers(output: &str) -> Result<Vec<serde_json::Map<String, Value>>> {
    require(
        !output.is_empty() && output.len() <= MAX_OUTPUT,
        "invalid verification output size",
    )?;
    let values = if let Some(value) = parse(output) {
        vec![value]
    } else {
        let mut values = Vec::new();
        for line in py::splitlines(output) {
            if py::strip(line).is_empty() {
                continue;
            }
            values.push(serde_json::from_str(line).map_err(|e| Error::Value(e.to_string()))?);
        }
        values
    };
    let mut rows = Vec::new();
    for value in values {
        match value {
            Value::Array(items) => rows.extend(items),
            other => rows.push(other),
        }
    }
    let count = rows.len();
    let objects: Vec<_> = rows
        .into_iter()
        .filter_map(|row| match row {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect();
    require(
        count > 0 && count <= 128 && objects.len() == count,
        "malformed or excessive verification statements",
    )?;
    Ok(objects)
}

/// The one predicate of type `predicate_type` whose statement names exactly
/// `digest` as its only subject.
pub fn statements(output: &str, predicate_type: &str, digest: &str) -> Result<Value> {
    let subject_digest = json!({"sha256": digest.get(7..).unwrap_or_default()});
    let mut result = Vec::new();
    for row in wrappers(output)? {
        let Some(Value::String(payload)) = row.get("payload") else {
            continue;
        };
        let Ok(decoded) = py::b64decode(payload) else {
            continue;
        };
        let Ok(Value::Object(statement)) = serde_json::from_slice::<Value>(&decoded) else {
            continue;
        };
        if statement.get("predicateType").and_then(Value::as_str) != Some(predicate_type) {
            continue;
        }
        if let Some(Value::Array(subjects)) = statement.get("subject")
            && let [Value::Object(subject)] = subjects.as_slice()
            && subject.get("digest") == Some(&subject_digest)
        {
            result.push(statement.get("predicate").cloned().unwrap_or(Value::Null));
        }
    }
    match result.as_slice() {
        [predicate @ Value::Object(_)] => Ok(predicate.clone()),
        _ => Err(Error::value(
            "release has no unique exact-digest attestation",
        )),
    }
}

/// Every signature row names `digest` and the release repository.
///
/// # Errors
/// One that does not.
pub fn signature(output: &str, digest: &str) -> Result<()> {
    let empty = json!({});
    for row in wrappers(output)? {
        let row = Value::Object(row);
        let critical = get(&row, "critical")?.unwrap_or(&empty);
        let image = get(critical, "image")?.unwrap_or(&empty);
        let identity = get(critical, "identity")?.unwrap_or(&empty);
        require(
            get(image, "docker-manifest-digest")?.and_then(Value::as_str) == Some(digest)
                && get(identity, "docker-reference")?.and_then(Value::as_str) == Some(REPOSITORY),
            "release signature identity or digest differs",
        )?;
    }
    Ok(())
}

/// The source revision and generation the SLSA provenance binds to this
/// board's release of `digest`.
///
/// # Errors
/// Provenance that does not bind the board, architecture, digest,
/// generation, one source and one immutable firmware, or lacks the
/// independent-rebuild witness.
pub fn provenance(output: &str, digest: &str) -> Result<(String, u64)> {
    let predicate = statements(output, SLSA, digest)?;
    let empty = json!({});
    let definition = get(&predicate, "buildDefinition")?.unwrap_or(&empty);
    let details = get(&predicate, "runDetails")?.unwrap_or(&empty);
    require(
        definition.is_object() && details.is_object(),
        "invalid release provenance",
    )?;
    let external = get(definition, "externalParameters")?.unwrap_or(&empty);
    let binds = || -> Result<bool> {
        if !external.is_object()
            || get(definition, "buildType")?
                != Some(&json!("https://tilo.sh/buildtypes/bootc-os/v1"))
            || get(details, "builder")?
                != Some(&json!({"id": "https://code.tilo.so/tilo/t/os-release-v1"}))
        {
            return Ok(false);
        }
        let internal = get(definition, "internalParameters")?.unwrap_or(&empty);
        let field = |key: &str| get(external, key).ok().flatten();
        let hex_field = |key: &str| field(key).and_then(Value::as_str).is_some_and(is_hex);
        Ok(
            get(internal, "expectedReleaseDigest")?.and_then(Value::as_str) == Some(digest)
                && field("hardwareProfile").and_then(Value::as_str) == Some(PROFILE)
                && field("architecture").and_then(Value::as_str) == Some("aarch64")
                && field("generation")
                    .and_then(py::as_int)
                    .is_some_and(|g| 0 < g && g < (1i128 << 64))
                && field("children").is_none()
                && hex_field("hardwareDefinitionSha256")
                && hex_field("firmwareManifestSha256"),
        )
    };
    require(
        binds()?,
        "release provenance does not bind the board, architecture, digest and generation",
    )?;
    let generation = get(external, "generation")?
        .and_then(py::as_int)
        .and_then(|g| u64::try_from(g).ok())
        .ok_or_else(|| Error::value("invalid release provenance"))?;
    let dependencies = match get(definition, "resolvedDependencies")? {
        Some(Value::Array(items)) if items.len() <= 64 => items,
        _ => return Err(Error::value("invalid release dependencies")),
    };
    let uris: Vec<&str> = dependencies
        .iter()
        .filter_map(|item| item.as_object()?.get("uri")?.as_str())
        .collect();
    let sources: Vec<String> = uris
        .iter()
        .filter_map(|uri| {
            let captures = SOURCE.as_ref()?.captures(uri)?;
            Some(captures.get(1)?.as_str().to_owned())
        })
        .collect();
    let firmware = uris
        .iter()
        .filter(|uri| FIRMWARE.as_ref().is_some_and(|re| re.is_match(uri)))
        .count();
    let [source] = sources.as_slice() else {
        return Err(Error::value(
            "release has no unique source and immutable board firmware",
        ));
    };
    require(
        firmware == 1,
        "release has no unique source and immutable board firmware",
    )?;
    let witness = json!({"name": "independent-rebuild", "uri": "urn:tilo:verification:manifest-digest-equal"});
    require(
        matches!(get(details, "byproducts")?, Some(Value::Array(items)) if items.contains(&witness)),
        "release has no independent rebuild witness",
    )?;
    Ok((source.clone(), generation))
}

/// When the freshness statement for this exact release expires.
pub fn freshness(
    output: &str,
    digest: &str,
    source: &str,
    generation: u64,
    now: i128,
) -> Result<i128> {
    let predicate = statements(output, FRESHNESS, digest)?;
    let message = "release freshness is expired, ambiguous, or belongs to another release";
    let fields = [
        "schema",
        "repository",
        "digest",
        "sourceRevision",
        "generation",
        "issuedAt",
        "expiresAt",
    ];
    let Value::Object(map) = &predicate else {
        return Err(Error::value(message));
    };
    let mut present: Vec<&str> = map.keys().map(String::as_str).collect();
    present.sort_unstable();
    let mut expected = fields.to_vec();
    expected.sort_unstable();
    require(present == expected, message)?;
    let text = |key: &str| index(&predicate, key).ok().and_then(Value::as_str);
    let int = |key: &str| index(&predicate, key).ok().and_then(py::as_int);
    let (Some(issued), Some(expires)) = (int("issuedAt"), int("expiresAt")) else {
        return Err(Error::value(message));
    };
    require(
        text("schema") == Some(FRESHNESS)
            && text("repository") == Some(REPOSITORY)
            && text("digest") == Some(digest)
            && text("sourceRevision") == Some(source)
            && int("generation") == Some(i128::from(generation))
            && 0 < issued
            && issued <= now
            && now < expires
            && (300..=604_800).contains(&expires.saturating_sub(issued)),
        message,
    )?;
    Ok(expires)
}

/// What [`select`] chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Successor {
    /// `code.tilo.so/tilo/tilo@sha256:…`
    pub exact: String,
    pub generation: u64,
    pub source: String,
    pub expires: i128,
}

/// Follows the board's `-latest` hint to a digest and accepts it only when
/// the pinned key signed it, its provenance and freshness bind it, its
/// generation is at least the running one and at most 64 ahead, and the
/// generation and source tags point at the same digest.
///
/// # Errors
/// Any of those checks, or a failed command.
pub fn select(current_generation: u64, now: i128, runner: &dyn Runner) -> Result<Successor> {
    let hint = format!("docker://{REPOSITORY}:{PROFILE}-latest");
    let output = runner.run(&[SKOPEO, "inspect", "--format", "{{.Digest}}", &hint])?;
    let digest = py::strip(&output).to_owned();
    require(
        digest.strip_prefix("sha256:").is_some_and(is_hex),
        "invalid successor hint digest",
    )?;
    let exact = format!("{REPOSITORY}@{digest}");
    signature(
        &runner.run(&[COSIGN, "verify", "--key", KEY, &exact])?,
        &digest,
    )?;
    let attestation = runner.run(&[
        COSIGN,
        "verify-attestation",
        "--key",
        KEY,
        "--type",
        "slsaprovenance1",
        &exact,
    ])?;
    let (source, generation) = provenance(&attestation, &digest)?;
    require(
        generation >= current_generation,
        "release hint regressed below the running generation",
    )?;
    require(
        generation.saturating_sub(current_generation) <= 64,
        "release advance exceeds rollback counter bound; install intermediate generations",
    )?;
    let fresh = runner.run(&[
        COSIGN,
        "verify-attestation",
        "--key",
        KEY,
        "--type",
        FRESHNESS,
        &exact,
    ])?;
    let expires = freshness(&fresh, &digest, &source, generation, now)?;
    for tag in [
        format!("{PROFILE}-gen{generation}"),
        format!("{PROFILE}-{source}"),
    ] {
        let tagged = format!("{REPOSITORY}:{tag}");
        signature(
            &runner.run(&[COSIGN, "verify", "--key", KEY, &tagged])?,
            &digest,
        )?;
    }
    Ok(Successor {
        exact,
        generation,
        source,
        expires,
    })
}
