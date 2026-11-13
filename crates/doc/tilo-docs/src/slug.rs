//! Heading ids, as GitHub (and `developer_docs.py`, which checks links to
//! them) derive them from a heading's text.

use std::collections::HashMap;

/// Hands out each heading's id: its slug, then `slug-1`, `slug-2` … for later
/// headings with the same text.
#[derive(Debug, Default)]
pub struct Slugger {
    seen: HashMap<String, usize>,
}

impl Slugger {
    pub fn id(&mut self, text: &str) -> String {
        let slug = slugify(text);
        let count = self.seen.entry(slug.clone()).or_insert(0);
        let id = if *count == 0 {
            slug
        } else {
            format!("{slug}-{count}")
        };
        *count = count.saturating_add(1);
        id
    }
}

#[must_use]
pub fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_githubs_rule_and_count_repeats() {
        let mut slugger = Slugger::default();
        assert_eq!(slugger.id("Keys, scopes & expiry"), "keys-scopes--expiry");
        assert_eq!(slugger.id("`tilo_do`"), "tilo_do");
        assert_eq!(slugger.id("Notes"), "notes");
        assert_eq!(slugger.id("Notes"), "notes-1");
    }
}
