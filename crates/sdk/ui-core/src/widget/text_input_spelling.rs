//! When a [`super::TextInput`] checks its spelling, and the misspelled words
//! it marks.

use std::ops::Range;

impl super::TextInput {
    /// The person is using this field: spelling applies to it from now on.
    pub(super) fn activate_spelling(&mut self) {
        if !self.spell_active {
            self.spell_active = true;
            self.warm_up_spelling();
        }
    }

    /// Build the field's dictionary off the event loop, once a field that
    /// checks spelling is in use. Fields are made at startup and most are
    /// never typed in; the dictionary is a large share of an idle desktop's
    /// heap, so it is not built for them.
    pub(super) fn warm_up_spelling(&self) {
        if self.checks_spelling() {
            spellcheck::warm_up_language(&self.spell_language);
        }
    }

    pub(super) fn refresh_spelling(&mut self) {
        self.refresh_personal_spelling();
        let generation = spellcheck::dictionaries_generation();
        if self.spell_generation != generation {
            self.spell_generation = generation;
            self.spell_cache_key = None;
        }
        if !self.checks_spelling() || self.text.is_empty() {
            self.spell_spans.clear();
            self.spell_cache_key = None;
            return;
        }
        if !self.spell_active {
            self.spell_spans.clear();
            self.spell_spans_stale = true;
            return;
        }
        let Some((language, checked)) = self.spell_cache_key.take() else {
            self.spell_spans = self.misspelled_in(0..self.text.len());
            self.spell_spans_stale = false;
            self.spell_cache_key = Some((self.spell_language.clone(), self.text.clone()));
            return;
        };
        if language == self.spell_language && checked == self.text {
            self.spell_cache_key = Some((language, checked));
            return;
        }
        if language == self.spell_language && !self.spell_spans_stale {
            // Words never cross a line feed: check only the lines the edit
            // touched and move the marks after them.
            let edit = crate::text_break::LineEdit::between(&checked, &self.text);
            let fresh = self.misspelled_in(edit.start..edit.new_end);
            let (old_len, new_len) = (checked.len(), self.text.len());
            let previous = std::mem::take(&mut self.spell_spans);
            self.spell_spans = previous
                .iter()
                .copied()
                .filter(|&(_, end)| end < edit.start)
                .chain(fresh)
                .chain(
                    previous
                        .iter()
                        .copied()
                        .filter(|&(start, _)| start > edit.old_end)
                        .map(|(start, end)| {
                            (
                                start.saturating_add(new_len).saturating_sub(old_len),
                                end.saturating_add(new_len).saturating_sub(old_len),
                            )
                        }),
                )
                .collect();
        } else {
            self.spell_spans = self.misspelled_in(0..self.text.len());
        }
        self.spell_spans_stale = false;
        let mut checked = checked;
        checked.clone_from(&self.text);
        self.spell_cache_key = Some((self.spell_language.clone(), checked));
    }

    /// Misspelled words of `range` (whole lines of the text), in text bytes.
    pub(super) fn misspelled_in(&mut self, range: Range<usize>) -> Vec<(usize, usize)> {
        self.spell_dictionary
            .get_or_insert_with(spellcheck::Dictionary::default);
        let Some(text) = self.text.get(range.clone()) else {
            return Vec::new();
        };
        let dictionary = self.spell_dictionary.as_ref();
        let personal = self.personal_spelling.as_ref();
        spellcheck::spans_for_language(&self.spell_language, text)
            .into_iter()
            .filter(|&(start, end)| {
                text.get(start..end).is_some_and(|word| {
                    !dictionary.is_some_and(|dictionary| dictionary.contains(word))
                        && !personal.is_some_and(|personal| {
                            personal
                                .snapshot
                                .as_ref()
                                .is_some_and(|snapshot| snapshot.contains(word, &personal.language))
                        })
                        && !self.ignored_spellings.contains(&word.to_lowercase())
                })
            })
            .map(|(start, end)| {
                (
                    start.saturating_add(range.start),
                    end.saturating_add(range.start),
                )
            })
            .collect()
    }
}
