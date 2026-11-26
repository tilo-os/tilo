//! Next-word / completion prediction (W9b).
//!
//! An **offline** n-gram language model: unigram frequency (from the
//! [`Dictionary`]) + a bigram table (`apps/osk/data/bigrams-<lang>.txt`),
//! one pair per [`Language`]. It serves
//! two jobs the suggestion bar needs:
//!
//! * **next-word** — after a word commits, what word usually follows?
//!   (`prev_word = Some("good")`, `prefix = ""` → `morning`, `luck`, …).
//! * **completion** — as the user types, complete the in-progress word, biased
//!   by the preceding word when there is one
//!   (`prev_word = Some("how"), prefix = "ar"` → `are`).
//!
//! Both are exposed through one call, [`Predictor::predict_next`], which blends
//! bigram context, prefix match, and raw frequency.
//!
//! A larger neural model (candle / llama.cpp) is a documented future
//! enhancement; this n-gram baseline ships working and is the boundary everything
//! else is wired against.

use std::collections::HashMap;

use crate::dict::{Dictionary, Language, fold_char, starts_with_folded, weight_to_count};
use crate::num::f32_of_u32;
use crate::touch_model::NextKeyDistribution;

/// Bound the local adaptation cache. Prediction is helpful only while it is
/// cheap and private; it must never turn an unusually long typing session into
/// an unbounded in-memory profile.
pub const MAX_LEARNED_CONTEXTS: usize = 512;
pub const MAX_LEARNED_WORDS_PER_CONTEXT: usize = 32;

const BIGRAM_NEXT_KEY_SHARE: f64 = 0.5;

fn bigrams_table(language: Language) -> &'static str {
    match language {
        Language::English => include_str!("../data/bigrams-en.txt"),
        Language::French => include_str!("../data/bigrams-fr.txt"),
    }
}

#[derive(Clone, Debug)]
pub struct Predictor {
    dict: Dictionary,
    /// `prev_word` -> [(`next_word_index_into_dict_or_string`, weight)]. We keep
    /// the successor strings directly (they are all dictionary words) for a
    /// simple, allocation-light lookup.
    bigrams: HashMap<String, Vec<(String, u32)>>,
    max_bigram_weight: u32,
    /// On-device adaptation learned from words this user actually commits.
    /// It is deliberately process-local here; the OSK service owns persistence
    /// policy so private typing data never enters an app document.
    learned: HashMap<String, HashMap<String, u32>>,
}

impl Predictor {
    pub fn for_language(language: Language) -> Self {
        Self::from_data(Dictionary::for_language(language), bigrams_table(language))
    }

    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

    pub fn french() -> Self {
        Self::for_language(Language::French)
    }

    pub fn from_data(dict: Dictionary, bigram_data: &str) -> Self {
        let mut bigrams: HashMap<String, Vec<(String, u32)>> = HashMap::new();
        let mut max_bigram_weight = 1u32;
        for line in bigram_data.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split('\t');
            let (Some(w1), Some(w2), Some(w)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(weight) = w.trim().parse::<u32>() else {
                continue;
            };
            max_bigram_weight = max_bigram_weight.max(weight);
            bigrams
                .entry(w1.to_lowercase())
                .or_default()
                .push((w2.to_lowercase(), weight));
        }
        for v in bigrams.values_mut() {
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        Self {
            dict,
            bigrams,
            max_bigram_weight,
            learned: HashMap::new(),
        }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }

    pub fn split_elision<'a>(&self, word: &'a str) -> Option<(&'a str, &'a str)> {
        let at = word.find('\'')?.checked_add(1)?;
        let (elision, rest) = word.split_at(at);
        (self.dict.contains(elision)
            && (rest.is_empty() || self.dict.iter_prefix(word).next().is_none()))
        .then_some((elision, rest))
    }

    pub fn bigram_contexts(&self) -> usize {
        self.bigrams.len()
    }

    /// The transition a committed `word` after `previous` teaches, as the
    /// predictor keeps it: the context (empty at a start) and the word, both
    /// lower case. `None` for a word that is not all letters, apart from
    /// the apostrophes of an elision (`l'`) or of a word like `aujourd'hui`.
    pub fn transition(previous: Option<&str>, word: &str) -> Option<(String, String)> {
        let word = word.trim().to_lowercase();
        if !word.starts_with(char::is_alphabetic)
            || !word.chars().all(|c| c.is_alphabetic() || c == '\'')
        {
            return None;
        }
        Some((previous.map(str::to_lowercase).unwrap_or_default(), word))
    }

    pub fn learn_transition(
        &mut self,
        previous: Option<&str>,
        word: &str,
    ) -> Option<(String, String)> {
        let (context, word) = Self::transition(previous, word)?;
        let learned = (context.clone(), word.clone());
        if !self.learned.contains_key(&context) && self.learned.len() >= MAX_LEARNED_CONTEXTS {
            return None;
        }
        if self.learned.get(&context).is_some_and(|words| {
            !words.contains_key(&word) && words.len() >= MAX_LEARNED_WORDS_PER_CONTEXT
        }) {
            return None;
        }
        let count = self
            .learned
            .entry(context)
            .or_default()
            .entry(word)
            .or_default();
        *count = count.saturating_add(1);
        Some(learned)
    }

