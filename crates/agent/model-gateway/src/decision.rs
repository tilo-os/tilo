//! Typed decision-model protocol, confidence tiers, and non-permission gating.
//!
//! Decision models (`Jev` / `Kev`) answer batched typed questions (`YesNo`,
//! `Choice`, `Score`) with calibrated probabilities over a single material
//! payload and optional known matter metadata in `state`.
//!
//! Four invariants govern every caller:
//! 1. **Non-permission invariant (Rule 1):** a decision verdict only screens
//!    telling or already-authorized work. It never constructs, widens, or
//!    bypasses an external permission or workspace grant; see [`screen_effect`]
//!    and [`ExternalAuthority`].
//! 2. **Risk-tiered confidence thresholds:** [`ThresholdTier::Tell`] uses a
//!    lower confidence bar to surface news, while [`ThresholdTier::ActAlone`]
//!    uses a stricter bar with a wider middle escalation band.
//! 3. **Pinned model versions:** moving aliases (`jev-latest`, `default`,
//!    `auto`) are rejected by [`PinnedModelVersion`].
//! 4. **Explicit unknown outcomes:** missing answers, abstentions, invalid
//!    probabilities, and endpoint outages map to [`AnswerOutcome::Unknown`]
//!    and [`Verdict::Unsure`] — never to `false`.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_net::{Client, Redirects, header};
use url::Url;
use zeroize::Zeroizing;

/// Default short HTTP timeout for a decision-model batch request.
pub const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(3);
/// Upper bound on decision-model HTTP timeouts so callers fail fast and fall back.
pub const MAX_DECISION_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum response body read from a decision endpoint, in bytes.
pub const MAX_DECISION_RESPONSE_BYTES: usize = 256 * 1024;
/// Maximum number of typed questions permitted in a single decision batch.
pub const MAX_QUESTIONS_PER_BATCH: usize = 128;

/// Default `Tell` tier thresholds: lower bar for surfacing news to the person.
const TELL_NO_MAX: f32 = 0.30;
const TELL_YES_MIN: f32 = 0.70;
/// Default `ActAlone` tier thresholds: stricter bar for already-authorized work.
const ACT_ALONE_NO_MAX: f32 = 0.15;
const ACT_ALONE_YES_MIN: f32 = 0.90;

/// Endpoint path profile for Jev/Kev-compatible decision APIs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointProfile {
    /// `TypeSafe` direct and local Kev endpoint (`POST /v1/systemone`).
    SystemOne,
    /// `OpenRouter` multimodal decisions endpoint (`POST /api/alpha/decisions`).
    OpenRouterDecisions,
    /// Caller-supplied relative endpoint path.
    Custom(String),
}

impl EndpointProfile {
    /// Relative HTTP path appended to the endpoint base URL.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::SystemOne => "/v1/systemone",
            Self::OpenRouterDecisions => "/api/alpha/decisions",
            Self::Custom(path) => path.as_str(),
        }
    }
}

/// A pinned decision-model identifier (for example `jev-1.14.0` or `kev-27b@v1.0`).
///
/// Moving aliases such as `jev-latest`, `latest`, `default`, or `auto` are
/// rejected so thresholds never drift silently across silent upstream updates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PinnedModelVersion(String);

impl PinnedModelVersion {
    /// Validate and construct a pinned model version identifier.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DecisionError> {
        let value = raw.into();
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.len() != value.len() {
            return Err(DecisionError::InvalidModelVersion(
                "model version must be non-empty without surrounding whitespace".to_owned(),
            ));
        }
        if value.chars().any(char::is_whitespace) {
            return Err(DecisionError::InvalidModelVersion(
                "model version must not contain whitespace".to_owned(),
            ));
        }
        let lower = value.to_ascii_lowercase();
        let is_moving_alias = matches!(
            lower.as_str(),
            "latest" | "default" | "auto" | "current" | "stable"
        ) || lower.ends_with("-latest")
            || lower.ends_with(":latest")
            || lower.ends_with("@latest")
            || lower.ends_with("/latest")
            || lower.ends_with("-stable")
            || lower.ends_with(":stable")
            || lower.ends_with("@stable");
        if is_moving_alias {
            return Err(DecisionError::InvalidModelVersion(format!(
                "model version `{value}` is a moving alias; pin an explicit release"
            )));
        }
        if !value.chars().any(|ch| ch.is_ascii_digit()) {
            return Err(DecisionError::InvalidModelVersion(format!(
                "model version `{value}` must include an explicit version number"
            )));
        }
        Ok(Self(value))
    }

    /// The pinned model version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for PinnedModelVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for PinnedModelVersion {
    type Error = DecisionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<PinnedModelVersion> for String {
    fn from(value: PinnedModelVersion) -> Self {
        value.0
    }
}

/// The material payload evaluated once across all questions in a batch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionMaterial {
    /// Matter text being judged (message text, summary, or event payload).
    pub text: String,
    /// Known structured metadata (such as sender groups and active situations)
    /// passed alongside `text` so questions do not guess durable facts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state: BTreeMap<String, Value>,
}

impl DecisionMaterial {
    /// Construct a material payload from matter text and empty metadata state.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            state: BTreeMap::new(),
        }
    }

    /// Attach one structured metadata entry to `state`.
    #[must_use]
    pub fn with_state(mut self, key: impl Into<String>, value: Value) -> Self {
        self.state.insert(key.into(), value);
        self
    }
}

