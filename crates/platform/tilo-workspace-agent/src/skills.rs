//! How kinds of work are done for one person: their skills, written down by their agents and
//! by themselves, kept as the person's own resource.
//!
//! A skill says what kind of work it is for, how that work is done here, the form its result
//! takes, results the person accepted and rejected as examples, and, where the person wants
//! one, the model the work is done with. An agent writes one down after it did a kind of work
//! and was corrected, and whenever its person says "from now on, do it like this"; "more like
//! this" and "less like this" on a result keep that result as an example of the skill that
//! made it. So a kind of work nobody built anything for is done the person's way the next
//! time, by a turn that never saw the first.
//!
//! The resource is the skills' one home. It is changed only through its verbs (`write-skill`,
//! `correct-skill`, `keep-example`, `drop-example`, `forget-skill`), by the person or by an
//! agent acting for them, and each skill says who wrote it last and when. It is the person's
//! own: whoever keeps it answers the person and the person's own agents, and nobody else.
//!
//! A skill names work and how to do it. It decides nothing about what an agent may do: the
//! resources the work touches carry the permissions, and a skill never widens them.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use tilo_resource::items::item_resource_id;
use tilo_resource::{
    ActionCx, Collection, Id, JsonSchema, Model, NonBlank, Refusal, SearchEntry, resource,
};

/// The resource's kind.
pub const KIND: &str = "chat.skills";

/// The most skills one person keeps.
pub const MAX_SKILLS: usize = 200;

/// The longest a skill's name is, in characters.
pub const MAX_NAME: usize = 80;

/// The most a skill says about the kind of work it is for, in bytes: a sentence or two.
pub const MAX_WORK: usize = 400;

/// The most a skill says about how its work is done, in bytes.
pub const MAX_HOW: usize = 4 * 1024;

/// The most a skill says about the form of its result, in bytes.
pub const MAX_FORM: usize = 1024;

/// The most examples one skill keeps: the newest stay.
pub const MAX_EXAMPLES: usize = 12;

/// The most one example holds, in bytes.
pub const MAX_EXAMPLE: usize = 1024;

/// The most a person's words about an example hold, in bytes.
pub const MAX_NOTE: usize = 200;

/// How many words of five letters or more a job shares with a skill before the skill is
/// taken to fit it ([`Skill::fits`]).
const FIT_WORDS: usize = 2;

/// The shortest word that counts when a job is held against a skill, and how many of its
/// letters are compared.
const FIT_LETTERS: usize = 5;

/// The most of one example an agent is told, in bytes.
const EXAMPLE_TOLD: usize = 600;

/// A result the person accepted or rejected, kept as an example of its skill.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Example {
    /// Its id among the examples of its skill.
    pub id: String,
    /// Whether the person accepted it (more like this) or rejected it (less like this).
    pub accepted: bool,
    /// The result, or the part of it that is meant.
    pub text: String,
    /// What the person said about it; empty when they said nothing more.
    pub note: String,
    /// When it was kept, in milliseconds since the Unix epoch.
    pub at: i64,
}

/// How one kind of work is done for the person.
#[derive(Model, Default, Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    /// What it is called, in a few words: what an agent names it by.
    #[model(label)]
    pub name: String,
    /// The kind of work it is for, in a sentence or two: when it applies.
    pub work: String,
    /// How that work is done here.
    pub how: String,
    /// The form the result takes; empty when it has no fixed form.
    pub form: String,
    /// The model the work is done with, as `provider/model`; `None` follows the person's
    /// choice for the kind of work.
    pub model: Option<String>,
    /// Results the person accepted and rejected, oldest first ([`MAX_EXAMPLES`]).
    pub examples: Vec<Example>,
    /// When it was written or last changed, in milliseconds since the Unix epoch.
    pub at: i64,
    /// The agent that changed it last, by its id; `None` where the person did.
    pub by: Option<String>,
}

impl Skill {
    /// Whether this skill fits `job`, by its words: the job names the skill, or shares
    /// [`FIT_WORDS`] words of [`FIT_LETTERS`] letters or more with what the skill is called
    /// and what it is for. A first sieve by words, not by meaning: whoever tells an agent its
    /// skills names the others too, so the agent reads one this sieve let through wrongly, or
    /// missed, for itself.
    #[must_use]
    pub fn fits(&self, job: &str) -> bool {
        let name = self.name.trim().to_lowercase();
        if !name.is_empty() && job.to_lowercase().contains(&name) {
            return true;
        }
        let own = words(&format!("{} {}", self.name, self.work));
        own.intersection(&words(job)).count() >= FIT_WORDS
    }

