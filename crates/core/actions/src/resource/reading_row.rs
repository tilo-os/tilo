//! One list item of a reading, as a person finds it in search.

use std::collections::BTreeMap;

/// One item a reading lists among its values — a container, a drive and its partitions, a
/// printer and its jobs, a process, a journal record — as a person finds it in search: what it
/// is called, what says what it is, and how choosing it opens it.
///
/// A reading publishes these for every list its resource keeps (`Service::search_rows`); the
/// system's search lists each as a row of its own, so a device reading's items are found the
/// way an app's are, without the app that shows them having to run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadingRow {
    /// The item's stable id in its reading: a container's name, a drive's name in `/dev`, a
    /// journal record's moment.
    pub id: String,
    /// What the row says.
    pub title: String,
    /// The line under it (a publisher, a state, a unit).
    pub subtitle: Option<String>,
    /// The group it is listed under ("Containers", "Printers").
    pub group: Option<String>,
    /// A named icon (a lucide kebab name).
    pub icon: Option<String>,
    /// Where it is (a container's image, a drive's mount point): further lines, not matched.
    pub facts: Vec<String>,
    /// More text it is found by: a publisher, a command, a record's unit.
    pub fields: Vec<String>,
    /// The action of its resource that opens it on the item, when the reading offers one: a
    /// program's `launch`, a partition's `mount`.
    pub action: Option<String>,
    /// The values that action takes to name the item, by parameter name.
    pub arguments: BTreeMap<String, serde_json::Value>,
    /// The app whose window shows the item, opened when the reading offers no action on it.
    pub app: Option<String>,
}

impl ReadingRow {
    /// The item `id`, called `title`.
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// The line under it.
    #[must_use]
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        let subtitle = subtitle.into();
        self.subtitle = (!subtitle.trim().is_empty()).then(|| subtitle.trim().to_owned());
        self
    }

    /// The group it is listed under.
    #[must_use]
    pub fn grouped(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// A named icon.
    #[must_use]
    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Where it is.
    #[must_use]
    pub fn fact(mut self, fact: impl Into<String>) -> Self {
        let fact = fact.into();
        if !fact.trim().is_empty() {
            self.facts.push(fact);
        }
        self
    }

    /// More text it is found by.
    #[must_use]
    pub fn field(mut self, field: impl Into<String>) -> Self {
        let field = field.into();
        if !field.trim().is_empty() {
            self.fields.push(field);
        }
        self
    }

    /// The action of its resource that opens it, given the item by `arguments`.
    #[must_use]
    pub fn opened_by<K, V>(
        mut self,
        action: impl Into<String>,
        arguments: impl IntoIterator<Item = (K, V)>,
    ) -> Self
    where
        K: Into<String>,
        V: Into<serde_json::Value>,
    {
        self.action = Some(action.into());
        self.arguments
            .extend(arguments.into_iter().map(|(name, value)| (name.into(), value.into())));
        self
    }

    /// The app whose window shows it.
    #[must_use]
    pub fn shown_by(mut self, app: impl Into<String>) -> Self {
        self.app = Some(app.into());
        self
    }
}
