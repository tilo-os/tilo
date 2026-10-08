//! What the keyboard learned from one person's typing: the words they
//! commit after which words, the words they kept as typed when corrected,
//! and where they tend to hit each letter.
//!
//! It is the person's own resource ([`ui::framework::PersonRecord`]): it
//! follows them to their other devices and never reaches a collaborator.
//! Nothing typed into a secret field, and nothing at all while
//! personalization is off, is learned.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ChangeKind, Doc, ExecutionLocus};
use ui::tilo_resource::{ModelError, Resource};

use crate::autocorrect::MAX_KEPT_WORDS;
use crate::predict::{MAX_LEARNED_CONTEXTS, MAX_LEARNED_WORDS_PER_CONTEXT};
use crate::touch_model::{KeyOffset, MAX_LEARNED_KEYS};

const TRANSITIONS: &str = "transitions";
const KEPT: &str = "kept";
const TOUCH: &str = "touch";
const ACTION_TYPE: &str = "osk.learned";

/// What the keyboard learned, as one person's record holds it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LearnedTyping {
    /// Per language: the previous word (empty at a start) → a word committed
    /// after it → how often.
    pub transitions: BTreeMap<String, BTreeMap<String, BTreeMap<String, u32>>>,
    /// Words kept as typed when the keyboard corrected them.
    pub kept: BTreeSet<String>,
    /// Where the person tends to hit each letter.
    pub touch: BTreeMap<char, KeyOffset>,
}

impl LearnedTyping {
    /// Whether nothing was learned.
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.kept.is_empty() && self.touch.is_empty()
    }
}

/// What typing taught the keyboard since it last saved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    /// Whether it teaches nothing.
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty() && self.kept.is_empty() && self.touch.is_empty()
    }
}

/// An edit of what the keyboard learned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearnedAction {
    /// Remember what typing taught.
    Learn(Lesson),
    /// Forget everything learned.
    Forget,
}

