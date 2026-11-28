//! The on-screen keyboard's own settings: the letter layouts it types with and switches between,
//! how its keys are sized, what it suggests and corrects, the language it reads words in, and what
//! typing taught it. They are the person's, so every device of theirs opens the keyboard the same
//! way and suggests the same words; what the keyboard learns from typing stays theirs alone.
//!
//! The settings live in the person's section (`settings/apps/so.tilo.osk`), so the keyboard's own
//! view reads them as any app reads a setting, and a person, a script or an agent changes them by
//! the setters and actions here.

use ui::tilo_resource;

/// The app whose settings these are: the keyboard reads them as its own.
pub use ui::apps::OSK as KEYBOARD_APP_ID;

/// One word typing taught after another: the word before, the word it led to, and how often the
/// person wrote the two in a row.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct Transition {
    /// The word before it, or empty for the first word of a phrase.
    pub before: String,
    /// The word it led to.
    pub word: String,
    /// How often the person wrote them in a row: a counter, so two devices learning at once
    /// both count.
    pub count: ui::tilo_resource::Counter,
}

/// One letter this person tends to hit off its middle, as their taps taught the keyboard.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct TapOffset {
    /// The letter, lower case.
    pub letter: String,
    /// How far from its middle the taps land, across.
    pub dx: f64,
    /// How far from its middle the taps land, down.
    pub dy: f64,
    /// How many taps taught it.
    pub samples: u32,
}

/// The letter layouts the on-screen keyboard switches between, the one it types with, and what it
/// has learned of this person's typing.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct OskSourceSettings {
    /// The layout it types with.
    #[model(readonly)]
    pub layout: String,
    /// The layouts it switches between, in order.
    #[model(readonly)]
    pub layouts: Vec<String>,
    /// How tall its key rows are: `compact`, `standard` or `large`.
    #[model(readonly)]
    pub size: String,
    /// Whether its suggestion bar offers words.
    #[model(readonly)]
    pub suggestions: bool,
    /// Whether a finished word may be rewritten to a likely correction, on its own of the
    /// suggestion bar.
    #[model(readonly)]
    pub autocorrect: bool,
    /// The language its words are read in: `layout` follows the letter layout, `en` and `fr` are
    /// its own.
    #[model(readonly)]
    pub typing_language: String,
    /// Whether it adapts to what this person types. Turning it off forgets what it learned.
    #[model(readonly)]
    pub personalization: bool,
    /// The words that followed others, as typing taught them.
    #[model(readonly)]
    pub transitions: Vec<Transition>,
    /// The letters this person lands off their middle, as their taps taught.
    #[model(readonly)]
    pub touch: Vec<TapOffset>,
    /// The words the person kept as they typed them: never corrected again.
    #[model(readonly)]
    pub kept: Vec<String>,
}

impl Default for OskSourceSettings {
    fn default() -> Self {
        Self {
            layout: "qwerty".into(),
            layouts: Vec::new(),
            size: "standard".into(),
            suggestions: true,
            autocorrect: true,
            typing_language: "layout".into(),
            personalization: true,
            transitions: Vec::new(),
            touch: Vec::new(),
            kept: Vec::new(),
        }
    }
}

/// The most transitions, tap offsets and kept words the keyboard keeps: what a person typing for
/// years amounts to, small enough to travel with their settings on every device.
const MOST_TRANSITIONS: usize = 4096;
const MOST_KEPT: usize = 256;

impl settings_model::SettingsSchema for OskSourceSettings {
    const SCOPE: settings_model::Scope = settings_model::Scope::Personal;
    const APP: Option<&'static str> = Some(KEYBOARD_APP_ID);
}