/// One typed question posed to a decision model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DecisionQuestion {
    /// Independent yes/no question (Jev `Noul` primitive).
    #[serde(alias = "noul")]
    YesNo { id: String, question: String },
    /// Mutually exclusive selection among explicit options.
    Choice {
        id: String,
        question: String,
        options: Vec<String>,
    },
    /// Continuous normalized score question in `[0.0, 1.0]`.
    Score { id: String, question: String },
}

impl DecisionQuestion {
    /// Stable question identifier echoed back in [`DecisionAnswer::id`].
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::YesNo { id, .. } | Self::Choice { id, .. } | Self::Score { id, .. } => {
                id.as_str()
            }
        }
    }

    /// Validate that this question is well-formed before sending.
    fn validate(&self) -> Result<(), DecisionError> {
        let (id, question) = match self {
            Self::YesNo { id, question }
            | Self::Choice { id, question, .. }
            | Self::Score { id, question } => (id.as_str(), question.as_str()),
        };
        if id.trim().is_empty() {
            return Err(DecisionError::InvalidRequest(
                "question id must be non-empty".to_owned(),
            ));
        }
        if question.trim().is_empty() {
            return Err(DecisionError::InvalidRequest(format!(
                "question `{id}` text must be non-empty"
            )));
        }
        if let Self::Choice { options, .. } = self {
            if options.len() < 2 {
                return Err(DecisionError::InvalidRequest(format!(
                    "choice question `{id}` requires at least two options"
                )));
            }
            let mut seen = HashSet::with_capacity(options.len());
            for option in options {
                if option.trim().is_empty() {
                    return Err(DecisionError::InvalidRequest(format!(
                        "choice question `{id}` contains an empty option"
                    )));
                }
                if !seen.insert(option.as_str()) {
                    return Err(DecisionError::InvalidRequest(format!(
                        "choice question `{id}` contains duplicate option `{option}`"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Batched decision request sent to a Jev/Kev-compatible endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// Pinned decision-model version identifier.
    pub model: PinnedModelVersion,
    /// Shared material and metadata read once for every question in `questions`.
    pub material: DecisionMaterial,
    /// Independent typed questions answered in one request.
    pub questions: Vec<DecisionQuestion>,
}

impl DecisionRequest {
    /// Construct a new batched decision request.
    #[must_use]
    pub const fn new(
        model: PinnedModelVersion,
        material: DecisionMaterial,
        questions: Vec<DecisionQuestion>,
    ) -> Self {
        Self {
            model,
            material,
            questions,
        }
    }

    /// Validate that the batch is non-empty, bounded, and has unique question IDs.
    pub fn validate(&self) -> Result<(), DecisionError> {
        if self.questions.is_empty() {
            return Err(DecisionError::InvalidRequest(
                "decision request must include at least one question".to_owned(),
            ));
        }
        if self.questions.len() > MAX_QUESTIONS_PER_BATCH {
            return Err(DecisionError::InvalidRequest(format!(
                "decision request exceeds maximum of {MAX_QUESTIONS_PER_BATCH} questions"
            )));
        }
        let mut ids = HashSet::with_capacity(self.questions.len());
        for question in &self.questions {
            question.validate()?;
            if !ids.insert(question.id()) {
                return Err(DecisionError::InvalidRequest(format!(
                    "duplicate question id `{}`",
                    question.id()
                )));
            }
        }
        Ok(())
    }
}

/// Why a decision answer resolved to [`AnswerOutcome::Unknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// Required facts were absent from material and state.
    MissingFacts,
    /// The model explicitly abstained from deciding.
    Abstained,
    /// The endpoint omitted this question from its response payload.
    OmittedByEndpoint,
    /// A probability or score was missing, non-finite, or outside `[0.0, 1.0]`.
    InvalidProbability,
    /// A choice response named an option not present in the request.
    UnrecognizedOption,
    /// The returned answer shape did not match the requested question kind.
    KindMismatch,
}

/// Typed outcome for one decision question.
///
/// Silence, omission, or invalid output always maps to [`Self::Unknown`]
/// rather than coercing to `false`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerOutcome {
    /// Calibrated probability in `[0.0, 1.0]` that the yes/no question holds.
    YesNo { probability: f32 },
    /// Selected option label and its confidence probability in `[0.0, 1.0]`.
    Choice { option: String, probability: f32 },
    /// Continuous normalized score in `[0.0, 1.0]` with optional confidence.
    Score {
        score: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probability: Option<f32>,
    },
    /// Explicit unknown outcome; never coerced to `false`.
    Unknown { reason: UnknownReason },
}

/// One typed answer paired with its question identifier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionAnswer {
    pub id: String,
    pub outcome: AnswerOutcome,
}

impl DecisionAnswer {
    /// Whether this answer is explicitly [`AnswerOutcome::Unknown`].
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self.outcome, AnswerOutcome::Unknown { .. })
    }

    /// Apply risk-tiered confidence thresholds to produce a [`Verdict`].
    #[must_use]
    pub fn verdict(&self, thresholds: ConfidenceThresholds) -> Verdict {
        thresholds.evaluate_outcome(&self.outcome)
    }
}

/// Parsed response for a [`DecisionRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    /// Model version echoed by the endpoint (or the requested pinned version).
    pub model: String,
    /// One typed answer per requested question, in request order.
    pub answers: Vec<DecisionAnswer>,
}

