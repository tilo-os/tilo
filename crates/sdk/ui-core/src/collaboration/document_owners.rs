//! App-owned document generations. Route registration conveys no owner custody.

/// Crate-internal App-to-transport metadata, never an authorization credential.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentOwnerScope {
    pub(crate) primary: Option<(String, uuid::Uuid, u64)>,
    pub(crate) records: Vec<(String, Vec<uuid::Uuid>)>,
}
impl DocumentOwnerScope {
    pub(crate) fn normalize(&mut self) {
        for (_, owners) in &mut self.records {
            owners.sort();
            owners.dedup();
        }
        self.records.sort_by(|a, b| a.0.cmp(&b.0));
    }
    pub(crate) fn routes(&self) -> Vec<String> {
        let mut routes: Vec<_> = self
            .primary
            .iter()
            .map(|(key, _, _)| key.clone())
            .chain(self.records.iter().map(|(key, _)| key.clone()))
            .collect();
        routes.sort();
        routes.dedup();
        routes
    }
    #[cfg(test)]
    pub(crate) fn for_keys(keys: Vec<String>) -> Self {
        Self {
            primary: None,
            records: keys
                .into_iter()
                .map(|key| (key, vec![uuid::Uuid::nil()]))
                .collect(),
        }
    }
}

/// Acknowledges withdrawal of this exact generation's delivery custody.
/// It does not claim process exit, durable publication, or action completion.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentOwnerRetirementReceipt {
    pub(crate) scope: DocumentOwnerScope,
}
impl DocumentOwnerRetirementReceipt {
    /// The exact metadata whose delivery custody was permanently withdrawn.
    pub fn retired_scope(&self) -> &DocumentOwnerScope {
        &self.scope
    }
}
