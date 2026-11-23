//! The fixture's search: what the desktop's one search answers in these tests.
use super::*;

impl tilo_actions::remote::ResourceSearch for Notes {
    /// A stand-in for the desktop's one search: the rows the app publishes
    /// whose names hold the query's words, best first, each as the resource
    /// it is.
    fn find(
        &self,
        _caller: &Caller,
        needle: &str,
    ) -> Pending<Result<Vec<tilo_actions::remote::Found>, ActionCallError>> {
        let unreachable = self.unreachable.lock().unwrap().clone();
        if let Some(error) = unreachable {
            return Pending::ready(Err(error));
        }
        let words = page::needle_words(needle);
        if words.is_empty() {
            return Pending::ready(Ok(Vec::new()));
        }
        let mut found = Vec::new();
        let rows = self.rows("1");
        // A row about what another resource holds is about that resource:
        // the live index lists the notebook's rows, and never both a note
        // and the body inside it.
        // A row about what another resource holds (a body published beside
        // its note) is about that resource: the live index lists the
        // notebook's rows, and never both a note and the body inside it. An
        // action is about the resource it acts on, which is the note itself.
        let owned = |row: &CatalogueRow| {
            row.action.is_none()
                && row.resource.kind == "tilo.rich-text"
                && rows.iter().any(|other| {
                    other.action.is_some()
                        && other.resource.id == row.resource.id
                        && other.resource.kind == row.resource.kind
                })
                && rows.iter().any(|owner| {
                    owner.has_children
                        && owner.resource.id != row.resource.id
                        && row
                            .resource
                            .id
                            .contains(owner.resource.id.rsplit(':').next().unwrap_or_default())
                })
        };
        for row in rows.iter().filter(|row| !owned(row)) {
            // Every word, as the one search reads a query: not the rows matching the most of it.
            let score = page::word_score(row, &words);
            if score < words.len() {
                continue;
            }
            found.push((
                score,
                tilo_actions::remote::Found {
                    resource: row.resource.id.clone(),
                    kind: row.resource.kind.clone(),
                    label: row.resource.label.clone(),
                    open: Some(
                        row.action.as_ref().map_or_else(
                            || "open".to_owned(),
                            |action| action.address.action.clone(),
                        ),
                    ),
                    arguments: serde_json::Map::new(),
                    app: Some(APP.to_owned()),
                    provider: None,
                    when: None,
                },
            ));
        }
        found.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        Pending::ready(Ok(found.into_iter().map(|(_, found)| found).collect()))
    }
}
