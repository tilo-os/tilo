//! When a [`super::TextInput`] checks its spelling, and the misspelled words
//! it marks.

use std::ops::Range;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke, Vec2};

use crate::text_edit::word_at_caret;
use crate::text_render::TextRenderer;
use crate::text_selection::{line_boundaries, prev_char_boundary, word_boundaries};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::TextRole;
use crate::widget::Widget;

impl super::TextInput {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Autocorrection {
    pub(super) start: usize,
    pub(super) end: usize,
    boundary_end: usize,
    pub(super) original: String,
    replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AutocorrectionPreview {
    language: String,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) original: String,
    pub(super) replacement: String,
    pub(super) automatic: bool,
}

pub(super) type PreviewKey = (String, String, usize, usize, bool);

pub(super) fn is_autocorrect_boundary(character: char) -> bool {
    character.is_whitespace() || (character.is_ascii_punctuation() && character != '\'')
}

pub(super) const SPELL_EDIT_SETTLE: Duration = Duration::from_millis(250);
#[derive(PartialEq)]
pub(super) struct PersonalSpelling {
    owner: Option<[u8; 32]>,
    generation: u64,
    configured: String,
    pub(super) language: String,
    pub(super) snapshot: Option<crate::personal_dictionary::Snapshot>,
}

impl super::TextInput {
    /// Make `correction` of the word right before the caret, or before the
    /// one word boundary after it, as a step of its own: undoing it gives
    /// back the word as typed and keeps the typing before it. The caret
    /// stays after the boundary.
    pub(super) fn apply_autocorrection(&mut self, correction: AutocorrectionPreview) -> bool {
        let Some(boundary) = self.text.get(correction.end..self.core.caret()) else {
            return false;
        };
        if self.has_selection()
            || boundary.chars().count() > 1
            || boundary.chars().any(char::is_alphanumeric)
        {
            return false;
        }
        let boundary_len = boundary.len();
        let Some(end) = self.replace_step(
            correction.start..correction.end,
            &correction.original,
            &correction.replacement,
        ) else {
            return false;
        };
        let boundary_end = end.saturating_add(boundary_len);
        self.set_cursor_pos(boundary_end);
        self.last_autocorrection = Some(Autocorrection {
            start: correction.start,
            end,
            boundary_end,
            original: correction.original,
            replacement: correction.replacement,
        });
        true
    }

    pub(super) fn field_allows_spelling(&self) -> bool {
        !self.holds_secret()
            && self.spellcheck.unwrap_or_else(|| {
                self.kind.checks_spelling()
                    && !self.mono
                    && self.highlighter.is_none()
                    && self.tab_spaces.is_none()
            })
    }

    pub(super) fn checks_spelling(&self) -> bool {
        self.field_allows_spelling() && crate::text_assistance::preferences().check_spelling
    }

    /// The correction a word boundary typed now makes of the word before the
    /// caret, made by [`Self::apply_autocorrection`] once the boundary is
    /// written, unless the owner defers it.
    pub(super) fn autocorrect_previous_word(&mut self) -> Option<AutocorrectionPreview> {
        if !self.autocorrect_enabled() || self.has_selection() || self.core.caret() == 0 {
            return None;
        }
        self.refresh_autocorrection_preview();
        // The visual preview is deliberately debounced (and only painted for a
        // focused field), but committing a boundary must not depend on whether
        // that preview has already had a frame in which to appear. This also
        // keeps fast typists from outrunning an external-language checker.
        let preview = self
            .autocorrection_preview
            .clone()
            .filter(|preview| preview.automatic)
            .or_else(|| self.autocorrection_candidate())?;
        if self.defer_autocorrection {
            self.pending_autocorrection = Some(preview);
            return None;
        }
        Some(preview)
    }

    pub(super) fn word_ending_at_caret(&self) -> Option<(usize, usize)> {
        if self.has_selection() || self.core.caret() == 0 {
            return None;
        }
        let probe = prev_char_boundary(&self.text, self.core.caret());
        let (start, end) = word_boundaries(&self.text, probe);
        (start < end && end == self.core.caret()).then_some((start, end))
    }

