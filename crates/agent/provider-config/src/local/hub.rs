//! Models a person picks from Hugging Face, or adds from a file, themselves.
//!
//! The signed catalog ([`super::LocalModelCatalog`]) is a curated starting
//! point, not the only source: a person can search the Hub, open any model
//! by id or URL (their own fine-tunes included), see each runnable variant
//! with its size, licence and how it fits this machine, and download it.
//! This module is the pure half of that: reading what a person typed,
//! reading the Hub API's answers, grouping a repository's files into
//! variants, rating a variant's fit and turning the pick into the
//! [`LocalModelRelease`] the existing runtime path launches. It does no I/O.
//!
//! # Why weights may come from anywhere while runtimes may not
//!
//! A runtime is code: the container image that parses weights, allocates
//! accelerator memory and opens a port. It stays a digest-pinned image from
//! the signed catalog, started with an argv built only from typed fields.
//! Weights are data that this signed code reads, so they can come from
//! wherever the person chooses without widening what runs on the machine:
//!
//! - only data formats are offered: GGUF for llama.cpp, safetensors plus
//!   JSON config and tokenizer files for vLLM. Pickled checkpoints
//!   (`*.bin`, `*.pt`), Python files and anything else in the repository
//!   are never downloaded, and vLLM is never told to trust remote code, so a
//!   repository cannot ship code that runs;
//! - every file is verified against the hash the Hub publishes for the
//!   pinned commit (the LFS SHA-256 for weights, the git blob id for small
//!   files) before it enters the content-addressed store, so what runs is
//!   exactly what the person picked, even if the repository moves on;
//! - the runtime sees the model read-only through one bind mount, rootless,
//!   and answers only on a loopback port the manager allocates.
//!
//! What the person takes on is the model's behaviour and licence, which is
//! why both are shown before anything downloads.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::snapshot::HardwareFit;
use super::{
    Accelerator, HardwareSnapshot, LocalModelRelease, ModelArtifactFormat, PinnedArtifact,
    RuntimeKind,
};

/// The Hub's origin. Every API call and download starts here.
pub const HUB_ORIGIN: &str = "https://huggingface.co";

/// Vault binding of the person's Hugging Face access token, which gated
/// models need. Settings stores it and the model manager reads it; neither
/// passes it on a command line or writes it to state or logs.
pub const TOKEN_SERVICE: &str = "tilo-models";
pub const TOKEN_PURPOSE: &str = "huggingface-access-token";
pub const TOKEN_ID: &str = "huggingface";

/// What the model manager reports of the Hugging Face credential stored
/// here, without the credential itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HubTokenState {
    /// Nothing is stored.
    #[default]
    None,
    /// An access token the person pasted.
    Pasted,
    /// Signed in with Hugging Face as `account`.
    SignedIn { account: HubAccount },
    /// `account`'s sign-in ended and could not be renewed: sign in again.
    Ended { account: HubAccount },
}

impl HubTokenState {
    /// Whether anything is stored, an ended sign-in included.
    pub const fn stored(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Whether requests carry a credential.
    pub const fn usable(&self) -> bool {
        matches!(self, Self::Pasted | Self::SignedIn { .. })
    }
}

/// A Hugging Face account, as its sign-in shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubAccount {
    pub username: String,
    /// The display name, when it differs from the username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Results one search asks for.
pub const SEARCH_LIMIT: u16 = 30;
/// The most files one variant may have; more is not a model.
const MAX_VARIANT_FILES: usize = 512;
const MAX_PATH_BYTES: usize = 1024;
const MIB: u64 = 1024 * 1024;
/// Memory left to the desktop and its apps while a model runs.
const DESKTOP_HEADROOM_MIB: u64 = 4096;
/// Context assumed when the Hub reports none; the runtime caps it again.
pub const DEFAULT_CONTEXT_WINDOW: u32 = 4096;

/// Why something a person typed or the Hub answered is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HubError {
    /// Not a Hugging Face model id or URL.
    NotAModel(&'static str),
    /// The Hub answered something that cannot be a model of this shape.
    Invalid(&'static str),
}

impl fmt::Display for HubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAModel(reason) => write!(formatter, "not a Hugging Face model: {reason}"),
            Self::Invalid(reason) => write!(formatter, "unusable Hugging Face answer: {reason}"),
        }
    }
}

impl std::error::Error for HubError {}

/// A model on the Hub as a person named it: `owner/name`, or a model, tree,
/// blob or resolve URL on `huggingface.co` or `hf.co`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubRef {
    pub repo: String,
    /// A branch, tag or commit; the default branch when absent.
    pub revision: Option<String>,
    /// The one file a blob or resolve URL pointed at.
    pub file: Option<String>,
}

