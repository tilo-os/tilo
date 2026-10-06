//! What a provider is asked: the typed text, the facet filters the platform
//! parsed out of it or the user picked as chips, and the scope it is narrowed
//! to.

/// One value a [`Facet`] can take: `rust` with the label "Rust".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FacetValue {
    pub value: String,
    pub label: String,
}

impl FacetValue {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A dimension a provider can filter by: a snippet's language, a store item's
/// category, a message's sender. The palette shows its values as filter chips
/// beside the kind chips, and typed `key:value` tokens for a declared `key`
/// become the same structured filter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Facet {
    /// The token key typed before the colon (`language` in `language:rust`).
    pub key: String,
    /// What the chip group is called ("Language").
    pub label: String,
    /// The values offered as chips. A typed value need not be listed.
    pub values: Vec<FacetValue>,
}

impl Facet {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            values: Vec::new(),
        }
    }

    #[must_use]
    pub fn value(mut self, value: impl Into<String>, label: impl Into<String>) -> Self {
        self.values.push(FacetValue::new(value, label));
        self
    }

    /// The label of `value`, or the value itself when it is not listed.
    #[must_use]
    pub fn value_label<'a>(&'a self, value: &'a str) -> &'a str {
        self.values
            .iter()
            .find(|candidate| candidate.value.eq_ignore_ascii_case(value))
            .map_or(value, |candidate| candidate.label.as_str())
    }
}

/// One applied facet filter: `key` equals `value`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FacetFilter {
    pub key: String,
    pub value: String,
}

impl FacetFilter {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    /// The token this filter reads as when typed.
    #[must_use]
    pub fn token(&self) -> String {
        format!("{}:{}", self.key, self.value)
    }
}

/// One query as a provider receives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    text: String,
    filters: Vec<FacetFilter>,
    scope: Option<String>,
}

impl SearchQuery {
    /// A plain text query with no filters and no scope.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            filters: Vec::new(),
            scope: None,
        }
    }

    /// Split typed `input` into free text and `key:value` filters for the
    /// declared `facets`. A token whose key no facet declares stays text, so
    /// `http://example.org` or `12:30` are searched for as typed. Keys match
    /// case-insensitively; a quoted value (`tag:"two words"`) may hold spaces.
    #[must_use]
    pub fn parse(input: &str, facets: &[Facet]) -> Self {
        let mut text = Vec::new();
        let mut filters = Vec::new();
        for token in tokens(input) {
            let parsed = token.split_once(':').and_then(|(key, value)| {
                let facet = facets
                    .iter()
                    .find(|facet| facet.key.eq_ignore_ascii_case(key))?;
                let value = unquote(value);
                (!value.is_empty()).then(|| FacetFilter::new(facet.key.clone(), value))
            });
            match parsed {
                Some(filter) if !filters.contains(&filter) => filters.push(filter),
                Some(_) => {}
                None => text.push(token),
            }
        }
        Self {
            text: text.join(" "),
            filters,
            scope: None,
        }
    }

    /// Add a filter picked as a chip.
    #[must_use]
    pub fn with_filter(mut self, filter: FacetFilter) -> Self {
        if !self.filters.contains(&filter) {
            self.filters.push(filter);
        }
        self
    }

    /// Narrow the query to the resource anchored at `anchor` (see
    /// [`ScopeDecl`](super::ScopeDecl)).
    #[must_use]
    pub fn within(mut self, anchor: impl Into<String>) -> Self {
        self.scope = Some(anchor.into());
        self
    }

    /// The free text, with every recognised filter token removed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every applied filter, in the order typed or picked.
    #[must_use]
    pub fn filters(&self) -> &[FacetFilter] {
        &self.filters
    }

    /// The values applied for `key`. Values of one key combine as "any of".
    pub fn filter_values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.filters
            .iter()
            .filter(move |filter| filter.key.eq_ignore_ascii_case(key))
            .map(|filter| filter.value.as_str())
    }

    /// Whether a hit with `values` for `key` passes this query's filters on
    /// that key: always when no value of `key` is applied.
    #[must_use]
    pub fn admits(&self, key: &str, values: &[&str]) -> bool {
        let mut wanted = self.filter_values(key).peekable();
        wanted.peek().is_none()
            || wanted.any(|want| values.iter().any(|have| have.eq_ignore_ascii_case(want)))
    }

    /// The anchor of the resource the query is narrowed to.
    #[must_use]
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    /// Whether the hit anchored at `anchor` lies inside the query's scope:
    /// always when the query has none.
    #[must_use]
    pub fn in_scope(&self, anchor: &str) -> bool {
        self.scope
            .as_deref()
            .is_none_or(|scope| super::request::scope_contains(scope, anchor))
    }
}

impl From<&str> for SearchQuery {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Whitespace-separated tokens; a double-quoted run stays one token.
fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in input.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .map_or(value, |rest| rest.strip_suffix('"').unwrap_or(rest))
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facets() -> Vec<Facet> {
        vec![
            Facet::new("language", "Language")
                .value("rust", "Rust")
                .value("go", "Go"),
            Facet::new("tag", "Tag"),
        ]
    }

    #[test]
    fn declared_tokens_become_filters_and_the_rest_stays_text() {
        let query = SearchQuery::parse(
            "parse Language:rust tag:\"two words\" http://x 12:30",
            &facets(),
        );
        assert_eq!(query.text(), "parse http://x 12:30");
        assert_eq!(
            query.filters(),
            [
                FacetFilter::new("language", "rust"),
                FacetFilter::new("tag", "two words")
            ]
        );
        assert!(query.admits("language", &["Rust"]));
        assert!(!query.admits("language", &["go"]));
        assert!(
            query.admits("source", &["anything"]),
            "an unfiltered key admits all"
        );
    }

    #[test]
    fn an_empty_or_repeated_token_adds_nothing() {
        let query = SearchQuery::parse("language: language:go language:go", &facets());
        assert_eq!(query.text(), "language:");
        assert_eq!(query.filters(), [FacetFilter::new("language", "go")]);
        let picked = query.with_filter(FacetFilter::new("language", "go"));
        assert_eq!(picked.filters().len(), 1);
    }

    #[test]
    fn a_scope_narrows_by_resource_path() {
        let query = SearchQuery::new("x").within("folder:inbox");
        assert!(query.in_scope("folder:inbox/msg:1"));
        assert!(!query.in_scope("folder:sent/msg:1"));
        assert!(SearchQuery::new("x").in_scope("anything"));
    }

    #[test]
    fn facet_value_labels_fall_back_to_the_value() {
        let facets = facets();
        assert_eq!(facets[0].value_label("RUST"), "Rust");
        assert_eq!(facets[0].value_label("zig"), "zig");
    }
}