    /// The skill as an agent is told it, in at most `most` bytes: what it is called and for,
    /// how the work is done, the form of the result, then the newest examples the person
    /// accepted and rejected, as many as fit.
    #[must_use]
    pub fn told(&self, most: usize) -> String {
        let mut told = format!(
            "Skill: {}\nFor: {}\nHow it is done here:\n{}",
            self.name.trim(),
            self.work.trim(),
            self.how.trim()
        );
        if !self.form.trim().is_empty() {
            let _ = write!(told, "\nThe form of the result:\n{}", self.form.trim());
        }
        for (accepted, heading) in [
            (true, "\nResults this person accepted (more like these):"),
            (false, "\nResults this person rejected (less like these):"),
        ] {
            let mut headed = false;
            for example in self
                .examples
                .iter()
                .rev()
                .filter(|example| example.accepted == accepted)
            {
                let mut line = format!("\n- {}", cut(example.text.trim(), EXAMPLE_TOLD));
                if !example.note.trim().is_empty() {
                    let _ = write!(line, " (they said: {})", example.note.trim());
                }
                let head = if headed { "" } else { heading };
                let longer = told
                    .len()
                    .saturating_add(head.len())
                    .saturating_add(line.len());
                if longer > most {
                    break;
                }
                told.push_str(head);
                told.push_str(&line);
                headed = true;
            }
        }
        cut(&told, most).to_owned()
    }
}

/// How kinds of work are done for one person, oldest skill first.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Skills::valid)]
pub struct Skills {
    /// The skills: written, corrected and forgotten only by the resource's verbs, so each
    /// says who changed it last and when.
    #[model(readonly)]
    pub skills: Collection<Skill>,
}

impl Skills {
    /// The skill `named` names: by its id, or by what it is called, whatever the case.
    #[must_use]
    pub fn named(&self, named: &str) -> Option<(Id, &Skill)> {
        let named = named.trim();
        self.skills
            .iter()
            .find(|(id, _)| id.as_str() == named)
            .or_else(|| {
                self.skills
                    .iter()
                    .find(|(_, skill)| same_name(&skill.name, named))
            })
            .map(|(id, skill)| (id.clone(), skill))
    }

    /// What search finds in the resource: each skill by what it is called and what it is for,
    /// opening it at its item.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.skills
            .iter()
            .filter(|(_, skill)| !skill.name.trim().is_empty())
            .map(|(id, skill)| {
                SearchEntry::new(skill.name.trim(), skill.work.trim()).anchored(item_resource_id::<
                    Self,
                >(
                    "skills", id
                ))
            })
            .collect()
    }

    /// What the resource holds: each skill with what it is for, how it is done, the form of
    /// its result, its examples, its model and who changed it last. It answers through
    /// [`fields`](tilo_resource::read::fields), so a caller with no window open reads it like
    /// any resource, and only the person it belongs to is such a caller.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    fn valid(&self) -> Result<(), Refusal> {
        if self.skills.len() > MAX_SKILLS {
            return Err(too_many());
        }
        for (_, skill) in self.skills.iter() {
            check_name(&skill.name)?;
            check_line(&skill.work, MAX_WORK, "what a skill is for")?;
            check_text(&skill.how, MAX_HOW, "how a skill's work is done")?;
            if !skill.form.is_empty() {
                check_text(&skill.form, MAX_FORM, "the form of a skill's result")?;
            }
            if let Some(model) = &skill.model {
                crate::check_model(model)?;
            }
            if skill.examples.len() > MAX_EXAMPLES {
                return Err(Refusal::new(format!(
                    "a skill keeps at most {MAX_EXAMPLES} examples"
                )));
            }
            for example in &skill.examples {
                check_text(&example.text, MAX_EXAMPLE, "an example")?;
                if !example.note.is_empty() {
                    check_line(&example.note, MAX_NOTE, "what was said about an example")?;
                }
            }
            if skill.by.as_deref().is_some_and(|by| {
                by.is_empty() || by.len() > 256 || by.chars().any(char::is_control)
            }) {
                return Err(Refusal::new(
                    "a skill names the agent that changed it last by its id",
                ));
            }
        }
        Ok(())
    }
}

