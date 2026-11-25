//! Where a search looks: a part of what an app shows, by its anchor.

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ScopeDecl {
    pub anchor: String,
    pub label: String,
}

impl ScopeDecl {
    pub fn new(anchor: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            anchor: anchor.into(),
            label: label.into(),
        }
    }

    /// Whether the hit anchored at `anchor` lies inside this scope.
    #[must_use]
    pub fn contains(&self, anchor: &str) -> bool {
        scope_contains(&self.anchor, anchor)
    }
}

/// Whether `anchor` is `scope` or lies beneath it in the anchor path.
#[must_use]
pub fn scope_contains(scope: &str, anchor: &str) -> bool {
    anchor
        .strip_prefix(scope)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}
