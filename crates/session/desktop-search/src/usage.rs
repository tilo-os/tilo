//! What the person chose from search before, kept by whoever holds the
//! desktop (the session, a web page) as part of what it keeps, never in a
//! file of the search's own.
//!
//! Two records, both written when a row is chosen:
//!
//! - which rows were chosen, how often and how recently
//!   ([`ui::search::Frecency`]). It orders equally good matches and leads
//!   short queries, where launchers live ("t" is the Terminal opened every
//!   day);
//! - which row was chosen for which typed text: what "te" was for last time
//!   is what it is for next time.

use ui::search::Frecency;

use crate::model::Hit;

/// Most characters of a typed query a choice is remembered for: past that
/// the text names the thing on its own.
const CHOICE_PREFIX_SCALARS: usize = 6;
/// What a row chosen always and lately adds to its strength at the merge:
/// enough to order equal matches and lead a short query, less than one
/// match tier.
const USED_STRENGTH: f32 = 0.3;
/// What a row chosen for the same typed text adds.
const CHOSEN_FOR_TEXT_STRENGTH: f32 = 0.6;

#[derive(Clone, Debug, Default)]
pub struct Usage {
    chosen: Frecency,
    /// Typed text → chosen row, keyed `<text>\u{1f}<provider>:<id>`.
    for_text: Frecency,
}

fn key(hit: &Hit) -> String {
    format!("{}:{}", hit.provider, hit.id)
}

/// The texts a choice made after typing `query` answers: each start of it
/// up to [`CHOICE_PREFIX_SCALARS`], folded. Choosing Terminal after "term"
/// makes it the answer to "t", "te", "ter" and "term".
fn texts(query: &str) -> Vec<String> {
    let folded = ui::search::fold_search_text(query.trim());
    let scalars: Vec<char> = folded.chars().collect();
    (1..=scalars.len().min(CHOICE_PREFIX_SCALARS))
        .map(|end| scalars.iter().take(end).collect())
        .collect()
}

impl Usage {
    /// The record as [`Self::save`] wrote it.
    #[must_use]
    pub fn load(chosen: &str, for_text: &str) -> Self {
        Self {
            chosen: Frecency::parse(chosen),
            for_text: Frecency::parse(for_text),
        }
    }

    /// The two records' text, to keep: which rows were chosen, and for
    /// what.
    #[must_use]
    pub fn save(&self) -> (String, String) {
        (self.chosen.serialize(), self.for_text.serialize())
    }

    /// `hit` was chosen at `now` after typing `query`.
    pub fn record(&mut self, query: &str, hit: &Hit, now: u64) {
        let key = key(hit);
        self.chosen.record(&key, now);
        for text in texts(query) {
            self.for_text.record(&format!("{text}\u{1f}{key}"), now);
        }
    }

    /// What `hit`'s use adds to its strength for `query` at `now`.
    #[must_use]
    pub fn strength(&self, query: &str, hit: &Hit, now: u64) -> f32 {
        let key = key(hit);
        let used = self.chosen.share(&key, now);
        let for_text = texts(query).pop().map_or(0.0, |text| {
            self.for_text.share(&format!("{text}\u{1f}{key}"), now)
        });
        used * USED_STRENGTH + for_text * CHOSEN_FOR_TEXT_STRENGTH
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;

    fn hit(id: &str) -> Hit {
        Hit {
            faces: Vec::new(),
            outcome: None,
            kind: ui::search::system::ResourceKind::Application,
            provider: "applications".into(),
            id: id.into(),
            title: id.into(),
            subtitle: None,
            facts: Vec::new(),
            score: 1,
            icon: None,
            target: Target::Launch { app: id.into() },
        }
    }

    #[test]
    fn a_choice_answers_the_text_typed_for_it_and_its_starts_and_is_kept() {
        let mut usage = Usage::default();
        usage.record("Term", &hit("terminal"), 1_000);
        let terminal = hit("terminal");
        assert!(usage.strength("t", &terminal, 1_000) > usage.strength("x", &terminal, 1_000));
        assert!(usage.strength("ter", &terminal, 1_000) > 0.0);
        assert!(usage.strength("t", &hit("tube"), 1_000) <= 0.0);
        let (chosen, for_text) = usage.save();
        let kept = Usage::load(&chosen, &for_text);
        assert!(
            (kept.strength("te", &terminal, 1_000) - usage.strength("te", &terminal, 1_000)).abs()
                < f32::EPSILON
        );
    }
}
