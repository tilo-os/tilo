//! The keyboard's size, autocorrect and typing-language preferences. They are
//! separate promises: the size changes only the keys, autocorrect changes what
//! was typed, suggestions offer what might, and the dictionary language need
//! not match the letter layout.

use super::*;

impl OskWidget {
    /// Whether a finished word may be rewritten to a likely correction.
    pub fn autocorrect_enabled(&self) -> bool {
        self.autocorrect_enabled
    }

    /// The chosen key height.
    pub fn key_size(&self) -> KeyboardSize {
        self.key_size
    }

    /// The language the dictionary and autocorrection use.
    pub fn typing_language(&self) -> Language {
        self.language
    }

    /// Correct finished words, or leave every word as typed, as the
    /// autocorrect preference says. The suggestion bar is unaffected.
    pub fn set_autocorrect_enabled(&mut self, enabled: bool) {
        self.autocorrect_enabled = enabled;
    }

    /// Set the key height. The preference changes no other app's density.
    pub fn set_key_size(&mut self, size: KeyboardSize) {
        if self.key_size == size {
            return;
        }
        self.key_size = size;
        self.structure_dirty = true;
    }

    /// Choose the typing language explicitly, or follow the letter layout
    /// when `code` is `None`. The previous language's context, corrections
    /// and learned words do not carry over.
    pub(super) fn set_typing_language(&mut self, code: Option<&str>) {
        self.typing_language = code.and_then(Language::from_code);
        self.follow_layout_language();
    }

    /// Switch the word data to the chosen typing language, or to the
    /// language the preferred letter layout types when none was chosen.
    /// The previous language's context, corrections and learned words do
    /// not carry over.
    pub(super) fn follow_layout_language(&mut self) {
        let Some(language) = self
            .typing_language
            .or_else(|| Language::for_layout(&self.preferred_text_layout))
        else {
            return;
        };
        if language == self.language {
            return;
        }
        self.language = language;
        self.predictor = Predictor::for_language(language);
        if self.personalization_enabled
            && let Some(transitions) = self.learned.transitions.get(language.code())
        {
            self.predictor.adopt_learned(transitions);
        }
        self.swipe_model.set_language(language);
        self.autocorrect = None;
        self.last_correction = None;
        self.prev_word = None;
        self.refresh_completion_suggestions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_word(osk: &mut OskWidget, word: &str) {
        for c in word.chars() {
            osk.apply_action(KeyAction::Char(c.to_string()));
        }
    }

    fn lay_out(osk: &mut OskWidget, w: f32, h: f32) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = osk.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(w, h),
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        taffy
            .compute_layout(
                root,
                Size {
                    width: taffy::AvailableSpace::Definite(w),
                    height: taffy::AvailableSpace::Definite(h),
                },
            )
            .unwrap();
        osk.apply_layout(&taffy, child, Point::ORIGIN);
    }

    fn key_rect(osk: &OskWidget, label: &str) -> (KeyId, Rect) {
        let layout = osk.current_layout();
        osk.key_geom
            .iter()
            .find(|(id, _)| layout.key(*id).is_some_and(|k| k.label == label))
            .copied()
            .unwrap()
    }

    #[test]
    fn autocorrect_is_independent_of_the_suggestion_bar() {
        let mut without_suggestions = OskWidget::new();
        without_suggestions.set_suggestions_enabled(false);
        type_word(&mut without_suggestions, "teh");
        without_suggestions.apply_action(KeyAction::Space);
        assert_eq!(
            without_suggestions.pending_text(),
            "the ",
            "turning suggestions off leaves correction alone"
        );

        let mut without_correction = OskWidget::new();
        without_correction.set_autocorrect_enabled(false);
        type_word(&mut without_correction, "teh");
        without_correction.apply_action(KeyAction::Space);
        assert_eq!(
            without_correction.pending_text(),
            "teh ",
            "the suggestion bar can offer words without rewriting what was typed"
        );
    }

    #[test]
    fn the_typing_language_is_chosen_independently_of_the_key_layout() {
        let mut osk = OskWidget::new();
        osk.present_preferences(&crate::resource::OskPreferences {
            layout: "qwerty".into(),
            typing_language: Some("fr".into()),
            ..Default::default()
        });
        assert_eq!(osk.active_layout_name(), "qwerty");
        assert_eq!(osk.language(), Language::French, "QWERTY types French");

        osk.select_text_layout("azerty");
        assert_eq!(osk.active_layout_name(), "azerty");
        assert_eq!(
            osk.language(),
            Language::French,
            "changing the keys does not silently change the typing language"
        );
        osk.present_preferences(&crate::resource::OskPreferences {
            layout: "azerty".into(),
            typing_language: Some("en".into()),
            ..Default::default()
        });
        assert_eq!(osk.active_layout_name(), "azerty");
        assert_eq!(osk.language(), Language::English, "AZERTY types English");

        osk.present_preferences(&crate::resource::OskPreferences {
            layout: "azerty".into(),
            typing_language: None,
            ..Default::default()
        });
        assert_eq!(
            osk.language(),
            Language::French,
            "no chosen language follows the layout"
        );
    }

    #[test]
    fn the_key_size_changes_key_height_without_changing_the_desktop_density() {
        let mut compact = OskWidget::new();
        compact.present_preferences(&crate::resource::OskPreferences {
            size: KeyboardSize::Compact,
            ..Default::default()
        });
        lay_out(&mut compact, 390.0, 844.0);
        let (_, compact_key) = key_rect(&compact, "q");

        let mut standard = OskWidget::new();
        lay_out(&mut standard, 390.0, 844.0);
        let (_, standard_key) = key_rect(&standard, "q");

        let mut large = OskWidget::new();
        large.present_preferences(&crate::resource::OskPreferences {
            size: KeyboardSize::Large,
            ..Default::default()
        });
        lay_out(&mut large, 390.0, 844.0);
        let (_, large_key) = key_rect(&large, "q");
        assert!(compact_key.height() < standard_key.height());
        assert!(standard_key.height() < large_key.height());
        assert_eq!(large.key_size(), KeyboardSize::Large);
    }
}