impl HubRef {
    /// Read `input` as a Hub model.
    ///
    /// # Errors
    /// [`HubError::NotAModel`] for anything else: another site, a dataset
    /// or Space, or a name that is not `owner/name`.
    pub fn parse(input: &str) -> Result<Self, HubError> {
        let input = input.trim();
        let input = input
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        let path = [
            "https://huggingface.co/",
            "http://huggingface.co/",
            "https://www.huggingface.co/",
            "huggingface.co/",
            "https://hf.co/",
            "hf.co/",
        ]
        .iter()
        .find_map(|prefix| input.strip_prefix(prefix))
        .unwrap_or(input);
        if path.contains("://") {
            return Err(HubError::NotAModel("only huggingface.co links are models"));
        }
        let segments: Vec<&str> = path.split('/').collect();
        if matches!(segments.first(), Some(&("datasets" | "spaces" | "api"))) {
            return Err(HubError::NotAModel("datasets and Spaces are not models"));
        }
        let [owner, name, rest @ ..] = segments.as_slice() else {
            return Err(HubError::NotAModel("a model is named owner/name"));
        };
        if !valid_name(owner) || !valid_name(name) {
            return Err(HubError::NotAModel("a model is named owner/name"));
        }
        let repo = format!("{owner}/{name}");
        let (revision, file) = match rest {
            [] => (None, None),
            [kind, rest @ ..] if matches!(*kind, "tree" | "blob" | "resolve") => {
                let (revision, file) = match rest {
                    ["refs", kind, number, file @ ..] => {
                        (format!("refs/{kind}/{number}"), file.join("/"))
                    }
                    [revision, file @ ..] => ((*revision).to_owned(), file.join("/")),
                    [] => return Err(HubError::NotAModel("the link names no revision")),
                };
                if !valid_revision(&revision) {
                    return Err(HubError::NotAModel("the link's revision is not valid"));
                }
                let file = if file.is_empty() || *kind == "tree" {
                    None
                } else {
                    safe_path(&file)
                        .then_some(file)
                        .map(Some)
                        .ok_or(HubError::NotAModel("the link's file path is not valid"))?
                };
                (Some(revision), file)
            }
            _ => return Err(HubError::NotAModel("the link is not a model page or file")),
        };
        Ok(Self {
            repo,
            revision,
            file,
        })
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_repo(repo: &str) -> bool {
    repo.split_once('/')
        .is_some_and(|(owner, name)| valid_name(owner) && valid_name(name))
}

fn valid_revision(revision: &str) -> bool {
    !revision.is_empty()
        && revision.len() <= 255
        && !revision.contains("..")
        && !revision.starts_with(['/', '-'])
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

/// A commit id: what every download is pinned to.
fn is_commit(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A relative path of ordinary components, as a repository file has.
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// The repository path with each component encoded, separators kept.
fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// The Hub search for `query` among models with `format`'s files, most
/// downloaded first.
#[must_use]
pub fn search_url(query: &str, format: HubFormat) -> String {
    let filter = match format {
        HubFormat::Gguf => "gguf",
        HubFormat::Safetensors => "safetensors",
    };
    let mut url = format!(
        "{HUB_ORIGIN}/api/models?search={}&filter={filter}&sort=downloads&direction=-1&limit={SEARCH_LIMIT}",
        encode(query.trim())
    );
    for field in [
        "downloads",
        "likes",
        "gated",
        "tags",
        "pipeline_tag",
        "lastModified",
    ] {
        url.push_str("&expand%5B%5D=");
        url.push_str(field);
    }
    url
}

/// The Hub's description of `repo` at `revision`, with every file's size
/// and published hash.
#[must_use]
pub fn model_info_url(repo: &str, revision: Option<&str>) -> String {
    format!(
        "{HUB_ORIGIN}/api/models/{repo}/revision/{}?blobs=true",
        encode(revision.unwrap_or("main"))
    )
}

/// Where `path` of `repo` at the pinned commit `revision` downloads from.
#[must_use]
pub fn file_url(repo: &str, revision: &str, path: &str) -> String {
    format!(
        "{HUB_ORIGIN}/{repo}/resolve/{revision}/{}",
        encode_path(path)
    )
}

/// The model's page, where a gated model's terms are accepted.
#[must_use]
pub fn model_page_url(repo: &str) -> String {
    format!("{HUB_ORIGIN}/{repo}")
}

/// Whether a model's files need the person to accept its terms on the Hub
/// first, and how the owner grants that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gated {
    #[default]
    Open,
    /// Granted as soon as the person accepts.
    Auto,
    /// Granted by the owner after the person asks.
    Manual,
}

impl Gated {
    pub const fn is_gated(self) -> bool {
        !matches!(self, Self::Open)
    }
}

impl<'de> Deserialize<'de> for Gated {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The Hub answers `false`, `"auto"` or `"manual"`; our own JSON the
        // snake-case names.
        Ok(match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::Bool(false) | serde_json::Value::Null => Self::Open,
            serde_json::Value::String(value) if value == "open" => Self::Open,
            serde_json::Value::String(value) if value == "auto" => Self::Auto,
            _ => Self::Manual,
        })
    }
}

/// One hit of the Hub's model search, as the API answers it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ApiSearchHit {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub gated: Gated,
    #[serde(default)]
    pub pipeline_tag: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, rename = "lastModified")]
    pub last_modified: Option<String>,
}