impl DecisionResponse {
    /// Look up the typed answer for question `id`.
    #[must_use]
    pub fn answer(&self, id: &str) -> Option<&DecisionAnswer> {
        self.answers.iter().find(|answer| answer.id == id)
    }
}

/// Three-valued decision verdict after applying confidence thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Confidence met or exceeded the tier's `yes_min` threshold.
    Yes,
    /// Confidence fell at or below the tier's `no_max` threshold.
    No,
    /// Confidence landed in the uncertain middle band or the answer was unknown.
    Unsure,
}

/// Risk tier selecting confidence thresholds for decision verdicts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdTier {
    /// Lower threshold for surfacing news or telling the person.
    Tell,
    /// Higher threshold for gating already-authorized autonomous work.
    ///
    /// Rule 1: meeting this threshold never grants an external permission;
    /// callers must also hold an independent [`ExternalAuthority::Granted`].
    ActAlone,
}

impl ThresholdTier {
    /// Default calibrated probability thresholds for this risk tier.
    #[must_use]
    pub const fn default_thresholds(self) -> ConfidenceThresholds {
        match self {
            Self::Tell => ConfidenceThresholds {
                no_max: TELL_NO_MAX,
                yes_min: TELL_YES_MIN,
            },
            Self::ActAlone => ConfidenceThresholds {
                no_max: ACT_ALONE_NO_MAX,
                yes_min: ACT_ALONE_YES_MIN,
            },
        }
    }
}

/// Validated probability thresholds (`0.0 <= no_max < yes_min <= 1.0`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceThresholds {
    no_max: f32,
    yes_min: f32,
}

impl ConfidenceThresholds {
    /// Construct custom thresholds after verifying `0.0 <= no_max < yes_min <= 1.0`.
    pub fn new(no_max: f32, yes_min: f32) -> Result<Self, DecisionError> {
        if !valid_unit_float(no_max) || !valid_unit_float(yes_min) {
            return Err(DecisionError::InvalidThresholds(
                "thresholds must be finite values within [0.0, 1.0]".to_owned(),
            ));
        }
        if no_max >= yes_min {
            return Err(DecisionError::InvalidThresholds(
                "no_max must be strictly less than yes_min".to_owned(),
            ));
        }
        Ok(Self { no_max, yes_min })
    }

    /// Upper probability bound (inclusive) classified as [`Verdict::No`].
    #[must_use]
    pub const fn no_max(self) -> f32 {
        self.no_max
    }

    /// Lower probability bound (inclusive) classified as [`Verdict::Yes`].
    #[must_use]
    pub const fn yes_min(self) -> f32 {
        self.yes_min
    }

    /// Map a raw probability to [`Verdict`]; invalid numbers yield [`Verdict::Unsure`].
    #[must_use]
    pub fn evaluate_probability(self, probability: f32) -> Verdict {
        if !valid_unit_float(probability) {
            return Verdict::Unsure;
        }
        if probability >= self.yes_min {
            Verdict::Yes
        } else if probability <= self.no_max {
            Verdict::No
        } else {
            Verdict::Unsure
        }
    }

    /// Map a typed [`AnswerOutcome`] to [`Verdict`].
    ///
    /// [`AnswerOutcome::Unknown`] always evaluates to [`Verdict::Unsure`].
    #[must_use]
    pub fn evaluate_outcome(self, outcome: &AnswerOutcome) -> Verdict {
        match *outcome {
            AnswerOutcome::YesNo { probability } | AnswerOutcome::Choice { probability, .. } => {
                self.evaluate_probability(probability)
            }
            AnswerOutcome::Score { score, probability } => {
                if let Some(confidence) = probability
                    && (!valid_unit_float(confidence) || confidence < self.yes_min)
                {
                    return Verdict::Unsure;
                }
                self.evaluate_probability(score)
            }
            AnswerOutcome::Unknown { .. } => Verdict::Unsure,
        }
    }
}

/// External authorization established independently of any decision model.
///
/// Rule 1: contracts, workspace grants, and explicit user approvals produce
/// [`Self::Granted`]. No decision-model type or verdict can produce it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalAuthority {
    /// An independent grant already permits the external action.
    Granted,
    /// No independent grant exists; autonomous execution is forbidden.
    Absent,
}

/// The only gating effects a decision-model verdict may produce (Rule 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenEffect {
    /// Surface to the person (`Tell`) or proceed with already-authorized work.
    Proceed,
    /// Stay quiet or skip work.
    Suppress,
    /// Escalate to the person or the large model; never guess or self-authorize.
    Escalate,
}

/// Enforce Rule 1: combine a risk tier, a decision [`Verdict`], and independent
/// [`ExternalAuthority`] into a [`ScreenEffect`].
///
/// - Under [`ThresholdTier::Tell`], `Yes` surfaces to the person (`Proceed`),
///   `No` suppresses, and `Unsure` escalates.
/// - Under [`ThresholdTier::ActAlone`], `Proceed` requires **both**
///   `Verdict::Yes` and `ExternalAuthority::Granted`. Without independent
///   authority, even a high-confidence `Verdict::Yes` downgrades to
///   [`ScreenEffect::Escalate`].
#[must_use]
pub const fn screen_effect(
    tier: ThresholdTier,
    verdict: Verdict,
    authority: ExternalAuthority,
) -> ScreenEffect {
    match (tier, verdict, authority) {
        (ThresholdTier::Tell, Verdict::Yes, _)
        | (ThresholdTier::ActAlone, Verdict::Yes, ExternalAuthority::Granted) => {
            ScreenEffect::Proceed
        }
        (ThresholdTier::ActAlone, Verdict::Yes, ExternalAuthority::Absent)
        | (_, Verdict::Unsure, _) => ScreenEffect::Escalate,
        (_, Verdict::No, _) => ScreenEffect::Suppress,
    }
}

