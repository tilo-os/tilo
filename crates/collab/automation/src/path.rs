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
        self.numbered_loop("each", iteration)
    }

    /// A counted repeat's body; `iteration` is known at run time only.
    #[must_use]
    pub fn repeat(&self, iteration: Option<usize>) -> Self {
        self.numbered_loop("repeat", iteration)
    }

    /// A while loop's body; `iteration` is known at run time only.
    #[must_use]
    pub fn while_loop(&self, iteration: Option<usize>) -> Self {
        self.numbered_loop("while", iteration)
    }

    fn numbered_loop(&self, name: &str, iteration: Option<usize>) -> Self {
        match iteration {
            Some(index) => self.join(&format!("{name}[{index}]")),
            None => self.join(name),
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