#[ui::tilo_resource::resource(kind = "settings.osk-sources")]
impl OskSourceSettings {
    /// Switch between these letter layouts, in this order, typing with one
    /// of them.
    #[action(id = "set-layouts")]
    pub fn set_layouts(
        &mut self,
        #[param("`qwerty` and `azerty`, in switching order")] layouts: Vec<String>,
        #[param("The one typed with, one of them")] active: String,
    ) -> Result<(), tilo_resource::Refusal> {
        if let Some(layout) = layouts
            .iter()
            .find(|layout| !LAYOUTS.contains(&layout.as_str()))
        {
            return Err(tilo_resource::Refusal::new(format!(
                "`{layout}` is not a layout of the on-screen keyboard"
            )));
        }
        let mut seen = std::collections::HashSet::new();
        if !layouts.iter().all(|layout| seen.insert(layout)) {
            return Err(tilo_resource::Refusal::new("a layout is listed twice"));
        }
        if !layouts.contains(&active) {
            return Err(tilo_resource::Refusal::new(format!(
                "`{active}` is not one of the layouts"
            )));
        }
        self.layouts = layouts;
        self.layout = active;
        Ok(())
    }

    /// Set how tall the keyboard's key rows are.
    #[action(id = "set-size", keywords = "keyboard, keys, height")]
    pub fn set_size(
        &mut self,
        #[param("`compact`, `standard` or `large`")] value: String,
    ) -> Result<(), tilo_resource::Refusal> {
        if !SIZES.contains(&value.as_str()) {
            return Err(tilo_resource::Refusal::new(format!(
                "`{value}` is not a keyboard size: `compact`, `standard` or `large`"
            )));
        }
        self.size = value;
        Ok(())
    }

    /// Offer words above the keys, or stop.
    #[action(id = "set-suggestions", keywords = "keyboard, predictions, words")]
    pub fn set_suggestions(&mut self, value: bool) -> Result<(), tilo_resource::Refusal> {
        self.suggestions = value;
        Ok(())
    }

    /// Rewrite a finished word to a likely correction, or never.
    #[action(id = "set-autocorrect", keywords = "keyboard, spelling, correction")]
    pub fn set_autocorrect(&mut self, value: bool) -> Result<(), tilo_resource::Refusal> {
        self.autocorrect = value;
        Ok(())
    }