    pub fn adopt_learned(
        &mut self,
        learned: impl IntoIterator<Item = (String, std::collections::BTreeMap<String, u32>)>,
    ) {
        self.learned = learned
            .into_iter()
            .take(MAX_LEARNED_CONTEXTS)
            .map(|(context, words)| {
                let words = words
                    .into_iter()
                    .take(MAX_LEARNED_WORDS_PER_CONTEXT)
                    .collect();
                (context, words)
            })
            .collect();
    }

    pub fn has_learned(&self, word: &str) -> bool {
        let word = word.to_lowercase();
        self.learned.values().any(|words| words.contains_key(&word))
    }

    pub fn clear_learned(&mut self) {
        self.learned.clear();
    }

    /// Number of retained local adaptation contexts, for settings UIs and
    /// bounded-resource tests. This intentionally reveals no typed content.
    pub fn learned_context_count(&self) -> usize {
        self.learned.len()
    }

    pub fn next_key_distribution(
        &self,
        prev_word: Option<&str>,
        prefix: &str,
    ) -> NextKeyDistribution {
        let prefix = prefix.to_lowercase();
        if let Some((elision, rest)) = self.split_elision(&prefix) {
            return self.next_key_distribution(Some(elision), rest);
        }
        let lexicon = NextKeyDistribution::from_weights(self.dict.next_char_weights(&prefix));
        let Some(successors) = prev_word.and_then(|p| self.bigrams.get(&p.to_lowercase())) else {
            return lexicon;
        };
        let skip = prefix.chars().count();
        let context =
            NextKeyDistribution::from_weights(successors.iter().filter_map(|(w, weight)| {
                if !starts_with_folded(w, &prefix) {
                    return None;
                }
                let next = fold_char(w.chars().nth(skip)?);
                Some((next, weight_to_count(*weight)))
            }));
        lexicon.blend(&context, BIGRAM_NEXT_KEY_SHARE)
    }

    pub fn predict_next(&self, prev_word: Option<&str>, prefix: &str) -> Vec<(String, f32)> {
        self.predict_next_n(prev_word, prefix, 8)
    }

    pub fn predict_next_n(
        &self,
        prev_word: Option<&str>,
        prefix: &str,
        n: usize,
    ) -> Vec<(String, f32)> {
        if n == 0 {
            return Vec::new();
        }
        let prefix = prefix.to_lowercase();
        if let Some((elision, rest)) = self.split_elision(&prefix) {
            // "l'hom" completes "hom" after "l'" and keeps the elision.
            return self
                .predict_next_n(Some(elision), rest, n)
                .into_iter()
                .map(|(word, score)| (format!("{elision}{word}"), score))
                .collect();
        }
        let prev = prev_word.map(str::to_lowercase);

        let mut scores: HashMap<String, f32> = HashMap::new();

        // Personal history is the strongest signal after it has repeated, but
        // starts gently enough that one typo cannot permanently dominate.
        let learned_context = prev.as_deref().unwrap_or("");
        if let Some(words) = self.learned.get(learned_context) {
            for (word, count) in words {
                if !starts_with_folded(word, &prefix) {
                    continue;
                }
                let confidence = f32_of_u32(*count) / f32_of_u32(count.saturating_add(3));
                upsert_max(&mut scores, word, 0.85 + 0.15 * confidence);
            }
        }

        if let Some(succ) = prev.as_deref().and_then(|p| self.bigrams.get(p)) {
            for (w2, weight) in succ {
                if !starts_with_folded(w2, &prefix) {
                    continue;
                }
                let bigram_score = f32_of_u32(*weight) / f32_of_u32(self.max_bigram_weight);
                let freq = self.dict.frequency(w2);
                let s = 0.7 * bigram_score + 0.3 * freq;
                upsert_max(&mut scores, w2, s);
            }
        }

        // --- prefix completion from the dictionary ----------------------
        // When there is a prefix, fold in frequency-ranked completions so we
        // still suggest even without a bigram hit. When there is no prefix and
        // no bigram context, fall back to the most common words.
        if !prefix.is_empty() {
            for entry in self.dict.complete(&prefix, n.saturating_mul(4)) {
                let freq = self.dict.frequency(&entry.word);
                // Completion-only candidates score below bigram hits.
                let s = 0.45 * freq;
                upsert_max(&mut scores, &entry.word, s);
            }
        } else if prev.is_none() || !self.bigrams.contains_key(prev.as_deref().unwrap_or("")) {
            for entry in self.dict.words().iter().take(n.saturating_mul(2)) {
                let freq = self.dict.frequency(&entry.word);
                upsert_max(&mut scores, &entry.word, 0.3 * freq);
            }
        }

        let mut out: Vec<(String, f32)> = scores.into_iter().collect();
        out.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        out.truncate(n);
        out
    }
}

/// Keep the maximum score seen for a word (a word may be reached via several
/// signals; we don't want to double-count, we want its best evidence).
fn upsert_max(scores: &mut HashMap<String, f32>, word: &str, s: f32) {
    let e = scores.entry(word.to_string()).or_insert(0.0);
    if s > *e {
        *e = s;
    }
}