#[resource(
    kind = "chat.skills",
    search = Skills::search,
    read_content = Skills::read_document
)]
impl Skills {
    /// Write a skill down.
    ///
    /// How a kind of work is done for this person: what it is called, the kind of work it is
    /// for, how that work is done here, and where it has one the form of the result and the
    /// model the work is done with. A skill of that name is written again, not twice: what
    /// it says is replaced and its examples stay.
    #[action]
    pub fn write_skill(
        &mut self,
        cx: &mut ActionCx,
        #[param("What it is called, in a few words")] name: NonBlank,
        #[param("The kind of work it is for, in a sentence or two: when it applies")]
        work: NonBlank,
        #[param("How that work is done here, step by step where that helps")] how: String,
        #[param("The form the result takes; none when it has no fixed form")] form: Option<String>,
        #[param(
            "The model the work is done with, as provider/model; none for the person's choice for the kind of work"
        )]
        model: Option<String>,
    ) -> Result<(), Refusal> {
        let name = clean_name(name.as_str())?;
        let work = clean_line(work.as_str(), MAX_WORK, "what a skill is for")?;
        let how = clean_text(&how, MAX_HOW, "how a skill's work is done")?;
        let form = match form.as_deref().map(str::trim) {
            None | Some("") => String::new(),
            Some(form) => clean_text(form, MAX_FORM, "the form of a skill's result")?,
        };
        let model = crate::chosen_model(model)?;
        let known = self
            .skills
            .iter()
            .find(|(_, skill)| same_name(&skill.name, &name))
            .map(|(id, _)| id.clone());
        let id = if let Some(id) = known {
            id
        } else {
            if self.skills.len() >= MAX_SKILLS {
                return Err(too_many());
            }
            let id = cx.id();
            self.skills.push(id.clone(), Skill::default());
            id
        };
        let skill = self.skills.get_mut(&id).ok_or_else(|| no_such(&id))?;
        skill.name = name;
        skill.work = work;
        skill.how = how;
        skill.form = form;
        skill.model = model;
        skill.at = cx.now_ms();
        skill.by = writer(cx);
        Ok(())
    }

    /// Correct a skill.
    ///
    /// Only what is named changes: what it is called, the kind of work it is for, how the
    /// work is done, the form of the result, the model. An empty form or model takes it
    /// away. Its examples stay.
    #[action(item = skills)]
    pub fn correct_skill(
        &mut self,
        cx: &mut ActionCx,
        skill: &Id,
        #[param("What it is called now; none to keep its name")] name: Option<String>,
        #[param("The kind of work it is for now; none to keep that")] work: Option<String>,
        #[param("How the work is done now; none to keep that")] how: Option<String>,
        #[param("The form the result takes now, empty for no fixed form; none to keep that")]
        form: Option<String>,
        #[param(
            "The model the work is done with now, as provider/model, empty for the person's choice; none to keep that"
        )]
        model: Option<String>,
    ) -> Result<(), Refusal> {
        if name.is_none() && work.is_none() && how.is_none() && form.is_none() && model.is_none() {
            return Err(Refusal::new("a correction says what changes"));
        }
        let name = name.as_deref().map(clean_name).transpose()?;
        if let Some(name) = &name
            && self
                .skills
                .iter()
                .any(|(id, other)| id != skill && same_name(&other.name, name))
        {
            return Err(Refusal::new(format!(
                "there is already a skill called {name:?}"
            )));
        }
        let work = work
            .as_deref()
            .map(|work| clean_line(work, MAX_WORK, "what a skill is for"))
            .transpose()?;
        let how = how
            .as_deref()
            .map(|how| clean_text(how, MAX_HOW, "how a skill's work is done"))
            .transpose()?;
        let form = match form.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(String::new()),
            Some(form) => Some(clean_text(form, MAX_FORM, "the form of a skill's result")?),
        };
        let model = match model {
            None => None,
            Some(model) => Some(crate::chosen_model(Some(model))?),
        };
        let found = self.skills.get_mut(skill).ok_or_else(|| no_such(skill))?;
        if let Some(name) = name {
            found.name = name;
        }
        if let Some(work) = work {
            found.work = work;
        }
        if let Some(how) = how {
            found.how = how;
        }
        if let Some(form) = form {
            found.form = form;
        }
        if let Some(model) = model {
            found.model = model;
        }
        found.at = cx.now_ms();
        found.by = writer(cx);
        Ok(())
    }

    /// Keep an example.
    ///
    /// A result the person accepted (more like this) or rejected (less like this), kept with
    /// the skill that made it and what they said about it. The same result said again
    /// changes the example it is. The newest examples stay.
    #[action(item = skills)]
    pub fn keep_example(
        &mut self,
        cx: &mut ActionCx,
        skill: &Id,
        #[param("The result, or the part of it that is meant")] text: String,
        #[param("Whether the person accepted it (more like this) or rejected it (less like this)")]
        accepted: bool,
        #[param("What the person said about it; none when they said nothing more")] note: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let text = clean_text(&text, MAX_EXAMPLE, "an example")?;
        let note = match note.as_deref().map(str::trim) {
            None | Some("") => String::new(),
            Some(note) => clean_line(note, MAX_NOTE, "what was said about an example")?,
        };
        let (id, now, by) = (cx.id().to_string(), cx.now_ms(), writer(cx));
        let found = self.skills.get_mut(skill).ok_or_else(|| no_such(skill))?;
        // Said again, it is the example it was, placed newest.
        let known = found
            .examples
            .iter()
            .position(|example| example.text == text)
            .map(|at| found.examples.remove(at));
        found.examples.push(Example {
            id: known.map_or(id, |known| known.id),
            accepted,
            text,
            note,
            at: now,
        });
        while found.examples.len() > MAX_EXAMPLES {
            found.examples.remove(0);
        }
        found.at = now;
        found.by = by;
        Ok(())
    }

    /// Take an example out of a skill.
    #[action(item = skills)]
    pub fn drop_example(
        &mut self,
        cx: &mut ActionCx,
        skill: &Id,
        #[param("The example, by its id")] example: String,
    ) -> Result<(), Refusal> {
        let found = self.skills.get_mut(skill).ok_or_else(|| no_such(skill))?;
        let at = found
            .examples
            .iter()
            .position(|kept| kept.id == example.trim())
            .ok_or_else(|| Refusal::new(format!("that skill keeps no example {example:?}")))?;
        found.examples.remove(at);
        found.at = cx.now_ms();
        found.by = writer(cx);
        Ok(())
    }

    /// Forget a skill.
    ///
    /// It is told to no later turn, and its examples go with it.
    #[action(item = skills)]
    pub fn forget_skill(&mut self, skill: &Id) -> Result<(), Refusal> {
        self.skills
            .remove(skill)
            .map(drop)
            .ok_or_else(|| no_such(skill))
    }
}

