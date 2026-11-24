//! What the keyboard learned from one person's typing: the words they
//! commit after which words, the words they kept as typed when corrected,
//! and where they tend to hit each letter.
//!
//! Learning stays in the person's profile. The keyboard excludes secret fields
//! and learns only while personalization is enabled.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Counter, JsonSchema, Model, Set, resource};

use crate::autocorrect::MAX_KEPT_WORDS;
use crate::predict::{MAX_LEARNED_CONTEXTS, MAX_LEARNED_WORDS_PER_CONTEXT};
use crate::touch_model::{KeyOffset as EngineOffset, MAX_LEARNED_KEYS};

/// Where one person tends to hit a letter, as the resource keeps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Model, JsonSchema)]
pub struct KeyOffset {
    pub dx: f32,
    pub dy: f32,
    pub samples: u32,
}

impl From<KeyOffset> for EngineOffset {
    fn from(stored: KeyOffset) -> Self {
        Self {
            dx: stored.dx,
            dy: stored.dy,
            samples: stored.samples,
        }
    }
}

impl From<EngineOffset> for KeyOffset {
    fn from(engine: EngineOffset) -> Self {
        Self {
            dx: engine.dx,
            dy: engine.dy,
            samples: engine.samples,
        }
    }
}

/// The transitions as the predictor counts them, from the resource's counters.
pub fn counts(
    transitions: &BTreeMap<String, BTreeMap<String, Counter>>,
) -> BTreeMap<String, BTreeMap<String, u32>> {
    transitions
        .iter()
        .map(|(context, words)| {
            (
                context.clone(),
                words
                    .iter()
                    .map(|(word, count)| (word.clone(), u32::try_from(count.get()).unwrap_or(u32::MAX)))
                    .collect(),
            )
        })
        .collect()
}

/// What the keyboard learned from one person's typing. Only its verbs
/// change it: learning comes from typing, never from an edit of a count.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Learned {
    /// Per language: the previous word (empty at a start) → a word committed
    /// after it → how often. Two devices counting the same word add up.
    #[model(readonly)]
    pub transitions: BTreeMap<String, BTreeMap<String, BTreeMap<String, Counter>>>,
    /// Words kept as typed when the keyboard corrected them.
    #[model(readonly)]
    pub kept: Set<String>,
    /// Where the person tends to hit each letter, by the letter.
    #[model(readonly)]
    pub touch: BTreeMap<String, KeyOffset>,
    #[model(readonly, default = false)]
    pub(crate) restored_previous: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Lesson {
    /// The language the transitions are in.
    pub language: String,
    /// Words committed, each after the previous word (empty at a start).
    pub transitions: Vec<(String, String)>,
    /// Words kept as typed.
    pub kept: Vec<String>,
    /// Where each letter tends to be hit now.
    pub touch: Vec<(char, KeyOffset)>,
}

impl Lesson {
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.kept.is_empty() && self.touch.is_empty()
    }
}

/// What the keyboard learned is what the person typed, so the resource
/// answers no read: its changes are published so they can be taken back,
/// and a palette, an agent or a script sees that there is something to
/// undo and never the words themselves.
#[resource(kind = "osk.learned", read = false)]
impl Learned {
    /// Remember what typing taught. Learning comes from use, so no undo
    /// takes it back.
    #[action(external)]
    pub fn learn(&mut self, #[param("What typing taught")] lesson: Lesson) {
        if !lesson.transitions.is_empty() {
            let contexts = self.transitions.entry(lesson.language).or_default();
            for (previous, word) in lesson.transitions {
                if !contexts.contains_key(&previous) && contexts.len() >= MAX_LEARNED_CONTEXTS {
                    continue;
                }
                let words = contexts.entry(previous).or_default();
                if !words.contains_key(&word) && words.len() >= MAX_LEARNED_WORDS_PER_CONTEXT {
                    continue;
                }
                words.entry(word).or_default().add(1);
            }
        }
        for word in lesson.kept {
            if !self.kept.contains(&word) && self.kept.len() < MAX_KEPT_WORDS {
                self.kept.insert(word);
            }
        }
        for (letter, offset) in lesson.touch {
            let key = letter.to_string();
            if !self.touch.contains_key(&key) && self.touch.len() >= MAX_LEARNED_KEYS {
                continue;
            }
            self.touch.insert(key, offset);
        }
    }