/// Convenience helper that evaluates an [`AnswerOutcome`] under `thresholds`
/// and applies Rule 1 via [`screen_effect`].
#[must_use]
pub fn screen_outcome(
    tier: ThresholdTier,
    thresholds: ConfidenceThresholds,
    outcome: &AnswerOutcome,
    authority: ExternalAuthority,
) -> ScreenEffect {
    screen_effect(tier, thresholds.evaluate_outcome(outcome), authority)
}

/// Errors returned while validating, sending, or parsing a decision request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionError {
    InvalidModelVersion(String),
    InvalidThresholds(String),
    InvalidRequest(String),
    InvalidEndpoint(String),
    TimedOut { timeout: Duration },
    Unavailable { message: String },
    Rejected { status: u16 },
    ResponseTooLarge { limit: usize },
    InvalidResponse(String),
}

impl DecisionError {
    /// Whether callers should treat this failure as an outage and fall back
    /// cleanly (never coercing the missing verdict to `false`).
    #[must_use]
    pub const fn should_fallback(&self) -> bool {
        matches!(
            self,
            Self::TimedOut { .. }
                | Self::Unavailable { .. }
                | Self::ResponseTooLarge { .. }
                | Self::InvalidResponse(_)
        )
    }
}

impl fmt::Display for DecisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModelVersion(message) => write!(f, "invalid decision model: {message}"),
            Self::InvalidThresholds(message) => {
                write!(f, "invalid confidence thresholds: {message}")
            }
            Self::InvalidRequest(message) => write!(f, "invalid decision request: {message}"),
            Self::InvalidEndpoint(message) => write!(f, "invalid decision endpoint: {message}"),
            Self::TimedOut { timeout } => {
                write!(
                    f,
                    "decision endpoint timed out after {}ms",
                    timeout.as_millis()
                )
            }
            Self::Unavailable { message } => write!(f, "decision endpoint unavailable: {message}"),
            Self::Rejected { status } => {
                write!(f, "decision endpoint rejected request: HTTP {status}")
            }
            Self::ResponseTooLarge { limit } => {
                write!(f, "decision response exceeded {limit} bytes")
            }
            Self::InvalidResponse(message) => write!(f, "invalid decision response: {message}"),
        }
    }
}

impl std::error::Error for DecisionError {}

/// Target endpoint configuration for a decision-model call.
#[derive(Clone)]
pub struct DecisionEndpoint {
    base_url: Url,
    profile: EndpointProfile,
    bearer_token: Option<Zeroizing<String>>,
    timeout: Duration,
}

impl fmt::Debug for DecisionEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionEndpoint")
            .field("base_url", &self.base_url.as_str())
            .field("profile", &self.profile)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[redacted]"),
            )
            .field("timeout", &self.timeout)
            .finish()
    }
}

impl DecisionEndpoint {
    /// Configure a decision endpoint with `base_url`, `profile`, and
    /// [`DEFAULT_DECISION_TIMEOUT`].
    pub fn new(base_url: &str, profile: EndpointProfile) -> Result<Self, DecisionError> {
        let parsed = Url::parse(base_url)
            .map_err(|error| DecisionError::InvalidEndpoint(format!("{base_url}: {error}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(DecisionError::InvalidEndpoint(
                "endpoint URL must use http or https".to_owned(),
            ));
        }
        Ok(Self {
            base_url: parsed,
            profile,
            bearer_token: None,
            timeout: DEFAULT_DECISION_TIMEOUT,
        })
    }

    /// Attach a bearer token sent in the `Authorization` header.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        self.bearer_token = if token.is_empty() {
            None
        } else {
            Some(Zeroizing::new(token))
        };
        self
    }

    /// Override the per-request timeout, clamped to `[1ms, MAX_DECISION_TIMEOUT]`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        let clamped = if timeout.is_zero() {
            Duration::from_millis(1)
        } else if timeout > MAX_DECISION_TIMEOUT {
            MAX_DECISION_TIMEOUT
        } else {
            timeout
        };
        self.timeout = clamped;
        self
    }

    /// Configured timeout for this endpoint.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Endpoint profile selecting the relative API path.
    #[must_use]
    pub const fn profile(&self) -> &EndpointProfile {
        &self.profile
    }

    /// Resolve the full POST URL by combining `base_url` and `profile.path()`.
    pub fn resolved_url(&self) -> Result<Url, DecisionError> {
        let path = self.profile.path().trim();
        if path.is_empty() || !path.starts_with('/') {
            return Err(DecisionError::InvalidEndpoint(
                "endpoint profile path must start with `/`".to_owned(),
            ));
        }
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        let resolved_path = if base_path.is_empty() || base_path == "/" {
            path.to_owned()
        } else if base_path.ends_with(path) {
            base_path.to_owned()
        } else {
            format!("{base_path}{path}")
        };
        url.set_path(&resolved_path);
        Ok(url)
    }
}