    pub(super) fn refresh_autocorrection_preview(&mut self) {
        self.refresh_suggestion_at(crate::motion::frame_now());
    }

    /// Bring the correction offered for the word under the caret up to
    /// `now`; whether the offer changed. A word settling under the caret
    /// is a change no input announces, so a host that keeps the field's
    /// last drawing (a block editor's line) repaints it when this says so.
    pub(crate) fn settle_suggestion_at(&mut self, now: Instant) -> bool {
        let before = self.autocorrection_preview.clone();
        self.refresh_suggestion_at(now);
        self.autocorrection_preview != before
    }

    pub(super) fn clear_suggestion(&mut self) {
        self.autocorrection_preview_key = None;
        self.autocorrection_preview = None;
    }

    pub(in crate::widget) fn refresh_suggestion_at(&mut self, now: Instant) {
        self.refresh_personal_spelling();
        let offers = self.autocorrect_enabled() || self.checks_spelling();
        if !offers || !self.is_focused || self.core.has_preedit() {
            self.caret_word.clear();
            self.clear_suggestion();
            return;
        }
        self.caret_word
            .observe(&self.text, self.core.selection(), now);
        let Some((range, word)) = self.caret_word.current() else {
            self.clear_suggestion();
            return;
        };
        let settled = self.caret_word.settled(now).is_some();
        let key = (
            self.spell_language.clone(),
            word.to_owned(),
            range.start,
            range.end,
            settled,
        );
        if self.autocorrection_preview_key.as_ref() == Some(&key) {
            return;
        }
        let preview = self.suggestion_for(range, settled);
        self.autocorrection_preview_key = Some(key);
        self.autocorrection_preview = preview;
    }

    pub(super) fn personal_word(&self, word: &str) -> bool {
        self.personal_spelling.as_ref().is_some_and(|personal| {
            personal
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.contains(word, &personal.language))
        })
    }

