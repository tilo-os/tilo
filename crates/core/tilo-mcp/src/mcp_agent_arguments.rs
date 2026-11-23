//! What each agent tool takes, as the page or the program sends it.

use super::*;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LsArgs {
    pub(super) path: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) limit: Option<usize>,
    pub(super) cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadArgs {
    pub(super) path: String,
    pub(super) cursor: Option<String>,
    pub(super) max_chars: Option<usize>,
    /// A record read (`record:<app>:<id>`) joins the record's chain of parts.
    #[serde(default)]
    pub(super) parts: bool,
    /// A document read waits for the next write past this revision.
    pub(super) after: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(super) struct DoStep {
    pub(super) path: String,
    #[serde(default)]
    pub(super) args: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DoArgs {
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) args: serde_json::Map<String, Value>,
    pub(super) calls: Option<Vec<DoStep>>,
    pub(super) wait_ms: Option<u64>,
    pub(super) invocation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FindArgs {
    pub(super) query: String,
    pub(super) app: Option<String>,
    pub(super) limit: Option<usize>,
}