/// Async HTTP client for Jev/Kev-compatible decision endpoints.
#[derive(Clone, Debug)]
pub struct DecisionClient {
    http: Client,
}

impl DecisionClient {
    /// Build a client with redirects disabled and no ambient proxy lookup.
    pub fn new() -> Result<Self, DecisionError> {
        let builder = Client::builder().redirects(Redirects::None);
        #[cfg(not(target_family = "wasm"))]
        let builder = builder.direct();
        let http = builder
            .build()
            .map_err(|error| DecisionError::Unavailable {
                message: error.to_string(),
            })?;
        Ok(Self { http })
    }

    /// Wrap an existing configured [`tilo_net::Client`].
    #[must_use]
    pub const fn with_client(http: Client) -> Self {
        Self { http }
    }

    /// Send a validated batched decision request with a short timeout and
    /// clean fallback error reporting on outage.
    pub async fn decide(
        &self,
        endpoint: &DecisionEndpoint,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse, DecisionError> {
        request.validate()?;
        let url = endpoint.resolved_url()?;
        let body = serde_json::to_vec(request)
            .map_err(|error| DecisionError::InvalidRequest(error.to_string()))?;

        let mut builder = self
            .http
            .post(url.as_str())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .timeout(endpoint.timeout())
            .body(body);
        if let Some(token) = &endpoint.bearer_token {
            builder = builder.bearer_auth(token.as_str());
        }

        let response = builder.send().await.map_err(|error| {
            if error.is_timeout() {
                DecisionError::TimedOut {
                    timeout: endpoint.timeout(),
                }
            } else {
                DecisionError::Unavailable {
                    message: error.to_string(),
                }
            }
        })?;

        let status = response.status();
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(DecisionError::Unavailable {
                message: format!("HTTP {}", status.as_u16()),
            });
        }
        if status.is_client_error() {
            return Err(DecisionError::Rejected {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            return Err(DecisionError::Unavailable {
                message: format!("unexpected HTTP {}", status.as_u16()),
            });
        }

        let bytes = response
            .bytes_limited(MAX_DECISION_RESPONSE_BYTES)
            .await
            .map_err(|error| match error {
                tilo_net::NetError::TooLarge { limit } => DecisionError::ResponseTooLarge { limit },
                tilo_net::NetError::Timeout => DecisionError::TimedOut {
                    timeout: endpoint.timeout(),
                },
                other => DecisionError::Unavailable {
                    message: other.to_string(),
                },
            })?;

        parse_response(request, &bytes)
    }
}

/// Wire payload accepted by [`parse_response`], covering both canonical
/// [`DecisionResponse`] JSON and Jev/SystemOne/OpenRouter response envelopes.
#[derive(Deserialize)]
struct RawEnvelope {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    answers: Option<Vec<RawAnswer>>,
    #[serde(default)]
    decisions: Option<Vec<RawAnswer>>,
    #[serde(default)]
    results: Option<Vec<RawAnswer>>,
}

#[derive(Deserialize)]
struct RawAnswer {
    id: String,
    #[serde(default)]
    outcome: Option<Value>,
    #[serde(default)]
    unknown: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default, alias = "p", alias = "confidence")]
    probability: Option<f32>,
    #[serde(default, alias = "choice")]
    option: Option<String>,
    #[serde(default)]
    score: Option<f32>,
}

/// Parse a decision endpoint's JSON response against `request`.
///
/// Every question in `request.questions` receives an entry in the returned
/// [`DecisionResponse`] in request order. Missing entries, unknown outcomes,
/// out-of-range probabilities, or unrecognized choice labels map to
/// [`AnswerOutcome::Unknown`] rather than `false`.
pub fn parse_response(
    request: &DecisionRequest,
    raw_bytes: &[u8],
) -> Result<DecisionResponse, DecisionError> {
    request.validate()?;
    let envelope: RawEnvelope = serde_json::from_slice(raw_bytes)
        .map_err(|error| DecisionError::InvalidResponse(error.to_string()))?;
    let raw_items = envelope
        .answers
        .or(envelope.decisions)
        .or(envelope.results)
        .ok_or_else(|| {
            DecisionError::InvalidResponse(
                "response missing `answers`, `decisions`, or `results` array".to_owned(),
            )
        })?;

    let mut by_id: BTreeMap<String, RawAnswer> = BTreeMap::new();
    for item in raw_items {
        by_id.entry(item.id.clone()).or_insert(item);
    }

    let mut answers = Vec::with_capacity(request.questions.len());
    for question in &request.questions {
        let outcome = match by_id.get(question.id()) {
            None => AnswerOutcome::Unknown {
                reason: UnknownReason::OmittedByEndpoint,
            },
            Some(raw) => parse_answer_outcome(question, raw),
        };
        answers.push(DecisionAnswer {
            id: question.id().to_owned(),
            outcome,
        });
    }

    Ok(DecisionResponse {
        model: envelope
            .model
            .filter(|model| !model.trim().is_empty())
            .unwrap_or_else(|| request.model.as_str().to_owned()),
        answers,
    })
}