/// A model's description at one revision, as the API answers it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ApiModelInfo {
    pub id: String,
    /// The commit the revision resolved to.
    pub sha: String,
    #[serde(default)]
    pub gated: Gated,
    #[serde(default, rename = "cardData")]
    pub card_data: Option<ApiCardData>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub siblings: Vec<ApiSibling>,
    #[serde(default)]
    pub gguf: Option<ApiGguf>,
}

/// The licence part of a model card's metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ApiCardData {
    #[serde(default)]
    pub license: Option<serde_json::Value>,
    #[serde(default)]
    pub license_name: Option<String>,
    #[serde(default)]
    pub license_link: Option<String>,
}

/// One file of the repository.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ApiSibling {
    pub rfilename: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, rename = "blobId")]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub lfs: Option<ApiLfs>,
}

/// A large file's Git LFS pointer: the SHA-256 and size of its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ApiLfs {
    pub sha256: String,
    pub size: u64,
}

/// What the Hub read from a repository's GGUF header.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ApiGguf {
    #[serde(default)]
    pub context_length: Option<u64>,
}

/// The file family a variant is made of, and a search filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubFormat {
    /// GGUF, run by llama.cpp.
    Gguf,
    /// safetensors with its config and tokenizer, run by vLLM.
    Safetensors,
}

impl HubFormat {
    /// The family of `format`, when it is one a person can pick.
    #[must_use]
    pub const fn of(format: ModelArtifactFormat) -> Option<Self> {
        match format {
            ModelArtifactFormat::LlamaCppGguf | ModelArtifactFormat::LlamaCppGgufShards => {
                Some(Self::Gguf)
            }
            ModelArtifactFormat::HfSnapshotFiles => Some(Self::Safetensors),
            _ => None,
        }
    }
}

/// One search hit, as Settings lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubSearchResult {
    pub repo: String,
    pub downloads: u64,
    pub likes: u64,
    pub gated: bool,
    /// The Hub's task tag, `text-generation` for chat models.
    #[serde(default)]
    pub pipeline: Option<String>,
    /// The licence the repository is tagged with.
    #[serde(default)]
    pub license: Option<String>,
    /// The runnable file families it is tagged with.
    #[serde(default)]
    pub formats: Vec<HubFormat>,
}

impl HubSearchResult {
    /// The hit, or `None` for an id that cannot be a model.
    #[must_use]
    pub fn of(hit: ApiSearchHit) -> Option<Self> {
        if !valid_repo(&hit.id) {
            return None;
        }
        let license = hit
            .tags
            .iter()
            .find_map(|tag| tag.strip_prefix("license:"))
            .map(str::to_owned);
        let formats = [
            ("gguf", HubFormat::Gguf),
            ("safetensors", HubFormat::Safetensors),
        ]
        .into_iter()
        .filter(|(tag, _)| hit.tags.iter().any(|candidate| candidate == tag))
        .map(|(_, format)| format)
        .collect();
        Some(Self {
            repo: hit.id,
            downloads: hit.downloads,
            likes: hit.likes,
            gated: hit.gated.is_gated(),
            pipeline: hit.pipeline_tag,
            license,
            formats,
        })
    }
}

/// The hash the Hub publishes for one file at a commit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HubDigest {
    /// SHA-256 of the bytes: the Git LFS object id of a large file.
    Sha256 { sha256: String },
    /// The git blob id of a small file: SHA-1 of `blob <len>\0<bytes>`.
    GitBlob { oid: String },
}

/// One file of a variant, verified against `digest` when downloaded.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HubFile {
    pub path: String,
    pub size_bytes: u64,
    pub digest: HubDigest,
}