    /// Set the language the keyboard reads words in, on its own of the letter layout.
    #[action(
        id = "set-typing-language",
        keywords = "keyboard, dictionary, language"
    )]
    pub fn set_typing_language(
        &mut self,
        #[param("`layout` follows the key layout; `en` and `fr` are its own")] value: String,
    ) -> Result<(), tilo_resource::Refusal> {
        if !LANGUAGES.contains(&value.as_str()) {
            return Err(tilo_resource::Refusal::new(format!(
                "`{value}` is not a typing language: `layout`, `en` or `fr`"
            )));
        }
        self.typing_language = value;
        Ok(())
    }

    /// Adapt to what this person types, or stop and forget what typing taught (the recent emoji
    /// the shared picker keeps are the workspace's, and stay).
    #[action(id = "set-personalization", keywords = "keyboard, learn, forget")]
    pub fn set_personalization(&mut self, value: bool) -> Result<(), tilo_resource::Refusal> {
        self.personalization = value;
        if !value {
            self.transitions.clear();
            self.touch.clear();
            self.kept.clear();
        }
        Ok(())
    }

    /// Keep what committing `word` after `before` taught: the pair, and how often the person has
    /// written it.
    #[action(id = "learn", keywords = "keyboard, prediction, learn")]
    pub fn learn(
        &mut self,
        #[param("The word before, or empty for the first word of a phrase")] before: String,
        #[param("The word it led to")] word: String,
    ) -> Result<(), tilo_resource::Refusal> {
        if !self.personalization {
            return Ok(());
        }
        let word = word.to_lowercase();
        let before = before.to_lowercase();
        if word.is_empty() {
            return Ok(());
        }
        if let Some(taught) = self
            .transitions
            .iter_mut()
            .find(|taught| taught.before == before && taught.word == word)
        {
            taught.count.add(1);
            return Ok(());
        }
        if self.transitions.len() >= MOST_TRANSITIONS {
            // The one least often written goes: what this person actually says stays.
            if let Some((least, _)) = self
                .transitions
                .iter()
                .enumerate()
                .min_by_key(|(_, taught)| taught.count)
            {
                self.transitions.swap_remove(least);
            }
        }
        self.transitions.push(Transition {
            before,
            word,
            count: ui::tilo_resource::Counter::new(1),
        });
        Ok(())
    }

    /// Keep where this person lands on `letter` , as their taps taught the keyboard: the middle
    /// of their taps, from how many.
    #[action(id = "teach", keywords = "keyboard, tap, learn")]
    pub fn teach(
        &mut self,
        #[param("The letter tapped, lower case")] letter: String,
        #[param("How far from its middle the taps land, across")] dx: f64,
        #[param("How far from its middle the taps land, down")] dy: f64,
        #[param("How many taps taught it")] samples: u32,
    ) -> Result<(), tilo_resource::Refusal> {
        if !self.personalization {
            return Ok(());
        }
        let letter = letter.to_lowercase();
        if letter.chars().count() != 1 {
            return Ok(());
        }
        let taught = TapOffset {
            letter,
            dx,
            dy,
            samples,
        };
        match self
            .touch
            .iter_mut()
            .find(|kept| kept.letter == taught.letter)
        {
            Some(kept) => *kept = taught,
            None => self.touch.push(taught),
        }
        Ok(())
    }

    /// Keep `word` as the person typed it: the keyboard never corrects it again.
    #[action(id = "keep", keywords = "keyboard, spelling, keep")]
    pub fn keep(
        &mut self,
        #[param("The word, as it was typed")] word: String,
    ) -> Result<(), tilo_resource::Refusal> {
        let word = word.to_lowercase();
        if word.is_empty() || self.kept.contains(&word) {
            return Ok(());
        }
        if self.kept.len() >= MOST_KEPT {
            self.kept.remove(0);
        }
        self.kept.push(word);
        Ok(())
    }

    /// Forget what typing taught: the transitions, the tap offsets and the kept words. The
    /// personalization choice itself stays as it is.
    #[action(id = "forget-learned", keywords = "keyboard, forget, learn")]
    pub fn forget_learned(&mut self) -> Result<(), tilo_resource::Refusal> {
        self.transitions.clear();
        self.touch.clear();
        self.kept.clear();
        Ok(())
    }
}

/// The letter layouts the on-screen keyboard draws.
pub const LAYOUTS: [&str; 2] = ["qwerty", "azerty"];
/// The keyboard's key sizes.
pub const SIZES: [&str; 3] = ["compact", "standard", "large"];
/// The languages its words may be read in, besides the letter layout's.
pub const LANGUAGES: [&str; 3] = ["layout", "en", "fr"];

#[cfg(test)]
mod tests {
    use super::*;

    /// The keyboard's choices take only what it can draw and read: a layout, a size or a language
    /// it has not is refused, and the choice stands as it was.
    #[test]
    fn choices_it_cannot_draw_are_refused() {
        let mut settings = OskSourceSettings::default();
        assert!(settings.set_size("huge".into()).is_err());
        assert!(settings.set_typing_language("de".into()).is_err());
        assert!(
            settings
                .set_layouts(vec!["qwerty".into(), "dvorak".into()], "qwerty".into())
                .is_err()
        );
        assert!(
            settings
                .set_layouts(vec!["qwerty".into(), "qwerty".into()], "qwerty".into())
                .is_err()
        );
        assert!(
            settings
                .set_layouts(vec!["qwerty".into()], "azerty".into())
                .is_err()
        );
        assert_eq!(settings, OskSourceSettings::default());

        settings.set_size("large".into()).expect("a size it has");
        settings
            .set_typing_language("fr".into())
            .expect("a language it reads");
        settings
            .set_layouts(vec!["azerty".into(), "qwerty".into()], "azerty".into())
            .expect("layouts it draws");
        settings.set_autocorrect(false).expect("corrections off");
        assert!(
            settings.suggestions,
            "corrections are their own choice: suggestions stay on"
        );
        assert_eq!(
            (
                settings.size.as_str(),
                settings.typing_language.as_str(),
                settings.layout.as_str()
            ),
            ("large", "fr", "azerty")
        );
        assert_eq!(settings.layouts, ["azerty", "qwerty"]);
    }

