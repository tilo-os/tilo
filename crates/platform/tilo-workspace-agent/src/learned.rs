//! What a person's agents learned about them: short facts, each with where it came from and
//! when, kept as the person's own record in the workspace.
//!
//! The record is each member's own there (`[resources] own` in Chat's manifest): it reaches
//! that person's devices and nobody else's, and only they, or software acting for them, read or
//! change it. Another member's agent is refused it by the rule that refuses the other member.
//! Chat keeps the kind, beside the workspace's agents, because it serves every agent a person
//! keeps there and not one app.
//!
//! Every turn of the person's agents is told the facts, and the agents write them through the
//! record's verbs: `learn`, `correct`, `forget`. The person changes the same list through the
//! same verbs wherever a view lists it, and asking an agent what it knows reads it too. Nothing
//! holds a second copy.

use tilo_resource::items::item_resource_id;
use tilo_resource::{ActionCx, Collection, Id, Model, NonBlank, Refusal, SearchEntry, resource};

/// The record's kind.
pub const KIND: &str = "chat.learned";

/// The most facts one person's record keeps.
pub const MAX_FACTS: usize = 200;

/// The most one fact says, in bytes: a sentence or two.
pub const MAX_FACT: usize = 400;

/// The most a fact's source says, in bytes.
pub const MAX_SOURCE: usize = 200;

/// One thing learned about the person.
#[derive(Model, Default, Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    /// What was learned, in a sentence or two.
    #[model(label)]
    pub text: String,
    /// Where it came from, in words: that the person said so, or the record or message it was
    /// read in.
    pub source: String,
    /// When it was learned or last corrected, in milliseconds since the Unix epoch.
    pub at: i64,
    /// The agent that wrote it down, by its id; `None` where the person wrote it themselves.
    pub by: Option<String>,
}

/// What a person's agents learned about them, oldest first.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Learned::valid)]
pub struct Learned {
    /// The facts: kept, corrected and forgotten only by the record's verbs, so each says where
    /// it came from and when.
    #[model(readonly)]
    pub facts: Collection<Fact>,
}

impl Learned {
    /// What search finds in the record: each fact by what it says, opening it at its item.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.facts
            .iter()
            .filter(|(_, fact)| !fact.text.trim().is_empty())
            .map(|(id, fact)| {
                SearchEntry::new(fact.text.trim(), "")
                    .anchored(item_resource_id::<Self>("facts", id))
            })
            .collect()
    }

    /// What the record holds: each fact with its source, its time and who wrote it down. It
    /// answers through [`fields`](tilo_resource::read::fields), so a caller with no window open
    /// reads it like any record, and only the person it belongs to is such a caller.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    fn valid(&self) -> Result<(), Refusal> {
        if self.facts.len() > MAX_FACTS {
            return Err(too_many());
        }
        for (_, fact) in self.facts.iter() {
            check_text(&fact.text)?;
            check_source(&fact.source)?;
            if fact.by.as_deref().is_some_and(|by| {
                by.is_empty() || by.len() > 256 || by.chars().any(char::is_control)
            }) {
                return Err(Refusal::new(
                    "a fact names the agent that wrote it down by its id",
                ));
            }
        }
        Ok(())
    }
}