    /// Restore the former workspace's saved learning to the profile.
    #[action(by_app, housekeeping, writes = transitions, writes = kept, writes = touch, writes = restored_previous)]
    pub fn restore_previous(
        &mut self,
        learned: serde_json::Value,
    ) -> Result<(), tilo_resource::Refusal> {
        if self.restored_previous || !self.is_empty() {
            return Ok(());
        }
        let previous = Self::from_json(&learned)
            .map_err(|error| tilo_resource::Refusal::new(error.to_string()))?;
        *self = previous;
        self.restored_previous = true;
        Ok(())
    }

    #[action]
    pub fn forget(&mut self) {
        *self = Self {
            restored_previous: true,
            ..Self::default()
        };
    }
}

impl Learned {
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.kept.is_empty() && self.touch.is_empty()
    }

    pub fn count(&self, language: &str, previous: &str, word: &str) -> u32 {
        self.transitions
            .get(language)
            .and_then(|contexts| contexts.get(previous))
            .and_then(|words| words.get(word))
            .map_or(0, |count| u32::try_from(count.get()).unwrap_or(u32::MAX))
    }

    pub fn touch_offsets(&self) -> BTreeMap<char, EngineOffset> {
        self.touch
            .iter()
            .filter_map(|(key, offset)| Some((key.chars().next()?, (*offset).into())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::{Invoke, Replica};

    fn lesson(transitions: &[(&str, &str)]) -> Lesson {
        Lesson {
            language: "en".into(),
            transitions: transitions
                .iter()
                .map(|(previous, word)| ((*previous).to_owned(), (*word).to_owned()))
                .collect(),
            ..Lesson::default()
        }
    }

    fn learn(lesson: Lesson) -> Invoke<Learned> {
        Learned::LEARN.with(learned::Learn { lesson })
    }

    tilo_test_env::scenario! {
        fn learned_lessons_count_merge_stay_bounded_and_forget;

        phase lessons_add_up_and_forgetting_leaves_nothing;
        {
            let mut record = Replica::<Learned>::new();
            record
                .apply(learn(lesson(&[("good", "mood"), ("", "hello")])))
                .unwrap();
            record
                .apply(learn(Lesson {
                    kept: vec!["teh".into()],
                    touch: vec![(
                        'a',
                        KeyOffset {
                            dx: 0.25,
                            dy: -0.125,
                            samples: 3,
                        },
                    )],
                    ..lesson(&[("good", "mood")])
                }))
                .unwrap();
            let learned = record.state().unwrap();
            assert_eq!(learned.count("en", "good", "mood"), 2);
            assert_eq!(learned.count("en", "", "hello"), 1);
            assert!(learned.kept.contains(&"teh".to_owned()));
            assert_eq!(
                learned.touch_offsets()[&'a'],
                EngineOffset {
                    dx: 0.25,
                    dy: -0.125,
                    samples: 3
                }
            );

            record.apply(Learned::FORGET.invoke()).unwrap();
            assert!(record.state().unwrap().is_empty());
        }

        phase two_devices_learning_at_once_count_both;
        {
            let mut first = Replica::<Learned>::new();
            first.apply(learn(lesson(&[("good", "mood")]))).unwrap();
            let mut second = first.fork();
            first.apply(learn(lesson(&[("good", "mood")]))).unwrap();
            second.apply(learn(lesson(&[("good", "mood")]))).unwrap();
            first.sync_with(&mut second).unwrap();
            assert_eq!(first.state().unwrap().count("en", "good", "mood"), 3);
            assert_eq!(second.state().unwrap(), first.state().unwrap());
        }

        phase a_full_context_learns_no_new_words;
        {
            let mut record = Replica::<Learned>::new();
            let words: Vec<String> = (0..=MAX_LEARNED_WORDS_PER_CONTEXT)
                .map(|index| format!("word{index}"))
                .collect();
            let pairs: Vec<(&str, &str)> = words.iter().map(|word| ("the", word.as_str())).collect();
            record.apply(learn(lesson(&pairs))).unwrap();
            assert_eq!(
                record.state().unwrap().transitions["en"]["the"].len(),
                MAX_LEARNED_WORDS_PER_CONTEXT
            );
        }

        phase learning_is_no_undo_step_and_forgetting_is_one;
        {
            use ui::tilo_actions::{Action as _, ChangeKind};
            let learn = Learned::LEARN
                .with(learned::Learn {
                    lesson: lesson(&[("good", "mood")]),
                })
                .build(0)
                .unwrap();
            assert_eq!(learn.change_kind(), ChangeKind::External);
            let forget = Learned::FORGET.invoke().build(0).unwrap();
            assert_eq!(forget.change_kind(), ChangeKind::Content);
        }
    }
}