    /// What typing teaches adds up, lower case, and stays bounded: the pair written least goes
    /// first. Turning personalization off forgets it and learns nothing more; forgetting what was
    /// learned keeps the choice.
    #[test]
    fn what_typing_teaches_adds_up_and_is_forgotten() {
        let mut settings = OskSourceSettings::default();
        settings.learn(String::new(), "Hi".into()).expect("learned");
        settings.learn("hi".into(), "you".into()).expect("learned");
        settings.learn("hi".into(), "you".into()).expect("learned");
        settings.teach("E".into(), 0.1, -0.05, 12).expect("taught");
        settings
            .teach("e".into(), 0.2, 0.0, 13)
            .expect("taught again");
        settings.keep("Teh".into()).expect("kept");
        settings.keep("teh".into()).expect("kept once");
        assert_eq!(
            settings.transitions,
            [
                Transition {
                    before: String::new(),
                    word: "hi".into(),
                    count: ui::tilo_resource::Counter::new(1)
                },
                Transition {
                    before: "hi".into(),
                    word: "you".into(),
                    count: ui::tilo_resource::Counter::new(2)
                },
            ]
        );
        assert_eq!(
            settings.touch.len(),
            1,
            "one offset per letter: {:?}",
            settings.touch
        );
        assert_eq!(settings.touch[0].samples, 13);
        assert_eq!(settings.kept, ["teh"]);

        for at in 0..MOST_TRANSITIONS {
            settings
                .learn("x".into(), format!("w{at}"))
                .expect("learned");
        }
        assert_eq!(settings.transitions.len(), MOST_TRANSITIONS, "bounded");
        assert!(
            settings.transitions.iter().any(|t| t.word == "you"),
            "the pair written most stays"
        );

        settings.forget_learned().expect("forgotten");
        assert!(
            settings.transitions.is_empty()
                && settings.touch.is_empty()
                && settings.kept.is_empty()
        );
        assert!(settings.personalization, "forgetting keeps the choice");

        settings.learn("hi".into(), "you".into()).expect("learned");
        settings.set_personalization(false).expect("off");
        assert!(settings.transitions.is_empty(), "turning it off forgets");
        settings.learn("hi".into(), "you".into()).expect("asked");
        settings.teach("a".into(), 0.1, 0.1, 3).expect("asked");
        assert!(
            settings.transitions.is_empty() && settings.touch.is_empty(),
            "nothing is learned while it is off"
        );
    }
}

#[cfg(test)]
mod merge_tests {
    use super::OskSourceSettings;
    use ui::tilo_resource::Replica;

    /// Two devices learning the same pair at once both count: the lesson is a counter, so the
    /// two increments add up instead of one replacing the other.
    #[test]
    fn two_devices_learning_the_same_pair_both_count() {
        let lesson = || OskSourceSettings::LEARN.with(("hi".to_owned(), "you".to_owned()));
        let mut first = Replica::<OskSourceSettings>::new().in_app("so.tilo.model", "Model");
        first.apply(lesson()).unwrap();
        let mut second = first.fork();
        first.apply(lesson()).unwrap();
        second.apply(lesson()).unwrap();
        first.sync_with(&mut second).unwrap();

        let transitions = &first.state().unwrap().transitions;
        assert_eq!(
            transitions.len(),
            1,
            "one pair, learned twice: {transitions:?}"
        );
        assert_eq!(
            transitions[0].count.get(),
            3,
            "the shared lesson and one from each device"
        );
        assert_eq!(second.state().unwrap(), first.state().unwrap());
    }
}