#[resource(
    kind = "chat.learned",
    search = Learned::search,
    read_content = Learned::read_document
)]
impl Learned {
    /// Keep a fact.
    ///
    /// Something lasting about the person or how they want things done, in a sentence or two,
    /// with where it came from. A fact kept already is not kept twice: saying it again renews
    /// its source and its time.
    #[action]
    pub fn learn(
        &mut self,
        cx: &mut ActionCx,
        #[param("What was learned, in a sentence or two")] text: NonBlank,
        #[param(
            "Where it came from: that the person said so, or the record or message it was read in"
        )]
        source: NonBlank,
    ) -> Result<(), Refusal> {
        let text = clean_text(text.as_str())?;
        let source = clean_source(source.as_str())?;
        let known = self
            .facts
            .iter()
            .find(|(_, fact)| fact.text.eq_ignore_ascii_case(&text))
            .map(|(id, _)| id.clone());
        let id = if let Some(id) = known {
            id
        } else {
            if self.facts.len() >= MAX_FACTS {
                return Err(too_many());
            }
            let id = cx.id();
            self.facts.push(id.clone(), Fact::default());
            id
        };
        let fact = self.facts.get_mut(&id).ok_or_else(|| no_such(&id))?;
        fact.text = text;
        fact.source = source;
        fact.at = cx.now_ms();
        fact.by = writer(cx);
        Ok(())
    }

    /// Correct a fact.
    ///
    /// What it says now, and where the correction came from when that differs from where the
    /// fact came from.
    #[action(item = facts)]
    pub fn correct(
        &mut self,
        cx: &mut ActionCx,
        fact: &Id,
        #[param("What it says now, in a sentence or two")] text: NonBlank,
        #[param("Where the correction came from; none to keep where the fact came from")]
        source: Option<String>,
    ) -> Result<(), Refusal> {
        let text = clean_text(text.as_str())?;
        let source = match source.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(source) => Some(clean_source(source)?),
        };
        let found = self.facts.get_mut(fact).ok_or_else(|| no_such(fact))?;
        found.text = text;
        if let Some(source) = source {
            found.source = source;
        }
        found.at = cx.now_ms();
        found.by = writer(cx);
        Ok(())
    }

    /// Forget a fact.
    ///
    /// It is told to no later turn.
    #[action(item = facts)]
    pub fn forget(&mut self, fact: &Id) -> Result<(), Refusal> {
        self.facts
            .remove(fact)
            .map(drop)
            .ok_or_else(|| no_such(fact))
    }
}

/// The agent an action is by, by its id; `None` for a person acting themselves.
fn writer(cx: &ActionCx) -> Option<String> {
    cx.invoked_by()
        .and_then(|acting| acting.agent_id().map(str::to_owned))
}

fn no_such(fact: &Id) -> Refusal {
    Refusal::new(format!("there is no fact {fact}"))
}

fn too_many() -> Refusal {
    Refusal::new(format!(
        "at most {MAX_FACTS} facts are kept: forget one that no longer holds first"
    ))
}

fn clean_text(text: &str) -> Result<String, Refusal> {
    let text = text.trim().to_owned();
    check_text(&text)?;
    Ok(text)
}