impl Action for LearnedAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::Learn(lesson) => learn(doc, lesson),
            Self::Forget => {
                for key in [TRANSITIONS, KEPT, TOUCH] {
                    if doc.get(ROOT, key)?.is_some() {
                        doc.delete(ROOT, key)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// Learning comes from use, so no undo takes it back; forgetting is the
    /// person's own step.
    fn change_kind(&self) -> ChangeKind {
        match self {
            Self::Learn(_) => ChangeKind::External,
            Self::Forget => ChangeKind::Content,
        }
    }
}

fn map_at(doc: &mut Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    match doc.get(parent, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(parent, key, ObjType::Map)?),
    }
}

fn learn(doc: &mut Doc, lesson: &Lesson) -> Result<(), ActionError> {
    if !lesson.transitions.is_empty() {
        let languages = map_at(doc, &ROOT, TRANSITIONS)?;
        let contexts = map_at(doc, &languages, &lesson.language)?;
        for (previous, word) in &lesson.transitions {
            let known_context = doc.get(&contexts, previous)?.is_some();
            if !known_context && doc.length(&contexts) >= MAX_LEARNED_CONTEXTS {
                continue;
            }
            let words = map_at(doc, &contexts, previous)?;
            match doc.get(&words, word)? {
                Some((Value::Scalar(count), _)) if matches!(*count, ScalarValue::Counter(_)) => {
                    doc.increment(&words, word, 1)?;
                }
                _ if doc.length(&words) >= MAX_LEARNED_WORDS_PER_CONTEXT => {}
                _ => doc.put(&words, word, ScalarValue::counter(1))?,
            }
        }
    }
    if !lesson.kept.is_empty() {
        let kept = map_at(doc, &ROOT, KEPT)?;
        for word in &lesson.kept {
            if doc.get(&kept, word)?.is_none() && doc.length(&kept) < MAX_KEPT_WORDS {
                doc.put(&kept, word, true)?;
            }
        }
    }
    if !lesson.touch.is_empty() {
        let touch = map_at(doc, &ROOT, TOUCH)?;
        for (letter, offset) in &lesson.touch {
            let key = letter.to_string();
            if doc.get(&touch, &key)?.is_none() && doc.length(&touch) >= MAX_LEARNED_KEYS {
                continue;
            }
            let entry = doc.put_object(&touch, &key, ObjType::Map)?;
            doc.put(&entry, "dx", f64::from(offset.dx))?;
            doc.put(&entry, "dy", f64::from(offset.dy))?;
            doc.put(&entry, "samples", u64::from(offset.samples))?;
        }
    }
    Ok(())
}

fn map(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn scalar(doc: &Doc, parent: &ObjId, key: &str) -> Option<ScalarValue> {
    match doc.get(parent, key).ok().flatten() {
        Some((Value::Scalar(value), _)) => Some(value.into_owned()),
        _ => None,
    }
}

fn number(value: Option<ScalarValue>) -> Option<f64> {
    match value? {
        ScalarValue::F64(number) => Some(number),
        ScalarValue::Int(number) => Some(number as f64),
        ScalarValue::Uint(number) => Some(number as f64),
        _ => None,
    }
}

/// What the keyboard learned from one person's typing.
pub struct Learned;

impl Resource for Learned {
    const KIND: &'static str = "osk.learned";
    const LABEL: &'static str = "What the keyboard learned";
    const ACTION_TYPE: &'static str = ACTION_TYPE;

    type State = LearnedTyping;
    type Action = LearnedAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<LearnedAction>(ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<LearnedTyping, ModelError> {
        let mut learned = LearnedTyping::default();
        if let Some(languages) = map(doc, &ROOT, TRANSITIONS) {
            for language in doc.keys(&languages) {
                let Some(contexts) = map(doc, &languages, &language) else {
                    continue;
                };
                let by_context = learned.transitions.entry(language).or_default();
                for previous in doc.keys(&contexts) {
                    let Some(words) = map(doc, &contexts, &previous) else {
                        continue;
                    };
                    let counts = by_context.entry(previous).or_default();
                    for word in doc.keys(&words) {
                        if let Some(ScalarValue::Counter(count)) = scalar(doc, &words, &word) {
                            let count = u32::try_from(i64::from(&count)).unwrap_or(u32::MAX);
                            counts.insert(word, count);
                        }
                    }
                }
            }
        }
        if let Some(kept) = map(doc, &ROOT, KEPT) {
            learned.kept = doc.keys(&kept).collect();
        }
        if let Some(touch) = map(doc, &ROOT, TOUCH) {
            for key in doc.keys(&touch) {
                let (Some(letter), Some(entry)) = (key.chars().next(), map(doc, &touch, &key))
                else {
                    continue;
                };
                let (Some(dx), Some(dy)) = (
                    number(scalar(doc, &entry, "dx")),
                    number(scalar(doc, &entry, "dy")),
                ) else {
                    continue;
                };
                let samples = number(scalar(doc, &entry, "samples")).unwrap_or(1.0);
                learned.touch.insert(
                    letter,
                    KeyOffset {
                        dx: dx as f32,
                        dy: dy as f32,
                        samples: samples as u32,
                    },
                );
            }
        }
        Ok(learned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn lessons_add_up_and_forgetting_leaves_nothing() {
        let mut doc = Doc::new();
        LearnedAction::Learn(lesson(&[("good", "mood"), ("", "hello")]))
            .apply(&mut doc)
            .unwrap();
        LearnedAction::Learn(Lesson {
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
        })
        .apply(&mut doc)
        .unwrap();
        let learned = Learned::read(&doc).unwrap();
        assert_eq!(learned.transitions["en"]["good"]["mood"], 2);
        assert_eq!(learned.transitions["en"][""]["hello"], 1);
        assert!(learned.kept.contains("teh"));
        assert_eq!(
            learned.touch[&'a'],
            KeyOffset {
                dx: 0.25,
                dy: -0.125,
                samples: 3
            }
        );

        LearnedAction::Forget.apply(&mut doc).unwrap();
        assert!(Learned::read(&doc).unwrap().is_empty());
    }

    #[test]
    fn two_devices_learning_at_once_count_both() {
        let mut first = Doc::new();
        LearnedAction::Learn(lesson(&[("good", "mood")]))
            .apply(&mut first)
            .unwrap();
        let mut second = first.fork();
        LearnedAction::Learn(lesson(&[("good", "mood")]))
            .apply(&mut first)
            .unwrap();
        LearnedAction::Learn(lesson(&[("good", "mood")]))
            .apply(&mut second)
            .unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut first, &mut second).unwrap();
        assert_eq!(
            Learned::read(&first).unwrap().transitions["en"]["good"]["mood"],
            3
        );
    }

    #[test]
    fn a_full_context_learns_no_new_words() {
        let mut doc = Doc::new();
        let words: Vec<String> = (0..=MAX_LEARNED_WORDS_PER_CONTEXT)
            .map(|index| format!("word{index}"))
            .collect();
        let pairs: Vec<(&str, &str)> = words.iter().map(|word| ("the", word.as_str())).collect();
        LearnedAction::Learn(lesson(&pairs))
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            Learned::read(&doc).unwrap().transitions["en"]["the"].len(),
            MAX_LEARNED_WORDS_PER_CONTEXT
        );
    }
}