    pub(super) fn refresh_personal_spelling(&mut self) {
        if self.holds_secret() || (!self.checks_spelling() && !self.autocorrect_enabled()) {
            self.personal_spelling = None;
            self.spell_dictionary = None;
            return;
        }
        if !self.spell_active && !self.is_focused {
            return;
        }
        // The person's own words live in the shell's records: they change
        // when the person or their settings do, never per frame. While both
        // hold, the words already read — and the dictionary built from them —
        // stand, and reading them again would copy and decode the whole
        // dictionary for every frame of every field that has been used.
        let generation = crate::local_identity::generation();
        let view = crate::personal_dictionary::revision();
        if self.last_personal_spelling_read == Some((generation, view))
            && self.personal_spelling.as_ref().is_some_and(|personal| {
                personal.generation == generation && personal.configured == self.spell_language
            })
        {
            return;
        }
        self.last_personal_spelling_read = Some((generation, view));
        let current = PersonalSpelling {
            owner: crate::local_identity::bound().map(|identity| identity.group_public_key),
            generation,
            configured: self.spell_language.clone(),
            language: self.spell_language.replace('_', "-").to_ascii_lowercase(),
            snapshot: crate::personal_dictionary::snapshot().ok(),
        };
        if self.personal_spelling.as_ref() != Some(&current) {
            if self.personal_spelling.as_ref().is_some_and(|previous| {
                previous.owner != current.owner || previous.generation != current.generation
            }) {
                self.ignored_spellings.clear();
            }
            let words = current
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.hints(&current.language).collect::<Vec<_>>())
                .unwrap_or_default();
            self.spell_dictionary = Some(spellcheck::Dictionary::from_words(words));
            self.personal_spelling = Some(current);
            self.spell_cache_key = None;
            self.clear_suggestion();
        }
    }

    pub(super) fn suggestion_for(
        &mut self,
        range: Range<usize>,
        settled: bool,
    ) -> Option<AutocorrectionPreview> {
        if self.autocorrect_enabled()
            && range.end == self.core.caret()
            && let Some(automatic) = self.autocorrection_candidate()
        {
            return Some(automatic);
        }
        if !settled || !self.checks_spelling() {
            return None;
        }
        let original = self.text.get(range.clone())?.to_string();
        let dictionary = self
            .spell_dictionary
            .get_or_insert_with(spellcheck::Dictionary::default);
        if dictionary.contains(&original)
            || self.personal_word(&original)
            || self.ignored_spellings.contains(&original.to_lowercase())
        {
            return None;
        }
        let (line_start, _) = line_boundaries(&self.text, range.start);
        let context = self.text.get(line_start..range.end)?;
        let local = range.start.saturating_sub(line_start)..range.end.saturating_sub(line_start);
        let misspelled = spellcheck::spans_for_language(&self.spell_language, context)
            .into_iter()
            .any(|(start, end)| start == local.start && end == local.end);
        if !misspelled {
            return None;
        }
        let replacement = spellcheck::suggestions_for_language(&self.spell_language, &original, 1)
            .into_iter()
            .next()
            .filter(|replacement| *replacement != original)?;
        Some(AutocorrectionPreview {
            language: self.spell_language.clone(),
            start: range.start,
            end: range.end,
            original,
            replacement,
            automatic: false,
        })
    }

    #[must_use]
    pub fn overhang(&self) -> Option<Rect> {
        self.suggestion_chip
    }

    pub(super) fn accept_suggestion(&mut self) -> bool {
        self.refresh_personal_spelling();
        let Some(preview) = self.autocorrection_preview.take() else {
            return false;
        };
        if self
            .replace_step(
                preview.start..preview.end,
                &preview.original,
                &preview.replacement,
            )
            .is_none()
        {
            self.clear_suggestion();
            return false;
        }
        self.clear_suggestion();
        self.suggestion_chip = None;
        self.spell_cache_key = None;
        true
    }

    pub(super) fn autocorrection_candidate(&mut self) -> Option<AutocorrectionPreview> {
        self.activate_spelling();
        self.refresh_personal_spelling();
        let (start, end) = self.word_ending_at_caret()?;
        let original = self.text.get(start..end)?.to_string();
        let dictionary = self
            .spell_dictionary
            .get_or_insert_with(spellcheck::Dictionary::default);
        if self
            .declined_autocorrection
            .as_ref()
            .is_some_and(|(at, word)| *at == start && *word == original)
        {
            return None;
        }
        let sure = spellcheck::sure_correction(&self.spell_language, &original);
        if sure.is_none() && (dictionary.contains(&original) || self.personal_word(&original)) {
            return None;
        }
        let replacement = match sure {
            Some(sure) => sure,
            None => spellcheck::probable_correction(&self.spell_language, &original)?,
        };
        (replacement != original).then(|| AutocorrectionPreview {
            language: self.spell_language.clone(),
            start,
            end,
            original,
            replacement,
            automatic: true,
        })
    }

    /// The boundary is written before the correction, so undoing the
    /// correction keeps the boundary that was typed.
    pub(super) fn insert_word_boundary(&mut self, boundary: &str) {
        let correction = self.autocorrect_previous_word();
        self.insert_str(boundary);
        if let Some(correction) = correction {
            self.apply_autocorrection(correction);
        }
    }

    pub(super) fn undo_last_autocorrection(&mut self, remove_boundary: bool) -> bool {
        let Some(correction) = self.last_autocorrection.clone() else {
            return false;
        };
        if self.text.get(correction.start..correction.end) != Some(&correction.replacement) {
            self.last_autocorrection = None;
            return false;
        }
        let replace_end = if remove_boundary {
            if self.core.caret() != correction.boundary_end {
                return false;
            }
            let Some(boundary) = self.text.get(correction.end..correction.boundary_end) else {
                return false;
            };
            if boundary.chars().count() > 1 || boundary.chars().any(char::is_alphanumeric) {
                return false;
            }
            correction.boundary_end
        } else {
            correction.end
        };
        self.core.set_selection(correction.start, replace_end);
        self.insert_str(&correction.original);
        self.last_autocorrection = None;
        self.declined_autocorrection = Some((correction.start, correction.original));
        true
    }

    pub(super) fn actively_edited_word(&self) -> Option<(usize, usize)> {
        if !self.is_focused
            || self.has_selection()
            || self.typing.last_edit().is_none_or(|edited| {
                crate::time::now().saturating_duration_since(edited) >= SPELL_EDIT_SETTLE
            })
        {
            return None;
        }

        let range = word_at_caret(&self.text, self.core.caret())?;
        Some((range.start, range.end))
    }

    pub(super) fn spelling_span_is_visible(&self, start: usize, end: usize) -> bool {
        if self
            .autocorrection_preview
            .as_ref()
            .is_some_and(|preview| end > preview.start && start < preview.end)
        {
            return false;
        }
        self.actively_edited_word()
            .is_none_or(|(active_start, active_end)| end <= active_start || start >= active_end)
    }

    #[allow(
        clippy::while_float,
        reason = "x advances by a fixed positive 2px and is clamped to x1"
    )]
    pub(super) fn draw_spelling(&mut self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        self.refresh_autocorrection_preview();
        self.refresh_spelling();
        let color = theme.tokens.colors.semantic.danger;
        let spelling = self
            .spell_spans
            .iter()
            .copied()
            .filter(|&(start, end)| self.spelling_span_is_visible(start, end));
        let mistakes = self
            .mistakes
            .as_ref()
            .map(|marker| marker(&self.text))
            .unwrap_or_default()
            .into_iter()
            .filter(|range| self.text.get(range.clone()).is_some())
            .map(|range| (range.start, range.end));
        let marked: Vec<(usize, usize)> = spelling.chain(mistakes).collect();
        for (start, end) in marked {
            for rect in self.selection_rects(theme, start, end) {
                let x0 = rect.x0.max(self.rect.x0);
                let x1 = rect.x1.min(self.rect.x1);
                if x1 <= x0 || rect.y1 < self.rect.y0 || rect.y1 > self.rect.y1 {
                    continue;
                }
                let y = rect.y1 - 1.0;
                let mut path = vello::kurbo::BezPath::new();
                path.move_to((x0, y));
                let mut x = x0;
                let mut up = false;
                while x < x1 {
                    x = (x + 2.0).min(x1);
                    path.line_to((x, y + if up { -1.0 } else { 1.0 }));
                    up = !up;
                }
                scene.stroke(&Stroke::new(1.0), transform, color, None, &path);
            }
        }
    }

    pub(super) fn draw_autocorrection(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(correction) = self.last_autocorrection.as_ref() else {
            return;
        };
        if self.text.get(correction.start..correction.end) != Some(&correction.replacement) {
            return;
        }
        let (start, end) = (correction.start, correction.end);
        for rect in self.selection_rects(theme, start, end) {
            let line = vello::kurbo::Line::new(
                (rect.x0.max(self.rect.x0), rect.y1 - 0.5),
                (rect.x1.min(self.rect.x1), rect.y1 - 0.5),
            );
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.tokens.colors.semantic.info,
                None,
                &line,
            );
        }
    }

    pub(super) fn draw_autocorrection_preview(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.refresh_autocorrection_preview();
        self.suggestion_chip = None;
        let Some(preview) = self.autocorrection_preview.as_ref() else {
            return;
        };
        let label = if preview.automatic {
            format!("Space → {}", preview.replacement)
        } else {
            preview.replacement.clone()
        };
        let style = theme.text(TextRole::Caption);
        let (text_width, text_height) = text_renderer.measure_style(&label, None, &style);
        let pad_x = 8.0_f64;
        let pad_y = 4.0_f64;
        let width = f64::from(text_width) + pad_x * 2.0;
        let height = f64::from(text_height) + pad_y * 2.0;
        let caret = Widget::ime_cursor_rect(self).unwrap_or(self.rect);
        let max_x = (self.rect.x1 - width).max(self.rect.x0);
        let x = caret.x0.clamp(self.rect.x0, max_x);
        let below = caret.y1 + 4.0;
        let y = if below + height <= self.rect.y1 - 4.0 {
            below
        } else {
            self.rect.y0 - height - 4.0
        };
        let rect = Rect::new(x, y, x + width, y + height);
        self.suggestion_chip = Some(rect);
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.detail));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(theme.tokens.colors.surfaces.overlay),
            None,
            &shape,
        );
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(rect.x0 + pad_x, rect.y0 + pad_y)),
            None,
            &style,
        );
    }
}
