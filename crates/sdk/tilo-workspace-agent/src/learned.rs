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
//!
//! Each fact names the person it was learned about ([`Fact::about`]): the person who wrote it,
//! or whom the agent that wrote it acted for. Where one record holds what an agent learned from
//! several people (a server keeps one per workspace), that is its reader rule: a fact is read,
//! corrected and forgotten for the person it is about and nobody else ([`Fact::is_about`]).
//! What Ada taught her agent is not there for Bo's turn of the same agent, and what Bo teaches
//! it is his.

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
    /// The person it was learned about, by their person id: who wrote it down, or whom the
    /// agent that did acted for. `None` where the writer named no person: an agent writing in
    /// the person's own record on their device, which is about them and nobody else.
    pub about: Option<String>,
}

impl Fact {
    /// Whether this fact was learned about `person`: only then is it read, corrected or
    /// forgotten for them.
    #[must_use]
    pub fn is_about(&self, person: &str) -> bool {
        self.about.as_deref() == Some(person)
    }
}

/// What a person's agents learned about them, oldest first: each fact with its
/// source, its time and who wrote it down.
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
            if fact.about.as_deref().is_some_and(|about| {
                about.is_empty() || about.len() > 256 || about.chars().any(char::is_control)
            }) {
                return Err(Refusal::new(
                    "a fact names the person it is about by their person id",
                ));
            }
        }
        Ok(())
    }
}

#[resource(
    kind = "chat.learned",
    search = Learned::search,
    read_content = tilo_resource::read::fields
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
        let about = about(cx);
        let known = self
            .facts
            .iter()
            .find(|(_, fact)| fact.about == about && fact.text.eq_ignore_ascii_case(&text))
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
        fact.about = about;
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
        let about = about(cx);
        let found = self
            .facts
            .get_mut(fact)
            .filter(|found| found.about == about)
            .ok_or_else(|| no_such(fact))?;
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
    pub fn forget(&mut self, cx: &mut ActionCx, fact: &Id) -> Result<(), Refusal> {
        let about = about(cx);
        if self
            .facts
            .get(fact)
            .is_none_or(|found| found.about != about)
        {
            return Err(no_such(fact));
        }
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

/// The person an action is for: who invoked it, or whom the agent that did acts for. `None`
/// for a writer that names no person, as an agent does in a person's own record on their
/// device, which is theirs.
fn about(cx: &ActionCx) -> Option<String> {
    let invoked = cx
        .invoked_by()
        .and_then(|acting| acting.people().first().map(|person| (*person).to_owned()));
    invoked
        .or_else(|| {
            cx.author()
                .and_then(|author| author.person_id())
                .map(str::to_owned)
        })
        .filter(|person| !person.is_empty())
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