/// The agent an action is by, by its id; `None` for a person acting themselves.
fn writer(cx: &ActionCx) -> Option<String> {
    cx.invoked_by()
        .and_then(|acting| acting.agent_id().map(str::to_owned))
}

/// Whether two skills are called the same, whatever the case.
fn same_name(one: &str, other: &str) -> bool {
    one.trim().to_lowercase() == other.trim().to_lowercase()
}

/// The words of `text` that count when a job is held against a skill: each word of
/// [`FIT_LETTERS`] letters or more, by its first [`FIT_LETTERS`] in lower case, so a word
/// fits in its other forms too (analysis and analyse, report and reporting).
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= FIT_LETTERS)
        .map(|word| word.to_lowercase().chars().take(FIT_LETTERS).collect())
        .collect()
}

/// `text`, to at most `most` bytes, ended between two characters.
fn cut(text: &str, most: usize) -> &str {
    if text.len() <= most {
        return text;
    }
    let mut end = most;
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

fn no_such(skill: &Id) -> Refusal {
    Refusal::new(format!("there is no skill {skill}"))
}

fn too_many() -> Refusal {
    Refusal::new(format!(
        "at most {MAX_SKILLS} skills are kept: forget one that is no longer used first"
    ))
}

fn clean_name(name: &str) -> Result<String, Refusal> {
    let name = name.trim().to_owned();
    check_name(&name)?;
    Ok(name)
}

fn check_name(name: &str) -> Result<(), Refusal> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME {
        return Err(Refusal::new(format!(
            "a skill's name is 1 to {MAX_NAME} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Refusal::new("a skill's name is one line of text"));
    }
    Ok(())
}

fn clean_line(line: &str, most: usize, what: &str) -> Result<String, Refusal> {
    let line = line.trim().to_owned();
    check_line(&line, most, what)?;
    Ok(line)
}

/// One line of text that says something, in at most `most` bytes.
fn check_line(line: &str, most: usize, what: &str) -> Result<(), Refusal> {
    if line.trim().is_empty() || line.len() > most {
        return Err(Refusal::new(format!(
            "{what} says something, in at most {most} bytes"
        )));
    }
    if line.chars().any(char::is_control) {
        return Err(Refusal::new(format!("{what} is one line of text")));
    }
    Ok(())
}

fn clean_text(text: &str, most: usize, what: &str) -> Result<String, Refusal> {
    let text = text.trim().to_owned();
    check_text(&text, most, what)?;
    Ok(text)
}

/// Text that says something, in at most `most` bytes: lines and tabs, no other control.
fn check_text(text: &str, most: usize, what: &str) -> Result<(), Refusal> {
    if text.trim().is_empty() || text.len() > most {
        return Err(Refusal::new(format!(
            "{what} says something, in at most {most} bytes"
        )));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Refusal::new(format!("{what} is plain text")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tilo_resource::{Model as _, Replica};

    use super::*;

    const ADA: &str = "ada";
    const ASSISTANT: &str = "assistant";

    fn write(
        name: &str,
        how: &str,
        form: Option<&str>,
        model: Option<&str>,
    ) -> tilo_resource::Invoke<Skills> {
        Skills::WRITE_SKILL.with(skills::WriteSkill {
            name: NonBlank::parse(name).expect("a name"),
            work: NonBlank::parse("Answering a request that asks for a weekly report")
                .expect("what it is for"),
            how: how.to_owned(),
            form: form.map(str::to_owned),
            model: model.map(str::to_owned),
        })
    }

    fn correct(skill: &Id) -> skills::CorrectSkill {
        skills::CorrectSkill {
            skill: skill.clone(),
            name: None,
            work: None,
            how: None,
            form: None,
            model: None,
        }
    }

    fn example(
        skill: &Id,
        text: &str,
        accepted: bool,
        note: Option<&str>,
    ) -> tilo_resource::Invoke<Skills> {
        Skills::KEEP_EXAMPLE.with(skills::KeepExample {
            skill: skill.clone(),
            text: text.to_owned(),
            accepted,
            note: note.map(str::to_owned),
        })
    }

    fn kept(resource: &Replica<Skills>) -> Vec<(Id, Skill)> {
        resource
            .state()
            .unwrap()
            .skills
            .iter()
            .map(|(id, skill)| (id.clone(), skill.clone()))
            .collect()
    }

    /// The person's agent writes a skill down and the person corrects it: one list, changed by
    /// both through the same verbs, and each skill says who changed it last and when.
    #[test]
    fn a_skill_is_written_by_an_agent_and_corrected_by_its_person() {
        let mut person = Replica::<Skills>::new().as_author(ADA);
        let mut agent = person.fork().as_agent_for(ASSISTANT, ADA);
        agent.set_now_ms(1_000);
        agent
            .apply(write(
                "  Weekly report  ",
                "Read the numbers from the sheet.\nSay what moved first.",
                Some("Three short paragraphs"),
                Some("Anthropic/claude-research"),
            ))
            .unwrap();
        person.sync_with(&mut agent).unwrap();
        let written = kept(&person);
        let [(id, skill)] = written.as_slice() else {
            panic!("one skill, not {written:?}");
        };
        assert_eq!(
            skill,
            &Skill {
                name: "Weekly report".into(),
                work: "Answering a request that asks for a weekly report".into(),
                how: "Read the numbers from the sheet.\nSay what moved first.".into(),
                form: "Three short paragraphs".into(),
                model: Some("Anthropic/claude-research".into()),
                examples: Vec::new(),
                at: 1_000,
                by: Some(ASSISTANT.into()),
            }
        );
        // Written again under its name, whatever the case, it is the skill it was.
        agent.set_now_ms(2_000);
        agent
            .apply(write("weekly REPORT", "Say what moved first.", None, None))
            .unwrap();
        let again = kept(&agent);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].0, *id);
        assert_eq!(
            (
                again[0].1.how.as_str(),
                again[0].1.form.as_str(),
                &again[0].1.model
            ),
            ("Say what moved first.", "", &None)
        );
        // The person corrects it themselves: only what they name changes, and it says they did.
        person.sync_with(&mut agent).unwrap();
        person.set_now_ms(3_000);
        person
            .apply(Skills::CORRECT_SKILL.with(skills::CorrectSkill {
                how: Some("Say what moved first, in numbers.".into()),
                model: Some("Anthropic/claude-research".into()),
                ..correct(id)
            }))
            .unwrap();
        let corrected = kept(&person);
        assert_eq!(corrected[0].1.name, "weekly REPORT");
        assert_eq!(corrected[0].1.how, "Say what moved first, in numbers.");
        assert_eq!(
            corrected[0].1.model.as_deref(),
            Some("Anthropic/claude-research")
        );
        assert_eq!(
            (corrected[0].1.at, corrected[0].1.by.clone()),
            (3_000, None)
        );
        // An empty model takes it away again, and a correction that names nothing is refused.
        person
            .apply(Skills::CORRECT_SKILL.with(skills::CorrectSkill {
                model: Some(String::new()),
                ..correct(id)
            }))
            .unwrap();
        assert_eq!(kept(&person)[0].1.model, None);
        assert!(
            person
                .apply(Skills::CORRECT_SKILL.with(correct(id)))
                .is_err()
        );
    }

    /// More like this, less like this: a result the person accepted or rejected is an example
    /// of the skill, newest last, and the same result said again is the example it was. The
    /// person takes one out, then the whole skill, and nothing is left to change.
    #[test]
    fn results_accepted_and_rejected_are_a_skills_examples_until_it_is_forgotten() {
        let mut person = Replica::<Skills>::new().as_author(ADA);
        let mut agent = person.fork().as_agent_for(ASSISTANT, ADA);
        person
            .apply(write("Weekly report", "Say what moved first.", None, None))
            .unwrap();
        let written = kept(&person);
        let id = &written[0].0;
        agent.sync_with(&mut person).unwrap();
        agent.set_now_ms(4_000);
        agent
            .apply(example(id, "Revenue rose 4 %, churn fell.", true, None))
            .unwrap();
        agent
            .apply(example(
                id,
                "This week was a week of many things.",
                false,
                Some("Too vague"),
            ))
            .unwrap();
        agent
            .apply(example(
                id,
                "Revenue rose 4 %, churn fell.",
                true,
                Some("Exactly this"),
            ))
            .unwrap();
        let examples = kept(&agent)[0].1.examples.clone();
        assert_eq!(
            examples
                .iter()
                .map(|example| (
                    example.text.as_str(),
                    example.accepted,
                    example.note.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                ("This week was a week of many things.", false, "Too vague"),
                ("Revenue rose 4 %, churn fell.", true, "Exactly this"),
            ]
        );
        assert_eq!(kept(&agent)[0].1.by.as_deref(), Some(ASSISTANT));
        // The person takes one example out, then the whole skill.
        person.sync_with(&mut agent).unwrap();
        person
            .apply(Skills::DROP_EXAMPLE.with(skills::DropExample {
                skill: id.clone(),
                example: examples[0].id.clone(),
            }))
            .unwrap();
        assert_eq!(kept(&person)[0].1.examples.len(), 1);
        assert!(
            person
                .apply(Skills::DROP_EXAMPLE.with(skills::DropExample {
                    skill: id.clone(),
                    example: examples[0].id.clone(),
                }))
                .is_err()
        );
        person
            .apply(Skills::FORGET_SKILL.with(skills::ForgetSkill { skill: id.clone() }))
            .unwrap();
        assert_eq!(kept(&person), Vec::new());
        assert!(
            person
                .apply(Skills::FORGET_SKILL.with(skills::ForgetSkill { skill: id.clone() }))
                .is_err()
        );
        assert!(person.apply(example(id, "Anything.", true, None)).is_err());
    }

    /// What a skill says is bounded, its model is named as provider/model, two skills are
    /// never called the same, a skill keeps its newest examples, and the list is bounded.
    #[test]
    fn a_skill_is_bounded_and_so_is_the_list() {
        let mut resource = Replica::<Skills>::new().as_author(ADA);
        for (name, how, form, model) in [
            ("x".repeat(MAX_NAME + 1), "Do it.".to_owned(), None, None),
            ("two\nlines".to_owned(), "Do it.".to_owned(), None, None),
            ("Report".to_owned(), "  ".to_owned(), None, None),
            ("Report".to_owned(), "y".repeat(MAX_HOW + 1), None, None),
            (
                "Report".to_owned(),
                "Do it.".to_owned(),
                Some("z".repeat(MAX_FORM + 1)),
                None,
            ),
            (
                "Report".to_owned(),
                "Do it.".to_owned(),
                None,
                Some("no-provider".to_owned()),
            ),
        ] {
            assert!(
                resource
                    .apply(write(&name, &how, form.as_deref(), model.as_deref()))
                    .is_err(),
                "{name:?} {model:?}"
            );
        }
        assert_eq!(kept(&resource), Vec::new());
        resource
            .apply(write("Report", "Do it.", None, None))
            .unwrap();
        resource
            .apply(write("Brief", "Do it.", None, None))
            .unwrap();
        let both = kept(&resource);
        let (report, brief) = (&both[0].0, &both[1].0);
        let renamed = resource.apply(Skills::CORRECT_SKILL.with(skills::CorrectSkill {
            name: Some("report".into()),
            ..correct(brief)
        }));
        assert!(renamed.unwrap_err().to_string().contains("already a skill"));
        for n in 0..MAX_EXAMPLES + 3 {
            resource
                .apply(example(report, &format!("Result {n}."), n % 2 == 0, None))
                .unwrap();
        }
        let examples = kept(&resource)[0].1.examples.clone();
        assert_eq!(examples.len(), MAX_EXAMPLES);
        assert_eq!(examples[0].text, "Result 3.");
        assert!(
            resource
                .apply(example(report, &"e".repeat(MAX_EXAMPLE + 1), true, None))
                .is_err()
        );
        for n in 2..MAX_SKILLS {
            resource
                .apply(write(&format!("Skill {n}"), "Do it.", None, None))
                .unwrap();
        }
        let refused = resource
            .apply(write("One more", "Do it.", None, None))
            .unwrap_err();
        assert!(refused.to_string().contains("forget one"), "{refused}");
        // A skill kept already is still written again at the limit.
        resource
            .apply(write("Skill 7", "Do it better.", None, None))
            .unwrap();
        assert_eq!(kept(&resource).len(), MAX_SKILLS);
    }

    /// A skill is named by its id or by what it is called, fits a job by its words, and is
    /// told to an agent whole within what it is given: what it says first, then the newest
    /// examples that fit.
    #[test]
    fn a_skill_is_named_fits_a_job_by_its_words_and_is_told_within_bounds() {
        let mut resource = Replica::<Skills>::new().as_author(ADA);
        resource.set_now_ms(1_700_000_000_000);
        resource
            .apply(write(
                "Weekly report",
                "Say what moved first.",
                Some("Three short paragraphs"),
                None,
            ))
            .unwrap();
        let state = resource.state().unwrap();
        let (id, skill) = state.named("weekly report").expect("named by its name");
        assert_eq!(state.named(id.as_str()).map(|(id, _)| id), Some(id.clone()));
        assert!(state.named("Monthly report").is_none());
        // The job names it, or shares two longer words with what it is for; one shared word,
        // or short ones, do not make it fit.
        assert!(skill.fits("Please write the WEEKLY REPORT for the board"));
        assert!(skill.fits("a request came in: answering it is due"));
        assert!(skill.fits("Reporting the weekly numbers"));
        assert!(!skill.fits("Plan the offsite for the team"));
        assert!(!skill.fits("a report on the fire drill"));
        resource
            .apply(example(
                &id,
                "Revenue rose 4 %.",
                true,
                Some("Exactly this"),
            ))
            .unwrap();
        resource
            .apply(example(&id, "Many things happened.", false, None))
            .unwrap();
        let state = resource.state().unwrap();
        let (_, skill) = state.named("Weekly report").unwrap();
        let told = skill.told(8 * 1024);
        assert_eq!(
            told,
            "Skill: Weekly report\nFor: Answering a request that asks for a weekly report\n\
             How it is done here:\nSay what moved first.\nThe form of the result:\n\
             Three short paragraphs\nResults this person accepted (more like these):\n\
             - Revenue rose 4 %. (they said: Exactly this)\n\
             Results this person rejected (less like these):\n- Many things happened."
        );
        // Given less, it says what it says and leaves examples out whole, never half of one.
        let short = skill.told(told.len() - 1);
        assert!(short.ends_with("(they said: Exactly this)"), "{short}");
        assert!(skill.told(40).len() <= 40);
        // What the resource holds reads back as it was written, by its fields.
        let read = Skills::from_json(&state.to_json()).expect("the resource reads");
        assert_eq!(read, state);
        let content = state.read_document(None).unwrap();
        assert!(
            content.to_string().contains("Say what moved first."),
            "{content}"
        );
        assert_eq!(state.search().len(), 1);
    }
}
