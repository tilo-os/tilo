//! What waits for the person's yes, written into the shell's `Asking` root
//! (`type Asking (origin: session.asking)`): each call that needs it, read from its own asked
//! `tilo.invocations` record by the host. The session reads them from the records that sync to its
//! device, a server from its own, and both write them here, so the shell's one asking view shows
//! the same questions on the desktop and on the web. The views only read them: an answer is the
//! record's own `resolve`, called as any caller calls it.

use tilo_actions::remote::asked::{Asked, AskedBy, Offer};

use super::{Mirror, ORIGIN, origin_root, text};
use crate::origin::{Origin, OriginCx, Polled};
use crate::runtime::Value;

/// One question as the shell's `Asked` holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AskedReading {
    /// The asked record's address, `<workspace>/<record>`: what the answer is called on.
    pub key: String,
    pub workspace_name: String,
    /// The action, by its label.
    pub name: String,
    /// What the asker says of its own; empty where the action's label says it.
    pub words: String,
    /// What the call acts on, by its name; empty where it names nothing.
    pub on: String,
    pub destructive: bool,
    /// What always lets the asker do: the action's verb and the plural of what it acts on.
    pub verb: String,
    pub plural: String,
    pub can_always: bool,
    /// Who asks: `agent`, `app` or `device`.
    pub by: String,
    /// The agent's or the device's name, or the app's title.
    pub by_name: String,
    /// Who wrote the app that asks; empty for none.
    pub author: String,
    pub asked_at_ms: i64,
    /// Where the press that asked it was made: the device and the window's tab.
    pub pressed_device: String,
    pub pressed_window: String,
    pub conversation: String,
    /// What it shows before it is answered, as labels and values, in order.
    pub facts: Vec<(String, String)>,
    /// The records the called action writes besides its own, each by the app that keeps it and
    /// what the step does there, in the person's words.
    pub effects: Vec<(String, String)>,
    /// The choices it answers with instead of yes or no, in order.
    pub choices: Vec<String>,
}

impl From<&Asked> for AskedReading {
    /// The row a host lists for its person, as the shell holds it: who asks in named parts,
    /// always offered only where it names what it lets the asker do.
    fn from(row: &Asked) -> Self {
        let (by, by_name, author) = match &row.facts.by {
            AskedBy::Agent(agent) => ("agent", agent.name.clone(), String::new()),
            AskedBy::App { title, author, .. } => (
                "app",
                title.clone(),
                author.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            ),
            AskedBy::Device(device) => ("device", device.name.clone(), String::new()),
            AskedBy::Program { name, .. } => ("program", name.clone(), String::new()),
        };
        let plural = row.facts.plural.clone().unwrap_or_default();
        let (pressed_device, pressed_window) = row
            .pressed
            .as_ref()
            .map(|p| (p.device.clone(), p.instance.clone()))
            .unwrap_or_default();
        Self {
            key: format!("{}/{}", row.record.workspace, row.record.record),
            workspace_name: row.workspace_name.clone(),
            name: row.name.clone(),
            words: row.facts.words.clone(),
            on: row.facts.on.clone().unwrap_or_default(),
            destructive: row.facts.destructive,
            can_always: !plural.is_empty()
                && !row.facts.verb.is_empty()
                && row.offers.contains(&Offer::ApproveAndGrant),
            verb: row.facts.verb.clone(),
            plural,
            by: by.to_owned(),
            by_name,
            author,
            asked_at_ms: i64::try_from(row.asked_at_ms).unwrap_or(i64::MAX),
            pressed_device,
            pressed_window,
            conversation: row.facts.conversation.clone().unwrap_or_default(),
            facts: row
                .facts
                .facts
                .iter()
                .map(|f| (f.label.clone(), f.value.clone()))
                .collect(),
            effects: row
                .facts
                .effects
                .iter()
                .map(|e| (e.app.clone(), e.what.clone()))
                .collect(),
            choices: row.facts.choices.clone(),
        }
    }
}

/// A fact or a choice of a question, as the shell holds it: keyed `<question>/<number>`.
struct Part<'a> {
    key: String,
    question: &'a str,
    index: usize,
    label: &'a str,
    value: &'a str,
}

/// The questions as they were last written into the shell's `Asking` root, so the next write
/// changes only what came, went or changed.
#[derive(Default)]
pub struct AskingWriter {
    mirror: Mirror,
}

