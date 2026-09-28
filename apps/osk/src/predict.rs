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
//! enhancement; this n-gram baseline ships working and is the seam everything
//! else is wired against.

use std::collections::HashMap;

use crate::dict::{Dictionary, Language, fold_char, starts_with_folded, weight_to_count};
use crate::touch_model::NextKeyDistribution;

/// Bound the local adaptation cache. Prediction is helpful only while it is
/// cheap and private; it must never turn an unusually long typing session into
/// an unbounded in-memory profile.
const MAX_LEARNED_CONTEXTS: usize = 512;
const MAX_LEARNED_WORDS_PER_CONTEXT: usize = 32;

/// Weight of the previous word's bigram successors against the plain lexicon
/// when estimating the next letter.
const BIGRAM_NEXT_KEY_SHARE: f64 = 0.5;

/// The embedded bigram table of `language`: `w1<TAB>w2<TAB>weight` per line,
/// ~40k common pairs.
fn bigrams_table(language: Language) -> &'static str {
    match language {
        Language::English => include_str!("../data/bigrams-en.txt"),
        Language::French => include_str!("../data/bigrams-fr.txt"),
    }
}

/// Offline n-gram next-word predictor.
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
    /// Build the predictor over the embedded dictionary + bigrams of
    /// `language`.
    pub fn for_language(language: Language) -> Self {
        Self::from_data(Dictionary::for_language(language), bigrams_table(language))
    }

    /// Build the predictor over the embedded English dictionary + bigrams.
    pub fn english() -> Self {
        Self::for_language(Language::English)
    }

    /// Build the predictor over the embedded French dictionary + bigrams.
    pub fn french() -> Self {
        Self::for_language(Language::French)
    }

    /// Build from a dictionary and a `w1<TAB>w2<TAB>weight` bigram table.
    /// Exposed for tests / custom corpora.
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
        // Sort each successor list by weight descending for fast top-N.
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

    /// Number of distinct preceding-words with bigram successors.
    pub fn bigram_contexts(&self) -> usize {
        self.bigrams.len()
    }

    /// Teach the predictor one committed transition. Repetition increases its
    /// rank with a saturating counter, entirely offline.
    pub fn learn_transition(&mut self, previous: Option<&str>, word: &str) {
        let word = word.trim().to_lowercase();
        if word.is_empty() || !word.chars().all(char::is_alphabetic) {
            return;
        }
        let context = previous.map(str::to_lowercase).unwrap_or_default();
        if !self.learned.contains_key(&context) && self.learned.len() >= MAX_LEARNED_CONTEXTS {
            return;
        }
        if self.learned.get(&context).is_some_and(|words| {
            !words.contains_key(&word) && words.len() >= MAX_LEARNED_WORDS_PER_CONTEXT
        }) {
            return;
        }
        let count = self
            .learned
            .entry(context)
            .or_default()
            .entry(word)
            .or_default();
        *count = count.saturating_add(1);
    }

    /// Whether this user has committed `word` before (in any context).
    pub fn has_learned(&self, word: &str) -> bool {
        let word = word.to_lowercase();
        self.learned.values().any(|words| words.contains_key(&word))
    }

    /// Forget all on-device adaptation immediately. The embedded dictionary
    /// and bundled bigram model remain available, but no typed words stay in
    /// this process after a privacy reset.
    pub fn clear_learned(&mut self) {
        self.learned.clear();
    }

    /// Number of retained local adaptation contexts, for settings UIs and
    /// bounded-resource tests. This intentionally reveals no typed content.
    pub fn learned_context_count(&self) -> usize {
        self.learned.len()
    }

    /// How likely each letter is to be typed next, given the in-progress
    /// `prefix` and the previous committed word. The lexicon (every word
    /// extending `prefix`, weighted by frequency) is blended with the bigram
    /// successors of `prev_word` that extend `prefix`, when there are any.
    pub fn next_key_distribution(
        &self,
        prev_word: Option<&str>,
        prefix: &str,
    ) -> NextKeyDistribution {
        let prefix = prefix.to_lowercase();
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

    /// Predict the next/completion words, ranked best-first, as
    /// `(word, score)` with `score` in roughly `0.0..=1.0`.
    ///
    /// * `prev_word` — the word just committed, if any (lower-cased internally).
    /// * `prefix` — the in-progress word fragment (may be empty).
    ///
    /// Scoring blends three signals:
    /// * **bigram** — P(word | `prev_word`) from the bigram table (dominant when
    ///   present),
    /// * **prefix** — does the candidate start with `prefix` (required when
    ///   `prefix` is non-empty),
    /// * **frequency** — the unigram prior.
    pub fn predict_next(&self, prev_word: Option<&str>, prefix: &str) -> Vec<(String, f32)> {
        self.predict_next_n(prev_word, prefix, 8)
    }

    /// As [`predict_next`](Self::predict_next) but with an explicit cap.
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
        let prev = prev_word.map(str::to_lowercase);

        // Accumulate the best score per candidate word.
        let mut scores: HashMap<String, f32> = HashMap::new();

        // Personal history is the strongest signal after it has repeated, but
        // starts gently enough that one typo cannot permanently dominate.
        let learned_context = prev.as_deref().unwrap_or("");
        if let Some(words) = self.learned.get(learned_context) {
            for (word, count) in words {
                if !starts_with_folded(word, &prefix) {
                    continue;
                }
                let confidence = *count as f32 / count.saturating_add(3) as f32;
                upsert_max(&mut scores, word, 0.85 + 0.15 * confidence);
            }
        }

        // --- bigram successors of prev_word -----------------------------
        if let Some(succ) = prev.as_deref().and_then(|p| self.bigrams.get(p)) {
            for (w2, weight) in succ {
                if !starts_with_folded(w2, &prefix) {
                    continue;
                }
                let bigram_score = *weight as f32 / self.max_bigram_weight as f32;
                // Bigram is the strongest signal — weight it heavily.
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
            for entry in self.dict.complete(&prefix, n * 4) {
                let freq = self.dict.frequency(&entry.word);
                // Completion-only candidates score below bigram hits.
                let s = 0.45 * freq;
                upsert_max(&mut scores, &entry.word, s);
            }
        } else if prev.is_none() || !self.bigrams.contains_key(prev.as_deref().unwrap_or("")) {
            // Cold start: no context, no prefix → most common words.
            for entry in self.dict.words().iter().take(n * 2) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> Predictor {
        let dict = Dictionary::from_str(
            "the\t100\nmorning\t60\nmore\t70\nmood\t40\ngood\t80\nluck\t50\nare\t90\nart\t30\n",
        );
        let bigrams = "good\tmorning\t900\ngood\tluck\t700\nhow\tare\t950\n";
        Predictor::from_data(dict, bigrams)
    }

    #[test]
    fn embedded_bigrams_predict_next_words_sensibly() {
        eprintln!("phase embedded_predictor_loads_bigrams");
        {
            let p = Predictor::english();
            assert!(
                p.bigram_contexts() > 1000,
                "want many bigram contexts, got {}",
                p.bigram_contexts()
            );
        }

        eprintln!("phase next_word_uses_bigram_context");
        {
            let p = small();
            let preds = p.predict_next(Some("good"), "");
            let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
            // "morning" and "luck" are good's bigram successors and should lead.
            assert_eq!(words[0], "morning", "got {words:?}");
            assert!(words.contains(&"luck"));
        }

        eprintln!("phase embedded_real_bigram_predicts_sensibly");
        {
            let p = Predictor::english();
            // "of" -> "the" is the single most common English bigram.
            let preds = p.predict_next(Some("of"), "");
            assert!(!preds.is_empty());
            let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
            assert_eq!(words[0], "the", "expected 'the' after 'of', got {words:?}");
        }

        eprintln!("phase embedded_tables_are_consistent");
        {
            let p = Predictor::english();
            let dict = p.dictionary();
            assert!(
                dict.words()
                    .iter()
                    .all(|e| e.word.bytes().all(|b| b.is_ascii_lowercase())),
                "the dictionary holds lower-case a-z words only"
            );
            for (w1, successors) in &p.bigrams {
                assert!(dict.contains(w1), "bigram context {w1:?} not in dictionary");
                for (w2, _) in successors {
                    assert!(dict.contains(w2), "bigram {w1:?} {w2:?} not in dictionary");
                }
            }
            let after_thank = p.predict_next(Some("thank"), "");
            assert_eq!(
                after_thank.first().map(|(w, _)| w.as_str()),
                Some("you"),
                "got {after_thank:?}"
            );
        }
    }

    #[test]
    fn prefix_completion_filters_context_and_falls_back_to_common_words() {
        eprintln!("phase prefix_completion_respects_prefix");
        {
            let p = small();
            let preds = p.predict_next(None, "mo");
            let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
            assert!(words.iter().all(|w| w.starts_with("mo")), "got {words:?}");
            // Frequency order among "mo*": more(70) > morning(60) > mood(40).
            assert_eq!(words[0], "more", "got {words:?}");
        }

        eprintln!("phase bigram_plus_prefix_filters_to_matching_successor");
        {
            let p = small();
            // After "good", typing "m" should surface "morning" (the matching
            // bigram successor), not "luck".
            let preds = p.predict_next(Some("good"), "m");
            let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
            assert_eq!(words[0], "morning", "got {words:?}");
            assert!(!words.contains(&"luck"), "luck doesn't start with m");
        }

        eprintln!("phase cold_start_returns_common_words");
        {
            let p = small();
            let preds = p.predict_next(None, "");
            assert!(!preds.is_empty());
            // Most common word in the toy dict is "the".
            assert_eq!(preds[0].0, "the");
        }

        eprintln!("phase unknown_context_falls_back_to_prefix_or_common");
        {
            let p = small();
            // No bigram for "xyzzy"; with a prefix we still complete.
            let preds = p.predict_next(Some("xyzzy"), "ar");
            let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
            assert!(words.iter().all(|w| w.starts_with("ar")), "got {words:?}");
            assert!(words.contains(&"are"));
        }

        eprintln!("phase next_key_distribution_blends_lexicon_with_bigram_context");
        {
            let p = small();
            // Lexicon alone: "mo" continues as "more" (r), "morning" (r), "mood" (o).
            let plain = p.next_key_distribution(None, "mo");
            assert!(plain.prob('r') > plain.prob('o'));
            // After "good" only "morning" follows, reinforcing 'r' further.
            let ctx = p.next_key_distribution(Some("good"), "mo");
            assert!(ctx.prob('r') > plain.prob('r'));
            // A prefix nothing extends carries no context at all.
            assert!(p.next_key_distribution(None, "zzq").is_uniform());
        }
    }

    #[test]
    fn embedded_models_complete_everyday_english_and_french() {
        eprintln!("phase english_completes_everyday_and_tech_words");
        {
            let p = Predictor::english();
            for (prefix, word) in [("wish", "wishlist"), ("scree", "screenshot")] {
                let preds = p.predict_next(None, prefix);
                let words: Vec<&str> = preds.iter().map(|(w, _)| w.as_str()).collect();
                assert!(
                    words.contains(&word),
                    "{prefix:?} should offer {word:?}: {words:?}"
                );
            }
            for word in ["github", "wifi", "podcast", "login"] {
                assert!(p.dictionary().contains(word), "{word} missing");
            }
        }

        eprintln!("phase french_predicts_sensibly");
        {
            let p = Predictor::french();
            let after_je: Vec<String> = p
                .predict_next(Some("je"), "")
                .into_iter()
                .map(|(w, _)| w)
                .collect();
            assert!(after_je.iter().any(|w| w == "suis"), "je -> {after_je:?}");
            let bonj = p.predict_next(None, "bonj");
            assert_eq!(
                bonj.first().map(|(w, _)| w.as_str()),
                Some("bonjour"),
                "{bonj:?}"
            );
            // No accented keys: the unaccented spelling reaches the word.
            let tres = p.predict_next(None, "tres");
            assert_eq!(
                tres.first().map(|(w, _)| w.as_str()),
                Some("très"),
                "{tres:?}"
            );
            let dict = p.dictionary();
            for (w1, successors) in &p.bigrams {
                assert!(dict.contains(w1), "bigram context {w1:?} not in dictionary");
                for (w2, _) in successors {
                    assert!(dict.contains(w2), "bigram {w1:?} {w2:?} not in dictionary");
                }
            }
        }
    }

    #[test]
    fn local_adaptation_learns_commits_within_bounds_and_clears() {
        eprintln!("phase committed_transitions_adapt_predictions_locally");
        {
            let mut p = small();
            for _ in 0..4 {
                p.learn_transition(Some("good"), "mood");
            }
            let predictions = p.predict_next(Some("good"), "");
            assert_eq!(
                predictions.first().map(|(word, _)| word.as_str()),
                Some("mood"),
                "repeated personal usage should outrank the generic corpus"
            );
        }

        eprintln!("phase local_adaptation_is_bounded_and_clearable");
        {
            let mut predictor = small();
            for index in 0..(MAX_LEARNED_CONTEXTS + 10) {
                predictor.learn_transition(Some(&format!("context-{index}")), "word");
            }
            assert_eq!(predictor.learned_context_count(), MAX_LEARNED_CONTEXTS);
            predictor.clear_learned();
            assert_eq!(predictor.learned_context_count(), 0);
        }
    }
}