impl HubFile {
    fn of(sibling: &ApiSibling) -> Option<Self> {
        if !safe_path(&sibling.rfilename) {
            return None;
        }
        let (size_bytes, digest) = match (&sibling.lfs, &sibling.blob_id, sibling.size) {
            (Some(lfs), _, _) if is_sha256(&lfs.sha256) => (
                lfs.size,
                HubDigest::Sha256 {
                    sha256: lfs.sha256.clone(),
                },
            ),
            (None, Some(oid), Some(size)) if is_sha1(oid) => {
                (size, HubDigest::GitBlob { oid: oid.clone() })
            }
            _ => return None,
        };
        Some(Self {
            path: sibling.rfilename.clone(),
            size_bytes,
            digest,
        })
    }

    /// The SHA-256 of the bytes when the Hub publishes it.
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        match &self.digest {
            HubDigest::Sha256 { sha256 } => Some(sha256),
            HubDigest::GitBlob { .. } => None,
        }
    }

    /// The last component of the path.
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_sha1(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A runnable selection of a repository's files: one GGUF quantization
/// (one file, or its numbered shards) or the safetensors snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubVariant {
    /// `Q4_K_M`, `BF16`, `safetensors`: unique within the repository.
    pub name: String,
    pub format: ModelArtifactFormat,
    pub files: Vec<HubFile>,
}

impl HubVariant {
    #[must_use]
    pub fn download_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }
}

/// The quantization a GGUF file name states (`Q4_K_M`, `IQ3_XXS`, `BF16`).
#[must_use]
pub fn quantization(path: &str) -> Option<String> {
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".gguf")
        .trim_end_matches(".GGUF");
    let stem = shard(stem).map_or(stem, |(base, _, _)| base);
    // Directory names count too: `Q4_K_M/model-00001-of-00002.gguf`.
    let mut tokens: Vec<&str> = path
        .split('/')
        .rev()
        .skip(1)
        .flat_map(|part| part.split(['-', '.']))
        .collect();
    tokens.splice(0..0, stem.split(['-', '.']).rev());
    tokens
        .into_iter()
        .map(str::to_ascii_uppercase)
        .find(|token| is_quantization(token))
}