impl AskingWriter {
    /// Write `questions` into the shell's `Asking` root: only what changed since the last write.
    /// Whether anything was written.
    pub fn write(&mut self, cx: &OriginCx<'_>, questions: &[AskedReading]) -> bool {
        // A shell that declares no asking view is written nothing.
        let Some((root, types)) = origin_root(cx, "asking") else {
            return false;
        };
        let find = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
        };
        let Some(ty) = find("asking") else {
            return false;
        };
        let facts = parts(questions, |q| {
            q.facts
                .iter()
                .map(|(label, value)| (label.as_str(), value.as_str()))
                .collect()
        });
        let choices = parts(questions, |q| {
            q.choices.iter().map(|label| (label.as_str(), "")).collect()
        });
        let effects = parts(questions, |q| {
            q.effects
                .iter()
                .map(|(app, what)| (app.as_str(), what.as_str()))
                .collect()
        });
        let mut writes = self.mirror.sync(
            questions,
            &root,
            ty.field("questions"),
            find("asked"),
            |q| q.key.clone(),
            question_fields,
        );
        writes.extend(self.mirror.sync(
            &facts,
            &root,
            ty.field("facts"),
            find("asked_fact"),
            |f| f.key.clone(),
            |f| {
                vec![
                    ("key", text(&f.key)),
                    ("question", text(f.question)),
                    ("label", text(f.label)),
                    ("value", text(f.value)),
                ]
            },
        ));
        writes.extend(self.mirror.sync(
            &choices,
            &root,
            ty.field("choices"),
            find("asked_choice"),
            |c| c.key.clone(),
            |c| {
                vec![
                    ("key", text(&c.key)),
                    ("question", text(c.question)),
                    (
                        "index",
                        Value::Int(i64::try_from(c.index).unwrap_or(i64::MAX)),
                    ),
                    ("label", text(c.label)),
                ]
            },
        ));
        writes.extend(self.mirror.sync(
            &effects,
            &root,
            ty.field("effects"),
            find("asked_effect"),
            |e| e.key.clone(),
            |e| {
                vec![
                    ("key", text(&e.key)),
                    ("question", text(e.question)),
                    ("app", text(e.label)),
                    ("what", text(e.value)),
                ]
            },
        ));
        if writes.is_empty() {
            return false;
        }
        self.mirror
            .apply(cx, writes, "what waits for the person's yes")
    }
}

/// The facts or the choices of `questions`, as `of` lists each question's, keyed by their
/// question and their place among its.
fn parts<'a>(
    questions: &'a [AskedReading],
    of: fn(&'a AskedReading) -> Vec<(&'a str, &'a str)>,
) -> Vec<Part<'a>> {
    questions
        .iter()
        .flat_map(|q| {
            of(q)
                .into_iter()
                .enumerate()
                .map(move |(index, (label, value))| Part {
                    key: format!("{}/{index}", q.key),
                    question: &q.key,
                    index,
                    label,
                    value,
                })
        })
        .collect()
}

/// The fields of a question, as it is written into the shell's record. Its facts and choices are
/// their own collections', each naming its question by key.
fn question_fields(q: &AskedReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&q.key)),
        ("workspace_name", text(&q.workspace_name)),
        ("name", text(&q.name)),
        ("words", text(&q.words)),
        ("on", text(&q.on)),
        ("destructive", Value::Bool(q.destructive)),
        ("verb", text(&q.verb)),
        ("plural", text(&q.plural)),
        ("can_always", Value::Bool(q.can_always)),
        ("by", text(&q.by)),
        ("by_name", text(&q.by_name)),
        ("author", text(&q.author)),
        ("asked_at_ms", Value::Int(q.asked_at_ms)),
        ("pressed_device", text(&q.pressed_device)),
        ("pressed_window", text(&q.pressed_window)),
        ("conversation", text(&q.conversation)),
    ]
}

/// What waits for the person's yes as the origin of a shell's `session.asking`, for a host with
/// nothing else of the session to give it: a server showing the asking view on a web page. The
/// host hands it each reading ([`Self::read`]); the instance writes what changed with its next
/// frame.
#[derive(Default)]
pub struct AskingOrigin {
    writer: AskingWriter,
    questions: Vec<AskedReading>,
    due: bool,
}

impl AskingOrigin {
    /// The questions now, written with the instance's next frame when they changed. Whether they
    /// changed.
    pub fn read(&mut self, questions: Vec<AskedReading>) -> bool {
        if self.questions == questions {
            return false;
        }
        self.questions = questions;
        self.due = true;
        true
    }
}

impl Origin for AskingOrigin {
    fn name(&self) -> &str {
        ORIGIN
    }

    /// An answer is the record's own `resolve`, called as an action; no outside effect is the
    /// questions'.
    fn serves(&self, _capability: &str) -> bool {
        false
    }

    fn due(&self) -> bool {
        self.due
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        self.due = false;
        Polled {
            wrote: self.writer.write(cx, &self.questions),
            settle: false,
        }
    }

    fn deliver(
        &mut self,
        capability: &str,
        _payload: &serde_json::Value,
    ) -> tilolang_runtime::Delivery {
        tilolang_runtime::Delivery::Rejected(format!(
            "the questions take no outside effect `{capability}`"
        ))
    }
}