fn check_text(text: &str) -> Result<(), Refusal> {
    if text.trim().is_empty() || text.len() > MAX_FACT {
        return Err(Refusal::new(format!(
            "a fact is a sentence or two, at most {MAX_FACT} bytes"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(Refusal::new("a fact is one line of text"));
    }
    Ok(())
}

fn clean_source(source: &str) -> Result<String, Refusal> {
    let source = source.trim().to_owned();
    check_source(&source)?;
    Ok(source)
}

fn check_source(source: &str) -> Result<(), Refusal> {
    if source.trim().is_empty() || source.len() > MAX_SOURCE {
        return Err(Refusal::new(format!(
            "a fact says where it came from, in at most {MAX_SOURCE} bytes"
        )));
    }
    if source.chars().any(char::is_control) {
        return Err(Refusal::new("a fact's source is one line of text"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tilo_resource::{Model as _, Replica};

    use super::*;

    const ADA: &str = "ada";
    const ASSISTANT: &str = "assistant";

    fn learn(text: &str, source: &str) -> tilo_resource::Invoke<Learned> {
        Learned::LEARN.with(learned::Learn {
            text: NonBlank::parse(text).expect("a fact"),
            source: NonBlank::parse(source).expect("a source"),
        })
    }

    fn correct(fact: &Id, text: &str, source: Option<&str>) -> tilo_resource::Invoke<Learned> {
        Learned::CORRECT.with(learned::Correct {
            fact: fact.clone(),
            text: NonBlank::parse(text).expect("a fact"),
            source: source.map(str::to_owned),
        })
    }

    fn forget(fact: &Id) -> tilo_resource::Invoke<Learned> {
        Learned::FORGET.with(learned::Forget { fact: fact.clone() })
    }

    fn facts(record: &Replica<Learned>) -> Vec<(Id, Fact)> {
        record
            .state()
            .unwrap()
            .facts
            .iter()
            .map(|(id, fact)| (id.clone(), fact.clone()))
            .collect()
    }

    /// The person's agent keeps what it learned with its source and its time, corrects it, and
    /// the person takes it away again: one list, written by both through the same verbs.
    #[test]
    fn a_fact_is_kept_with_its_source_corrected_and_forgotten() {
        let mut person = Replica::<Learned>::new().as_author(ADA);
        let mut agent = person.fork().as_agent_for(ASSISTANT, ADA);
        agent.set_now_ms(1_000);
        agent
            .apply(learn(
                "  Prefers answers in German.  ",
                "They said so in the conversation",
            ))
            .unwrap();
        person.sync_with(&mut agent).unwrap();
        let kept = facts(&person);
        let [(id, fact)] = kept.as_slice() else {
            panic!("one fact, not {kept:?}");
        };
        assert_eq!(
            fact,
            &Fact {
                text: "Prefers answers in German.".into(),
                source: "They said so in the conversation".into(),
                at: 1_000,
                by: Some(ASSISTANT.into()),
            }
        );
        // Said again, it is the fact it was, with its newer source and time.
        agent.set_now_ms(2_000);
        agent
            .apply(learn(
                "prefers answers in german.",
                "Their note \"About me\"",
            ))
            .unwrap();
        let again = facts(&agent);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].0, *id);
        assert_eq!(again[0].1.source, "Their note \"About me\"");
        assert_eq!(again[0].1.at, 2_000);
        // The person corrects it themselves: the record says they did.
        person.sync_with(&mut agent).unwrap();
        person.set_now_ms(3_000);
        person
            .apply(correct(id, "Prefers answers in English at work.", None))
            .unwrap();
        let corrected = facts(&person);
        assert_eq!(corrected[0].1.text, "Prefers answers in English at work.");
        assert_eq!(corrected[0].1.source, "Their note \"About me\"");
        assert_eq!(
            (corrected[0].1.at, corrected[0].1.by.clone()),
            (3_000, None)
        );
        person
            .apply(correct(
                id,
                "Prefers answers in English.",
                Some("They said so"),
            ))
            .unwrap();
        assert_eq!(facts(&person)[0].1.source, "They said so");
        // Forgotten, it is gone, and there is nothing left to correct or forget.
        person.apply(forget(id)).unwrap();
        assert_eq!(facts(&person), Vec::new());
        assert!(person.apply(forget(id)).is_err());
        assert!(person.apply(correct(id, "Anything.", None)).is_err());
    }

    /// A fact is one bounded line with a source, and the list is bounded.
    #[test]
    fn a_fact_is_one_bounded_line_and_the_list_is_bounded() {
        let mut record = Replica::<Learned>::new().as_author(ADA);
        for (text, source) in [
            ("x".repeat(MAX_FACT + 1), "They said so".to_owned()),
            ("two\nlines".to_owned(), "They said so".to_owned()),
            ("Likes tea.".to_owned(), "y".repeat(MAX_SOURCE + 1)),
        ] {
            assert!(record.apply(learn(&text, &source)).is_err(), "{text:?}");
        }
        assert_eq!(facts(&record), Vec::new());
        for n in 0..MAX_FACTS {
            record
                .apply(learn(&format!("Fact {n}."), "They said so"))
                .unwrap();
        }
        let refused = record
            .apply(learn("One more.", "They said so"))
            .unwrap_err();
        assert!(refused.to_string().contains("forget one"), "{refused}");
        // A fact kept already still renews at the limit.
        record.apply(learn("Fact 7.", "A newer source")).unwrap();
        assert_eq!(facts(&record).len(), MAX_FACTS);
    }

    /// What the record holds reads back as it was written, by its fields.
    #[test]
    fn the_record_reads_back_as_it_was_written() {
        let mut record = Replica::<Learned>::new().as_author(ADA);
        record.set_now_ms(1_700_000_000_000);
        record
            .apply(learn("Works from Graz.", "They said so"))
            .unwrap();
        let state = record.state().unwrap();
        let read = Learned::from_json(&state.to_json()).expect("the record reads");
        assert_eq!(read, state);
        let content = state.read_document(None).unwrap();
        assert!(
            content.to_string().contains("Works from Graz."),
            "{content}"
        );
        assert_eq!(state.search().len(), 1);
    }
}
