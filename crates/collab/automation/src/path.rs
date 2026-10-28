//! Where a step sits in a shortcut.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The position of a step: indices through nested blocks, with the branch
/// taken and, at run time, the loop iteration. `0`, `2.then.1`,
/// `3.each[4].0`. A checker error names the step it is about with this; a
/// run derives each invocation id from it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StepPath(String);

impl StepPath {
    /// The top-level block itself (no step).
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// The path a report spells, read back: `2`, `1.each[0].0`, `2.then.1`.
    #[must_use]
    pub fn parse(spelled: &str) -> Self {
        Self(spelled.to_string())
    }

    /// The `index`th step of this block.
    #[must_use]
    pub fn child(&self, index: usize) -> Self {
        self.join(&index.to_string())
    }

    /// The block run when a condition holds.
    #[must_use]
    pub fn then_branch(&self) -> Self {
        self.join("then")
    }

    /// The block run when a condition does not hold.
    #[must_use]
    pub fn else_branch(&self) -> Self {
        self.join("else")
    }

    /// A loop body; `iteration` is known at run time only.
    #[must_use]
    pub fn each(&self, iteration: Option<usize>) -> Self {
        match iteration {
            Some(index) => self.join(&format!("each[{index}]")),
            None => self.join("each"),
        }
    }

    /// The output binding of the whole shortcut.
    #[must_use]
    pub fn output() -> Self {
        Self("output".into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this step sits in a block `ancestor` also sits in, below it.
    /// A loop's iterations of one body are all below the loop step, so they
    /// hold together. A step is never below itself.
    #[must_use]
    pub fn is_inside(&self, ancestor: &Self) -> bool {
        self.0
            .strip_prefix(ancestor.0.as_str())
            .is_some_and(|rest| rest.starts_with('.'))
    }

    fn join(&self, segment: &str) -> Self {
        if self.0.is_empty() {
            Self(segment.to_string())
        } else {
            Self(format!("{}.{segment}", self.0))
        }
    }
}

impl fmt::Display for StepPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("(shortcut)")
        } else {
            f.write_str(&self.0)
        }
    }
}