fn parse_answer_outcome(question: &DecisionQuestion, raw: &RawAnswer) -> AnswerOutcome {
    if raw.unknown {
        return AnswerOutcome::Unknown {
            reason: parse_unknown_reason(raw.reason.as_deref()),
        };
    }

    if let Some(outcome_value) = &raw.outcome {
        if outcome_value.is_null() {
            return AnswerOutcome::Unknown {
                reason: UnknownReason::Abstained,
            };
        }
        if let Some(label) = outcome_value.as_str() {
            if label.eq_ignore_ascii_case("unknown") || label.eq_ignore_ascii_case("abstain") {
                return AnswerOutcome::Unknown {
                    reason: parse_unknown_reason(raw.reason.as_deref()),
                };
            }
            return AnswerOutcome::Unknown {
                reason: UnknownReason::KindMismatch,
            };
        }
        if let Ok(typed) = serde_json::from_value::<AnswerOutcome>(outcome_value.clone()) {
            return validate_outcome_for_question(question, typed);
        }
        return AnswerOutcome::Unknown {
            reason: UnknownReason::KindMismatch,
        };
    }

    match question {
        DecisionQuestion::YesNo { .. } => match raw.probability {
            Some(probability) if valid_unit_float(probability) => {
                AnswerOutcome::YesNo { probability }
            }
            Some(_) => AnswerOutcome::Unknown {
                reason: UnknownReason::InvalidProbability,
            },
            None => AnswerOutcome::Unknown {
                reason: UnknownReason::KindMismatch,
            },
        },
        DecisionQuestion::Choice { options, .. } => match (&raw.option, raw.probability) {
            (Some(option), Some(probability)) => {
                if !options.iter().any(|allowed| allowed == option) {
                    AnswerOutcome::Unknown {
                        reason: UnknownReason::UnrecognizedOption,
                    }
                } else if !valid_unit_float(probability) {
                    AnswerOutcome::Unknown {
                        reason: UnknownReason::InvalidProbability,
                    }
                } else {
                    AnswerOutcome::Choice {
                        option: option.clone(),
                        probability,
                    }
                }
            }
            (Some(_), None) => AnswerOutcome::Unknown {
                reason: UnknownReason::InvalidProbability,
            },
            _ => AnswerOutcome::Unknown {
                reason: UnknownReason::KindMismatch,
            },
        },
        DecisionQuestion::Score { .. } => match raw.score {
            Some(score) if valid_unit_float(score) => {
                if let Some(probability) = raw.probability
                    && !valid_unit_float(probability)
                {
                    return AnswerOutcome::Unknown {
                        reason: UnknownReason::InvalidProbability,
                    };
                }
                AnswerOutcome::Score {
                    score,
                    probability: raw.probability,
                }
            }
            Some(_) => AnswerOutcome::Unknown {
                reason: UnknownReason::InvalidProbability,
            },
            None => AnswerOutcome::Unknown {
                reason: UnknownReason::KindMismatch,
            },
        },
    }
}

fn validate_outcome_for_question(
    question: &DecisionQuestion,
    outcome: AnswerOutcome,
) -> AnswerOutcome {
    match (question, outcome) {
        (_, AnswerOutcome::Unknown { reason }) => AnswerOutcome::Unknown { reason },
        (DecisionQuestion::YesNo { .. }, AnswerOutcome::YesNo { probability }) => {
            if valid_unit_float(probability) {
                AnswerOutcome::YesNo { probability }
            } else {
                AnswerOutcome::Unknown {
                    reason: UnknownReason::InvalidProbability,
                }
            }
        }
        (
            DecisionQuestion::Choice { options, .. },
            AnswerOutcome::Choice {
                option,
                probability,
            },
        ) => {
            if !options.iter().any(|allowed| allowed == &option) {
                AnswerOutcome::Unknown {
                    reason: UnknownReason::UnrecognizedOption,
                }
            } else if !valid_unit_float(probability) {
                AnswerOutcome::Unknown {
                    reason: UnknownReason::InvalidProbability,
                }
            } else {
                AnswerOutcome::Choice {
                    option,
                    probability,
                }
            }
        }
        (DecisionQuestion::Score { .. }, AnswerOutcome::Score { score, probability }) => {
            if !valid_unit_float(score) || probability.is_some_and(|value| !valid_unit_float(value))
            {
                AnswerOutcome::Unknown {
                    reason: UnknownReason::InvalidProbability,
                }
            } else {
                AnswerOutcome::Score { score, probability }
            }
        }
        _ => AnswerOutcome::Unknown {
            reason: UnknownReason::KindMismatch,
        },
    }
}

fn parse_unknown_reason(reason: Option<&str>) -> UnknownReason {
    match reason.map(str::to_ascii_lowercase).as_deref() {
        Some("abstained" | "abstain") => UnknownReason::Abstained,
        Some("omitted_by_endpoint" | "omitted") => UnknownReason::OmittedByEndpoint,
        Some("invalid_probability") => UnknownReason::InvalidProbability,
        Some("unrecognized_option") => UnknownReason::UnrecognizedOption,
        Some("kind_mismatch") => UnknownReason::KindMismatch,
        _ => UnknownReason::MissingFacts,
    }
}