fn is_quantization(token: &str) -> bool {
    let digits_after = |prefix: &str| {
        token
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    (matches!(token, "F16" | "F32" | "BF16" | "FP16" | "FP32")
        || digits_after("Q")
        || digits_after("IQ")
        || digits_after("TQ"))
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// `name-00001-of-00003` → (`name`, 1, 3).
fn shard(stem: &str) -> Option<(&str, u32, u32)> {
    let (rest, count) = stem.rsplit_once("-of-")?;
    let (base, index) = rest.rsplit_once('-')?;
    let number = |text: &str| {
        (!text.is_empty() && text.len() <= 5 && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    let (index, count) = (number(index)?, number(count)?);
    (index >= 1 && index <= count && !base.is_empty()).then_some((base, index, count))
}

/// The files of a vLLM snapshot besides its weights: configuration,
/// tokenizer and chat template. Nothing executable.
fn snapshot_companion(name: &str) -> bool {
    ["json", "tiktoken", "jinja"]
        .iter()
        .any(|extension| has_extension(name, extension))
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "tokenizer.model" | "merges.txt" | "vocab.txt"
        )
}

/// Whether `name` ends in `.extension`, in any case.
fn has_extension(name: &str, extension: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// Group the repository's files into runnable variants, smallest first,
/// with the safetensors snapshot last. Files without a published hash, and
/// incomplete shard sets, never become a variant.
#[must_use]
pub fn variants(info: &ApiModelInfo) -> Vec<HubVariant> {
    let mut found = Vec::new();
    let mut shards: BTreeMap<String, Vec<(u32, u32, HubFile)>> = BTreeMap::new();
    for sibling in &info.siblings {
        let lower = sibling.rfilename.to_ascii_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower);
        // A vision projector is a companion file, not a model on its own.
        if !has_extension(name, "gguf") || name.contains("mmproj") {
            continue;
        }
        let Some(file) = HubFile::of(sibling).filter(|file| file.sha256().is_some()) else {
            continue;
        };
        let stem = without_gguf(&file.path);
        match shard(stem) {
            Some((base, index, count)) => {
                shards
                    .entry(base.to_owned())
                    .or_default()
                    .push((index, count, file));
            }
            None => found.push(HubVariant {
                name: quantization(&file.path).unwrap_or_else(|| stem_name(stem)),
                format: ModelArtifactFormat::LlamaCppGguf,
                files: vec![file],
            }),
        }
    }
    for (base, mut parts) in shards {
        parts.sort_by_key(|(index, _, _)| *index);
        let count = parts.first().map_or(0, |(_, count, _)| *count);
        let complete = usize::try_from(count)
            .is_ok_and(|count| count == parts.len() && count <= MAX_VARIANT_FILES)
            && parts
                .iter()
                .zip(1..)
                .all(|((index, total, _), expected)| *index == expected && *total == count);
        if complete {
            found.push(HubVariant {
                name: quantization(&base).unwrap_or_else(|| stem_name(&base)),
                format: ModelArtifactFormat::LlamaCppGgufShards,
                files: parts.into_iter().map(|(_, _, file)| file).collect(),
            });
        }
    }
    found.sort_by_key(HubVariant::download_bytes);
    disambiguate(&mut found);
    found.extend(snapshot_variant(info));
    found
}

/// `path` without its `.gguf` extension, in either case.
fn without_gguf(path: &str) -> &str {
    path.strip_suffix(".gguf")
        .or_else(|| path.strip_suffix(".GGUF"))
        .unwrap_or(path)
}

fn stem_name(stem: &str) -> String {
    stem.rsplit('/').next().unwrap_or(stem).to_owned()
}

/// Two files stating one quantization are told apart by their file names.
fn disambiguate(variants: &mut [HubVariant]) {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for variant in variants.iter() {
        let count = counts.entry(variant.name.clone()).or_default();
        *count = count.saturating_add(1);
    }
    for variant in variants.iter_mut() {
        if counts.get(&variant.name).is_some_and(|count| *count > 1)
            && let Some(first) = variant.files.first()
        {
            variant.name = first.path.trim_end_matches(".gguf").to_owned();
        }
    }
}

/// The safetensors snapshot at the repository root, when it has the
/// config, tokenizer and weights vLLM needs and every file has a hash.
fn snapshot_variant(info: &ApiModelInfo) -> Option<HubVariant> {
    let root = info
        .siblings
        .iter()
        .filter(|sibling| !sibling.rfilename.contains('/'));
    let mut files = Vec::new();
    for sibling in root {
        let weights = has_extension(&sibling.rfilename, "safetensors");
        if weights || snapshot_companion(&sibling.rfilename) {
            // A snapshot missing one file's hash cannot be verified whole.
            let file = HubFile::of(sibling)?;
            if weights && file.sha256().is_none() {
                return None;
            }
            files.push(file);
        }
    }
    let has = |name: &str| files.iter().any(|file| file.path == name);
    let complete = files
        .iter()
        .any(|file| has_extension(&file.path, "safetensors"))
        && has("config.json")
        && (has("tokenizer.json") || has("tokenizer_config.json"))
        && files.len() <= MAX_VARIANT_FILES;
    complete.then(|| HubVariant {
        name: "safetensors".into(),
        format: ModelArtifactFormat::HfSnapshotFiles,
        files,
    })
}

/// The licence a model states, with the document a person reads before
/// accepting it at the pinned commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubLicense {
    /// The card's licence id (`apache-2.0`, `llama3.1`, `other`), or
    /// `unknown` when it states none.
    pub id: String,
    pub url: String,
}

impl HubLicense {
    /// What `info` states at its commit.
    #[must_use]
    pub fn of(info: &ApiModelInfo) -> Self {
        let card = info.card_data.clone().unwrap_or_default();
        let id = match &card.license {
            Some(serde_json::Value::String(id)) => Some(id.clone()),
            Some(serde_json::Value::Array(ids)) => ids
                .iter()
                .find_map(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        }
        .or_else(|| {
            info.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("license:"))
                .map(str::to_owned)
        })
        .filter(|id| !id.trim().is_empty() && id.len() <= 128)
        .map_or_else(
            || "unknown".to_owned(),
            |id| {
                if id == "other" {
                    card.license_name
                        .clone()
                        .filter(|name| !name.is_empty() && name.len() <= 128)
                        .unwrap_or(id)
                } else {
                    id
                }
            },
        );
        // The licence file at this commit when the repository has one,
        // else the model card at this commit, which states it.
        let document = ["LICENSE", "LICENSE.md", "LICENSE.txt", "LICENCE"]
            .into_iter()
            .find(|name| info.siblings.iter().any(|file| file.rfilename == *name))
            .unwrap_or("README.md");
        Self {
            id,
            url: format!(
                "{HUB_ORIGIN}/{}/blob/{}/{}",
                info.id,
                info.sha,
                encode_path(document)
            ),
        }
    }
}

/// How well a variant fits this machine, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubFit {
    pub fit: HardwareFit,
    pub reason: HubFitReason,
    /// Memory the model is estimated to need while it runs.
    pub needs_memory_mib: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HubFitReason {
    Accelerator,
    /// It is split between accelerator and system memory.
    AcceleratorAndMemory,
    /// It runs from system memory on the processor.
    Memory,
    /// It needs more memory than this machine has.
    TooLarge,
    /// The processor-only path needs 8 GiB of memory and four threads.
    ProcessorTooSmall,
    /// The model store's drive has too little free space for it.
    NoStorage,
}

/// Rate how `format`'s files of `download_bytes` fit `hardware`. The
/// estimate is the weights plus a tenth and a gibibyte for the runtime and
/// a modest context; a person can still pick a tighter fit, never an
/// unsupported one.
#[must_use]
pub fn rate_fit(
    hardware: &HardwareSnapshot,
    format: ModelArtifactFormat,
    download_bytes: u64,
) -> HubFit {
    let needs = (download_bytes.div_ceil(MIB).saturating_mul(11) / 10).saturating_add(1024);
    let rated = |fit, reason| HubFit {
        fit,
        reason,
        needs_memory_mib: needs,
    };
    if download_bytes.div_ceil(MIB) > hardware.free_storage_mib {
        return rated(HardwareFit::Unsupported, HubFitReason::NoStorage);
    }
    // vLLM keeps the whole model in accelerator memory; llama.cpp can split
    // its layers between accelerator and system memory, and reaches more
    // GPUs through Vulkan.
    let whole = format == ModelArtifactFormat::HfSnapshotFiles;
    let accelerator = usable_accelerator_mib(hardware, !whole);
    let memory = hardware.memory_mib.saturating_sub(DESKTOP_HEADROOM_MIB);
    // A GPU sharing system memory carries llama.cpp's work, so the processor
    // floor below does not apply to it.
    let shared_gpu = !whole
        && hardware
            .accelerators
            .iter()
            .any(|gpu| gpu.serves_vulkan() && gpu.vulkan_vram_mib() == 0);
    if accelerator > 0 {
        if needs <= (accelerator / 10).saturating_mul(9) {
            return rated(HardwareFit::Excellent, HubFitReason::Accelerator);
        }
        if whole {
            return if needs <= accelerator {
                rated(HardwareFit::Tight, HubFitReason::Accelerator)
            } else {
                rated(HardwareFit::Unsupported, HubFitReason::TooLarge)
            };
        }
        let pool = accelerator.saturating_add(memory);
        return if needs.saturating_mul(10) <= pool.saturating_mul(6) {
            rated(HardwareFit::Good, HubFitReason::AcceleratorAndMemory)
        } else if needs <= pool {
            rated(HardwareFit::Tight, HubFitReason::AcceleratorAndMemory)
        } else {
            rated(HardwareFit::Unsupported, HubFitReason::TooLarge)
        };
    }
    if !shared_gpu && (hardware.cpu_threads < 4 || hardware.memory_mib < 8192) {
        return rated(HardwareFit::Unsupported, HubFitReason::ProcessorTooSmall);
    }
    // The processor path is labelled honestly: never excellent, and a
    // snapshot vLLM serves on the processor is at best tight.
    if needs.saturating_mul(10) <= memory.saturating_mul(6) && !whole {
        rated(HardwareFit::Good, HubFitReason::Memory)
    } else if needs <= memory {
        rated(HardwareFit::Tight, HubFitReason::Memory)
    } else {
        rated(HardwareFit::Unsupported, HubFitReason::TooLarge)
    }
}

/// The largest dedicated accelerator memory a runtime can use: one whose
/// vendor driver is active, or with `vulkan`, any GPU Mesa drives.
fn usable_accelerator_mib(hardware: &HardwareSnapshot, vulkan: bool) -> u64 {
    hardware
        .accelerators
        .iter()
        .filter_map(|accelerator| match accelerator {
            Accelerator::Nvidia { vram_mib, driver } if !driver.trim().is_empty() => {
                Some(*vram_mib)
            }
            Accelerator::Amd {
                vram_mib,
                rocm: Some(version),
            } if !version.trim().is_empty() => Some(*vram_mib),
            gpu if vulkan && gpu.serves_vulkan() => Some(gpu.vulkan_vram_mib()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// A variant as Settings offers it: its files, how it fits and whether a
/// signed runtime on this machine can serve it yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubVariantOffer {
    pub variant: HubVariant,
    pub fit: HubFit,
    pub runtime_available: bool,
}

/// One model at a pinned commit, as Settings shows it before a download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubModelOffer {
    pub repo: String,
    /// The commit every file is pinned to.
    pub revision: String,
    pub license: HubLicense,
    pub gated: Gated,
    /// Whether a Hugging Face token is stored on this device.
    pub signed_in: bool,
    pub context_window: u32,
    pub variants: Vec<HubVariantOffer>,
}

impl HubModelOffer {
    /// What a person can pick from `info` on `hardware`; `runtime` says
    /// whether a signed runtime here serves a format.
    ///
    /// # Errors
    /// [`HubError::Invalid`] when the answer is not a model at a commit.
    pub fn of(
        info: &ApiModelInfo,
        hardware: &HardwareSnapshot,
        runtime: impl Fn(ModelArtifactFormat) -> bool,
        signed_in: bool,
    ) -> Result<Self, HubError> {
        if !valid_repo(&info.id) {
            return Err(HubError::Invalid("model id"));
        }
        if !is_commit(&info.sha) {
            return Err(HubError::Invalid("revision is not a commit"));
        }
        let context_window = info
            .gguf
            .as_ref()
            .and_then(|gguf| gguf.context_length)
            .and_then(|length| u32::try_from(length).ok())
            .filter(|length| (1..=1_048_576).contains(length))
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        Ok(Self {
            repo: info.id.clone(),
            revision: info.sha.clone(),
            license: HubLicense::of(info),
            gated: info.gated,
            signed_in,
            context_window,
            variants: variants(info)
                .into_iter()
                .map(|variant| HubVariantOffer {
                    fit: rate_fit(hardware, variant.format, variant.download_bytes()),
                    runtime_available: runtime(variant.format),
                    variant,
                })
                .collect(),
        })
    }

    #[must_use]
    pub fn variant(&self, name: &str) -> Option<&HubVariantOffer> {
        self.variants
            .iter()
            .find(|offer| offer.variant.name == name)
    }
}

/// Where a person's own model came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserModelSource {
    /// A Hub repository at a pinned commit.
    Hub { repo: String, revision: String },
    /// A GGUF file the person chose on this device, copied into the store
    /// from the handle the file chooser gave. `name` is its file name, for
    /// display: the service never opens a path it was told.
    File {
        #[serde(alias = "path")]
        name: String,
    },
}

/// A model a person picked themselves, as the model manager records it.
/// It runs through the same signed runtimes as a catalog release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserModel {
    pub id: String,
    pub label: String,
    pub source: UserModelSource,
    pub format: ModelArtifactFormat,
    pub files: Vec<HubFile>,
    pub license_id: String,
    pub license_url: String,
    pub context_window: u32,
}

impl UserModel {
    /// The variant `variant` of the offered model.
    ///
    /// # Errors
    /// [`HubError::Invalid`] for a variant that is not one of the offer's.
    pub fn from_offer(offer: &HubModelOffer, variant: &str) -> Result<Self, HubError> {
        let picked = offer
            .variant(variant)
            .ok_or(HubError::Invalid("no such variant"))?;
        let model = Self {
            id: user_model_id(&offer.repo, &picked.variant.name),
            label: format!("{} · {}", offer.repo, picked.variant.name),
            source: UserModelSource::Hub {
                repo: offer.repo.clone(),
                revision: offer.revision.clone(),
            },
            format: picked.variant.format,
            files: picked.variant.files.clone(),
            license_id: offer.license.id.clone(),
            license_url: offer.license.url.clone(),
            context_window: offer.context_window,
        };
        model.validate()?;
        Ok(model)
    }

    /// A GGUF file the person chose, called `name`, of `size_bytes`, whose
    /// bytes are hashed as they are copied in: `sha256` is filled in then.
    ///
    /// # Errors
    /// [`HubError::Invalid`] for a name that is not a `.gguf` file name.
    pub fn from_file(name: &str, size_bytes: u64, sha256: &str) -> Result<Self, HubError> {
        if name.contains('/')
            || name.len() > 255
            || name.chars().any(char::is_control)
            || !has_extension(name, "gguf")
            || size_bytes == 0
        {
            return Err(HubError::Invalid("a model file is a .gguf file"));
        }
        let stem = without_gguf(name);
        let model = Self {
            id: format!("file/{}", slug(stem, 96)),
            label: stem.to_owned(),
            source: UserModelSource::File { name: name.into() },
            format: ModelArtifactFormat::LlamaCppGguf,
            files: vec![HubFile {
                path: name.into(),
                size_bytes,
                digest: HubDigest::Sha256 {
                    sha256: sha256.into(),
                },
            }],
            license_id: "unknown".into(),
            license_url: String::new(),
            context_window: DEFAULT_CONTEXT_WINDOW,
        };
        model.validate()?;
        Ok(model)
    }

    /// Check what a record says before it is used or after it is read back.
    ///
    /// # Errors
    /// [`HubError::Invalid`] naming the first contradiction.
    pub fn validate(&self) -> Result<(), HubError> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self.id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_./".contains(&byte)
            })
        {
            return Err(HubError::Invalid("model id"));
        }
        if HubFormat::of(self.format).is_none() {
            return Err(HubError::Invalid("format"));
        }
        if self.files.is_empty()
            || self.files.len() > MAX_VARIANT_FILES
            || (self.format == ModelArtifactFormat::LlamaCppGguf && self.files.len() != 1)
            || self
                .files
                .iter()
                .any(|file| !safe_path(&file.path) || file.size_bytes == 0)
        {
            return Err(HubError::Invalid("files"));
        }
        let mut paths: Vec<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
        paths.sort_unstable();
        if paths
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a == b))
        {
            return Err(HubError::Invalid("duplicate file"));
        }
        match &self.source {
            UserModelSource::Hub { repo, revision } => {
                if !valid_repo(repo) || !is_commit(revision) {
                    return Err(HubError::Invalid("source"));
                }
                if self.files.iter().any(|file| match &file.digest {
                    HubDigest::Sha256 { sha256 } => !is_sha256(sha256),
                    HubDigest::GitBlob { oid } => !is_sha1(oid),
                }) {
                    return Err(HubError::Invalid("digest"));
                }
            }
            UserModelSource::File { .. } => {}
        }
        if self.context_window == 0 || self.context_window > 1_048_576 {
            return Err(HubError::Invalid("context window"));
        }
        Ok(())
    }

    #[must_use]
    pub fn download_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size_bytes).sum()
    }

    /// Where `file` downloads from; `None` for a file on this device.
    #[must_use]
    pub fn file_url(&self, file: &HubFile) -> Option<String> {
        match &self.source {
            UserModelSource::Hub { repo, revision } => Some(file_url(repo, revision, &file.path)),
            UserModelSource::File { .. } => None,
        }
    }

    /// The model's page, or its file's name, for display.
    #[must_use]
    pub fn source_label(&self) -> String {
        match &self.source {
            UserModelSource::Hub { repo, .. } => model_page_url(repo),
            UserModelSource::File { name } => name.clone(),
        }
    }

    /// The pinned commit, or `local` for a file on this device.
    #[must_use]
    pub fn revision(&self) -> &str {
        match &self.source {
            UserModelSource::Hub { revision, .. } => revision,
            UserModelSource::File { .. } => "local",
        }
    }

    /// The release the runtime path launches. `identity` names the model's
    /// bytes in the store: the one file's SHA-256, or the digest of the
    /// file list of a directory layout. A person's pick declares no vision
    /// or face capability: nothing signed vouches for one.
    #[must_use]
    pub fn release(&self, identity: &str) -> LocalModelRelease {
        let size = self.download_bytes();
        LocalModelRelease {
            id: self.id.clone(),
            label: self.label.clone(),
            description: self.source_label(),
            version: self.revision().to_owned(),
            artifact: PinnedArtifact {
                url: match &self.source {
                    UserModelSource::Hub { repo, revision } => {
                        format!("{HUB_ORIGIN}/{repo}/tree/{revision}")
                    }
                    UserModelSource::File { .. } => format!("{HUB_ORIGIN}/"),
                },
                sha256: identity.to_owned(),
                size_bytes: size,
            },
            artifact_format: self.format,
            license_id: self.license_id.clone(),
            license_url: self.license_url.clone(),
            requires_acceptance: matches!(self.source, UserModelSource::Hub { .. }),
            installed_size_bytes: size,
            // Any signed runtime of the matching engine may serve it; the
            // hardware decides which, as for a catalog release.
            allowed_runtimes: RuntimeKind::PREFERENCE.into(),
            required_memory_mib: 0,
            required_accelerator_mib: 0,
            context_window: self.context_window,
            vision: false,
            face_recognition: false,
            segmentation: false,
        }
    }
}

/// `hf/<owner>/<name>/<variant>` in the runtime's alias alphabet: lowercase
/// letters, digits and `-_./`, at most 128 bytes.
#[must_use]
pub fn user_model_id(repo: &str, variant: &str) -> String {
    let (owner, name) = repo.split_once('/').unwrap_or(("", repo));
    format!(
        "hf/{}/{}/{}",
        slug(owner, 40),
        slug(name, 56),
        slug(variant, 26)
    )
}

fn slug(text: &str, limit: usize) -> String {
    let slug: String = text
        .chars()
        .map(|character| {
            let lower = character.to_ascii_lowercase();
            if lower.is_ascii_lowercase() || lower.is_ascii_digit() || "-_.".contains(lower) {
                lower
            } else {
                '-'
            }
        })
        .take(limit)
        .collect();
    let slug = slug.trim_start_matches(['-', '.']).to_owned();
    if slug.is_empty() {
        "model".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests;
