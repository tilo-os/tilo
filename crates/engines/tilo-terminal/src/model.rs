//! What a run's parsers find in its output: a diagnostic, a location, a link, a test, a
//! changed file.

use serde::{Deserialize, Serialize};

/// What one thing a parser found is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    #[default]
    Error,
    Warning,
    Location,
    Url,
    TestPassed,
    TestFailed,
    TestIgnored,
    Change,
}

/// One thing a run's parser found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// What it is.
    pub kind: ItemKind,
    /// What the output says about it.
    pub text: String,
    /// The file it is in, or the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// A diagnostic's code, a test's name, a change's status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