fn valid_unit_float(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_request() -> DecisionRequest {
        let model = PinnedModelVersion::parse("jev-1.14.0").unwrap();
        let material = DecisionMaterial::new("Jonas asks about our Q3 pipeline.")
            .with_state("sender_groups", json!(["Investors", "Customers"]));
        DecisionRequest::new(
            model,
            material,
            vec![
                DecisionQuestion::YesNo {
                    id: "q-investor".into(),
                    question: "Is this message about an investor relationship?".into(),
                },
                DecisionQuestion::Choice {
                    id: "q-urgency".into(),
                    question: "Which bucket best describes the timing?".into(),
                    options: vec!["now".into(), "this_week".into(), "later".into()],
                },
                DecisionQuestion::Score {
                    id: "q-relevance".into(),
                    question: "How directly does this affect Q3 goals?".into(),
                },
            ],
        )
    }

    #[test]
    fn pinned_model_version_rejects_moving_aliases_and_accepts_pinned_ids() {
        for valid in [
            "jev-1.14.0",
            "kev-27b@v1.0",
            "typesafe/jev-1.14.0",
            "kev-4b-v1",
        ] {
            assert_eq!(
                PinnedModelVersion::parse(valid).unwrap().as_str(),
                valid,
                "{valid}"
            );
        }
        for invalid in [
            "",
            "   ",
            "jev-latest",
            "kev:latest",
            "kev@latest",
            "latest",
            "default",
            "auto",
            "jev-stable",
            "jev",
            "jev 1.14.0",
        ] {
            assert!(
                PinnedModelVersion::parse(invalid).is_err(),
                "expected `{invalid}` to be rejected"
            );
        }
    }

    #[test]
    fn request_serialization_and_validation_cover_all_question_kinds() {
        let request = sample_request();
        request.validate().unwrap();

        let serialized = serde_json::to_string(&request).unwrap();
        let decoded: DecisionRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(decoded, request);

        let wire: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(wire["model"], "jev-1.14.0");
        assert_eq!(wire["questions"][0]["type"], "yes_no");
        assert_eq!(wire["questions"][1]["type"], "choice");
        assert_eq!(wire["questions"][2]["type"], "score");

        // Duplicate question IDs and under-specified choice options fail validation.
        let mut duplicate = sample_request();
        duplicate.questions.push(DecisionQuestion::YesNo {
            id: "q-investor".into(),
            question: "Duplicate id".into(),
        });
        assert!(matches!(
            duplicate.validate(),
            Err(DecisionError::InvalidRequest(_))
        ));

        let bad_choice = DecisionRequest::new(
            PinnedModelVersion::parse("jev-1.14.0").unwrap(),
            DecisionMaterial::new("item"),
            vec![DecisionQuestion::Choice {
                id: "only-one".into(),
                question: "Pick one".into(),
                options: vec!["sole".into()],
            }],
        );
        assert!(matches!(
            bad_choice.validate(),
            Err(DecisionError::InvalidRequest(_))
        ));
    }

    #[test]
    fn parse_response_preserves_explicit_unknown_and_never_coerces_silence_to_false() {
        let request = sample_request();
        let raw = json!({
            "model": "jev-1.14.0",
            "decisions": [
                { "id": "q-investor", "probability": 0.92 },
                { "id": "q-urgency", "option": "this_week", "probability": 0.84 }
                // Note: `q-relevance` intentionally omitted by the endpoint!
            ]
        });
        let parsed = parse_response(&request, raw.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.model, "jev-1.14.0");
        assert_eq!(parsed.answers.len(), 3);
        assert_eq!(
            parsed.answer("q-investor").unwrap().outcome,
            AnswerOutcome::YesNo { probability: 0.92 }
        );
        assert_eq!(
            parsed.answer("q-urgency").unwrap().outcome,
            AnswerOutcome::Choice {
                option: "this_week".into(),
                probability: 0.84,
            }
        );
        // Omitted answer MUST be Unknown (never false / 0.0) and evaluate to Unsure.
        let omitted = parsed.answer("q-relevance").unwrap();
        assert_eq!(
            omitted.outcome,
            AnswerOutcome::Unknown {
                reason: UnknownReason::OmittedByEndpoint,
            }
        );
        assert!(omitted.is_unknown());
        assert_eq!(
            omitted.verdict(ThresholdTier::Tell.default_thresholds()),
            Verdict::Unsure
        );
    }

    #[test]
    fn parse_response_handles_explicit_unknown_invalid_probabilities_and_bad_options() {
        let request = sample_request();
        let raw = json!({
            "answers": [
                { "id": "q-investor", "unknown": true, "reason": "missing_facts" },
                { "id": "q-urgency", "option": "invented_option", "probability": 0.95 },
                { "id": "q-relevance", "score": 1.4, "probability": 0.9 }
            ]
        });
        let parsed = parse_response(&request, raw.to_string().as_bytes()).unwrap();
        assert_eq!(
            parsed.answer("q-investor").unwrap().outcome,
            AnswerOutcome::Unknown {
                reason: UnknownReason::MissingFacts,
            }
        );
        assert_eq!(
            parsed.answer("q-urgency").unwrap().outcome,
            AnswerOutcome::Unknown {
                reason: UnknownReason::UnrecognizedOption,
            }
        );
        assert_eq!(
            parsed.answer("q-relevance").unwrap().outcome,
            AnswerOutcome::Unknown {
                reason: UnknownReason::InvalidProbability,
            }
        );
        for answer in &parsed.answers {
            assert_eq!(
                answer.verdict(ThresholdTier::Tell.default_thresholds()),
                Verdict::Unsure
            );
        }
    }

    #[test]
    fn canonical_response_round_trips_cleanly() {
        let request = sample_request();
        let response = DecisionResponse {
            model: "kev-27b@v1.0".into(),
            answers: vec![
                DecisionAnswer {
                    id: "q-investor".into(),
                    outcome: AnswerOutcome::YesNo { probability: 0.78 },
                },
                DecisionAnswer {
                    id: "q-urgency".into(),
                    outcome: AnswerOutcome::Choice {
                        option: "now".into(),
                        probability: 0.91,
                    },
                },
                DecisionAnswer {
                    id: "q-relevance".into(),
                    outcome: AnswerOutcome::Score {
                        score: 0.85,
                        probability: Some(0.88),
                    },
                },
            ],
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        let parsed = parse_response(&request, &bytes).unwrap();
        assert_eq!(parsed, response);
    }

    #[test]
    fn risk_tiered_thresholds_separate_tell_from_act_alone() {
        let tell = ThresholdTier::Tell.default_thresholds();
        let act = ThresholdTier::ActAlone.default_thresholds();
        assert!(act.yes_min() > tell.yes_min());
        assert!(act.no_max() < tell.no_max());

        // Probability 0.78 clears Tell, but stays Unsure for ActAlone.
        assert_eq!(tell.evaluate_probability(0.78), Verdict::Yes);
        assert_eq!(act.evaluate_probability(0.78), Verdict::Unsure);

        // Probability 0.93 clears both Tell and ActAlone.
        assert_eq!(tell.evaluate_probability(0.93), Verdict::Yes);
        assert_eq!(act.evaluate_probability(0.93), Verdict::Yes);

        // Probability 0.22 is No for Tell, but still Unsure for ActAlone.
        assert_eq!(tell.evaluate_probability(0.22), Verdict::No);
        assert_eq!(act.evaluate_probability(0.22), Verdict::Unsure);

        // NaN or out-of-range is always Unsure, never No.
        assert_eq!(tell.evaluate_probability(f32::NAN), Verdict::Unsure);
        assert_eq!(tell.evaluate_probability(-0.1), Verdict::Unsure);
        assert_eq!(tell.evaluate_probability(1.2), Verdict::Unsure);

        // Inverted or invalid thresholds are rejected.
        assert!(ConfidenceThresholds::new(0.7, 0.7).is_err());
        assert!(ConfidenceThresholds::new(0.8, 0.2).is_err());
        assert!(ConfidenceThresholds::new(f32::NAN, 0.8).is_err());
    }

    #[test]
    fn rule_1_verdict_never_grants_external_permission() {
        let act_thresholds = ThresholdTier::ActAlone.default_thresholds();
        let strong_yes = AnswerOutcome::YesNo { probability: 0.99 };

        // Without independent ExternalAuthority::Granted, even 0.99 Yes must escalate.
        assert_eq!(
            screen_outcome(
                ThresholdTier::ActAlone,
                act_thresholds,
                &strong_yes,
                ExternalAuthority::Absent,
            ),
            ScreenEffect::Escalate
        );
        // Only with an independent grant may ActAlone proceed.
        assert_eq!(
            screen_outcome(
                ThresholdTier::ActAlone,
                act_thresholds,
                &strong_yes,
                ExternalAuthority::Granted,
            ),
            ScreenEffect::Proceed
        );
        // Tell screens surfacing to the person (not external side effects).
        assert_eq!(
            screen_outcome(
                ThresholdTier::Tell,
                ThresholdTier::Tell.default_thresholds(),
                &AnswerOutcome::YesNo { probability: 0.75 },
                ExternalAuthority::Absent,
            ),
            ScreenEffect::Proceed
        );
        // Unknown always escalates, even when authority is granted.
        assert_eq!(
            screen_outcome(
                ThresholdTier::ActAlone,
                act_thresholds,
                &AnswerOutcome::Unknown {
                    reason: UnknownReason::MissingFacts,
                },
                ExternalAuthority::Granted,
            ),
            ScreenEffect::Escalate
        );
    }

    #[test]
    fn endpoint_profiles_resolve_expected_paths_and_clamp_timeouts() {
        let system_one =
            DecisionEndpoint::new("https://api.typesafe.ai", EndpointProfile::SystemOne).unwrap();
        assert_eq!(
            system_one.resolved_url().unwrap().as_str(),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(system_one.timeout(), DEFAULT_DECISION_TIMEOUT);

        let openrouter = DecisionEndpoint::new(
            "https://openrouter.ai",
            EndpointProfile::OpenRouterDecisions,
        )
        .unwrap()
        .with_timeout(Duration::from_secs(60))
        .with_bearer_token("secret-token");
        assert_eq!(
            openrouter.resolved_url().unwrap().as_str(),
            "https://openrouter.ai/api/alpha/decisions"
        );
        assert_eq!(openrouter.timeout(), MAX_DECISION_TIMEOUT);
        assert!(!format!("{openrouter:?}").contains("secret-token"));

        assert!(DecisionEndpoint::new("ftp://example.com", EndpointProfile::SystemOne).is_err());
    }

    #[test]
    fn outage_errors_trigger_clean_fallback_while_client_errors_do_not() {
        assert!(
            DecisionError::TimedOut {
                timeout: DEFAULT_DECISION_TIMEOUT
            }
            .should_fallback()
        );
        assert!(
            DecisionError::Unavailable {
                message: "connection refused".into()
            }
            .should_fallback()
        );
        assert!(DecisionError::InvalidResponse("bad json".into()).should_fallback());
        assert!(!DecisionError::Rejected { status: 400 }.should_fallback());
        assert!(!DecisionError::InvalidRequest("empty".into()).should_fallback());
    }
}
