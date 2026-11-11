//! What a person reviews of a page several actors filled: its forms, each
//! question with what it holds now and who set it, and what was done in the
//! tab, in order.
//!
//! All of it is read from the engine. The forms read names the questions,
//! the fields read says what each holds now and who set it, and the tab's
//! record says who did what. This is the shape the review view draws of
//! those answers, built anew from each one: it is no state of its own, and a
//! secret's text never enters it.

use serde_json::Value;

/// How many of the tab's actions the review keeps: as many as the tab's own
/// record holds.
const MAX_STEPS: usize = 1000;

/// Who set what a question holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetBy {
    /// An actor's action: the name the action door admitted it under, empty
    /// for the person at the window, and the action's place in the record.
    Action { actor: String, seq: u64 },
    /// Typed straight into the page, as a person does.
    Input,
    /// The page's own script.
    Page,
    /// Nothing, since the page was first read.
    NoOne,
    /// The fields read did not list the question, so nothing is claimed.
    Unknown,
}

/// What a question holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Holds {
    Nothing,
    Text(String),
    /// A box that is ticked.
    Yes,
    /// A box that is not.
    No,
    /// A secret, by whether it is set: its text is never read.
    Secret {
        set: bool,
    },
}

impl Holds {
    /// Whether the question is left without an answer.
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, Self::Nothing | Self::No | Self::Secret { set: false })
    }
}

/// One question of a form, as the page holds it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Question {
    pub(crate) name: String,
    pub(crate) holds: Holds,
    pub(crate) set: SetBy,
    /// It no longer holds what was set.
    pub(crate) changed: bool,
    pub(crate) required: bool,
    /// The page's own validation refuses what it holds.
    pub(crate) invalid: bool,
    /// What the page says is wrong with it, in the page's words.
    pub(crate) problem: Option<String>,
    /// The ref that brings its field into view.
    pub(crate) element: Option<String>,
}

impl Question {
    /// A required question nobody answered.
    pub(crate) fn required_and_empty(&self) -> bool {
        self.required && self.holds.is_empty()
    }
}

/// A form of the page with its questions, in the order it asks them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Form {
    /// The engine's id for it; empty for fields no form read names.
    pub(crate) id: String,
    /// Its own name or the heading it stands under; empty when it has none.
    pub(crate) label: String,
    /// The frame whose document holds it, by the number the tab calls the
    /// frame by; `None` for a form of the page's own document.
    pub(crate) frame: Option<String>,
    pub(crate) questions: Vec<Question>,
}

/// Every form of the page, as the last read of it answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fields {
    pub(crate) forms: Vec<Form>,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn list<'a>(value: Option<&'a Value>, key: &str) -> &'a [Value] {
    value
        .and_then(|value| value.get(key))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A whole number of the engine's, which may arrive written as a double
/// (`3.0`).
fn whole(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && *number >= 0.0 && number.fract() == 0.0)
            .map(ui::num::u64_of_f64)
    })
}

/// The length the engine lists a secret by (`<14 characters hidden>`): all it
/// says of one.
fn hidden_length(value: &str) -> Option<u64> {
    value
        .strip_prefix('<')?
        .strip_suffix(" characters hidden>")?
        .parse()
        .ok()
}

/// The frame a field is in, as the fields read says it of a field of a
/// frame's document: the number the tab calls the frame by. `None` for a
/// field of the page's own document.
fn frame_of(field: &Value) -> Option<String> {
    let said = match field.get("frame")? {
        Value::String(frame) => frame.clone(),
        Value::Number(frame) => frame.to_string(),
        _ => return None,
    };
    ui::webview::FrameId::parse(&said).map(|frame| frame.as_str().to_owned())
}

/// List `fields`, which no forms read names, grouped by the question the
/// fields read gives each, under the forms of `frame`'s document it puts
/// them in (`None`: the page's own).
fn list_alone(out: &mut Vec<Form>, frame: Option<&str>, fields: &[&Value]) {
    let mut left: Vec<(&str, &str, Vec<&Value>)> = Vec::new();
    for field in fields {
        let form = text(field, "form");
        let question = text(field, "question");
        let same = left
            .iter_mut()
            .find(|(of, name, _)| !question.is_empty() && *of == form && *name == question);
        match same {
            Some((_, _, answers)) => answers.push(*field),
            None => left.push((form, question, vec![*field])),
        }
    }
    for (form, question, answers) in left {
        let name = if question.is_empty() {
            answers.first().map_or("", |field| text(field, "name"))
        } else {
            question
        };
        let question = Question::read(name, None, &answers);
        match out
            .iter_mut()
            .find(|listed| listed.id == form && listed.frame.as_deref() == frame)
        {
            Some(listed) => listed.questions.push(question),
            None => out.push(Form {
                id: form.to_owned(),
                label: String::new(),
                frame: frame.map(str::to_owned),
                questions: vec![question],
            }),
        }
    }
}

/// Who set a field, as the fields read says it; `None` where it says
/// nothing, and whether the field changed since.
fn set_of(field: &Value) -> Option<(SetBy, bool)> {
    let set = field.get("set")?;
    let by = match text(set, "how") {
        "action" => SetBy::Action {
            actor: text(set, "by").to_owned(),
            seq: whole(set.get("seq")).unwrap_or(0),
        },
        "input" => SetBy::Input,
        "page" => SetBy::Page,
        _ => return None,
    };
    Some((by, flag(set, "changed")))
}

/// What the forms read says of a question, where there was one.
struct Asked<'a> {
    kind: &'a str,
    required: bool,
    /// What it holds, absent while it is unanswered.
    value: Option<&'a Value>,
    element: Option<&'a str>,
}

impl<'a> Asked<'a> {
    fn of(asked: &'a Value) -> Self {
        Self {
            kind: text(asked, "kind"),
            required: flag(asked, "required"),
            value: asked.get("value"),
            element: asked
                .get("element")
                .and_then(Value::as_str)
                .or_else(|| list(Some(asked), "elements").first()?.as_str()),
        }
    }

    /// What the forms read alone says the question holds: the fields read
    /// listed no field for it.
    fn holds(&self) -> Holds {
        let secret = self.kind == "secret";
        match self.value {
            None if secret => Holds::Secret { set: false },
            None if self.kind == "flag" => Holds::No,
            None => Holds::Nothing,
            Some(_) if secret => Holds::Secret { set: true },
            Some(Value::Bool(true)) => Holds::Yes,
            Some(Value::Bool(false)) => Holds::No,
            Some(Value::Array(chosen)) => Holds::Text(
                chosen
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(Value::String(held)) => Holds::Text(held.clone()),
            Some(other) => Holds::Text(other.to_string()),
        }
    }
}

impl Question {
    /// The question `name` from what the forms read asked under it and the
    /// fields the fields read listed for it: one, or one per answer of a
    /// group (radios of one name, checkboxes of one name).
    fn read(name: &str, asked: Option<&Asked<'_>>, fields: &[&Value]) -> Self {
        let group = fields.iter().any(|field| field.get("answer").is_some());
        let chosen: Vec<&Value> = if group {
            fields
                .iter()
                .copied()
                .filter(|field| text(field, "value") == "checked")
                .collect()
        } else {
            fields.to_vec()
        };
        let holds = match (fields.first(), asked) {
            (None, Some(asked)) => asked.holds(),
            (None, None) => Holds::Nothing,
            (Some(_), _) if group => {
                if chosen.is_empty() {
                    Holds::Nothing
                } else {
                    Holds::Text(
                        chosen
                            .iter()
                            .map(|field| text(field, "answer"))
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                }
            }
            (Some(field), _) => Self::held(field, asked),
        };
        // Who set the answer the question gives now: the answers of a group
        // that are chosen, else whatever was last done to it.
        let sources = if chosen.is_empty() { fields } else { &chosen };
        let mut set: Option<SetBy> = None;
        let mut changed = false;
        for (by, moved) in sources.iter().filter_map(|field| set_of(field)) {
            changed |= moved;
            let newer = match (&set, &by) {
                (None, _) => true,
                (Some(SetBy::Action { seq: kept, .. }), SetBy::Action { seq, .. }) => seq > kept,
                _ => false,
            };
            if newer {
                set = Some(by);
            }
        }
        let set = set.unwrap_or(if fields.is_empty() {
            SetBy::Unknown
        } else {
            SetBy::NoOne
        });
        Self {
            name: name.to_owned(),
            holds,
            set,
            changed,
            required: asked.is_some_and(|asked| asked.required)
                || fields.iter().any(|field| flag(field, "required")),
            invalid: fields.iter().any(|field| flag(field, "invalid")),
            problem: fields
                .iter()
                .map(|field| text(field, "problem"))
                .find(|problem| !problem.is_empty())
                .map(str::to_owned),
            element: chosen
                .first()
                .or_else(|| fields.first())
                .map(|field| text(field, "element"))
                .filter(|element| !element.is_empty())
                .or_else(|| asked.and_then(|asked| asked.element))
                .map(str::to_owned),
        }
    }

    /// What one field holds, as the fields read listed it.
    fn held(field: &Value, asked: Option<&Asked<'_>>) -> Holds {
        let value = text(field, "value");
        let kind = asked.map_or("", |asked| asked.kind);
        // A secret is whatever the forms read calls one, and whatever the
        // fields read lists by its length. Either way only whether it is set
        // is kept.
        if kind == "secret" || hidden_length(value).is_some() {
            return Holds::Secret {
                set: hidden_length(value).map_or(!value.is_empty(), |length| length > 0),
            };
        }
        if kind == "flag" || matches!(text(field, "role"), "checkbox" | "radio" | "switch") {
            return if value == "checked" {
                Holds::Yes
            } else {
                Holds::No
            };
        }
        // A choice whose empty option is shown is no answer: the forms read
        // says so by listing no value.
        let unanswered = asked.is_some_and(|asked| asked.value.is_none());
        if unanswered || value.trim().is_empty() {
            Holds::Nothing
        } else {
            Holds::Text(value.to_owned())
        }
    }
}

impl Fields {
    /// The page's forms from the engine's answers to the forms read (`forms`,
    /// `None` when it gave none) and the fields read.
    ///
    /// The forms read names each form and the questions it asks, in its
    /// order; the fields read says what each holds now and who set it. A
    /// field the forms read does not name (one in a shadow root, or all of
    /// them when the forms read gave nothing) is listed after them, under the
    /// form the fields read puts it in.
    pub(crate) fn read(forms: Option<&Value>, fields: &Value) -> Self {
        let all = list(Some(fields), "fields");
        // The page's own fields: a field of a frame says which frame, and is
        // listed after them under that frame.
        let listed: Vec<&Value> = all
            .iter()
            .filter(|field| frame_of(field).is_none())
            .collect();
        let mut taken = vec![false; listed.len()];
        let mut out: Vec<Form> = Vec::new();
        for form in list(forms, "forms") {
            let id = text(form, "form");
            let mut questions = Vec::new();
            for asked in list(Some(form), "fields") {
                let name = text(asked, "name");
                let mut answers: Vec<&Value> = Vec::new();
                for (index, field) in listed.iter().enumerate() {
                    if text(field, "form") == id && text(field, "question") == name {
                        if let Some(slot) = taken.get_mut(index) {
                            *slot = true;
                        }
                        answers.push(*field);
                    }
                }
                questions.push(Question::read(name, Some(&Asked::of(asked)), &answers));
            }
            if !questions.is_empty() {
                out.push(Form {
                    id: id.to_owned(),
                    label: text(form, "label").trim().to_owned(),
                    frame: None,
                    questions,
                });
            }
        }
        // What is left, grouped by the question the fields read gives it.
        let left: Vec<&Value> = listed
            .iter()
            .zip(&taken)
            .filter(|(_, taken)| !**taken)
            .map(|(field, _)| *field)
            .collect();
        list_alone(&mut out, None, &left);
        // The fields of each frame the read reached, in the order it listed
        // the frames. No forms read names them: they are grouped by the
        // question the fields read gives each.
        let mut frames: Vec<String> = Vec::new();
        for frame in all.iter().filter_map(frame_of) {
            if !frames.contains(&frame) {
                frames.push(frame);
            }
        }
        for frame in &frames {
            let own: Vec<&Value> = all
                .iter()
                .filter(|field| frame_of(field).as_deref() == Some(frame.as_str()))
                .collect();
            list_alone(&mut out, Some(frame.as_str()), &own);
        }
        Self { forms: out }
    }

    /// The names of the questions that hold a secret.
    pub(crate) fn secrets(&self) -> Vec<&str> {
        self.forms
            .iter()
            .flat_map(|form| &form.questions)
            .filter(|question| matches!(question.holds, Holds::Secret { .. }))
            .map(|question| question.name.as_str())
            .collect()
    }
}

/// What a step of the tab's record is: an actor's action on the page, or
/// something no action made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    /// An action, which the step's `action` names as the engine does
    /// (`fill`, `click`).
    Action,
    /// A person typed it straight into the page.
    Typed,
    /// The window held something the page was about to send, or what it
    /// held ended.
    Hold(HoldStep),
}

/// What a hold's step says: that something waits for the person, or how what
/// waited ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldStep {
    /// A form submission waits.
    Submit,
    /// A request with data waits.
    Request,
    /// A download waits.
    Download,
    /// Something waits that this review has no word of its own for.
    Other,
    /// What waited was let leave.
    Allowed,
    /// The page was kept as it is.
    Kept,
    /// What waited was dropped with nobody answering.
    Dropped,
    /// It never waited: as many as the window asks the person about already
    /// did, and it was dropped at once.
    TooMany,
    /// What waited was dropped because the page it was held on went away.
    Gone,
}

impl HoldStep {
    /// What the record's `action` says of a hold.
    fn read(action: &str) -> Self {
        match action {
            "submit" => Self::Submit,
            "request" => Self::Request,
            "download" => Self::Download,
            "allow" => Self::Allowed,
            "keep" => Self::Kept,
            "drop" => Self::Dropped,
            "too_many" => Self::TooMany,
            "gone" => Self::Gone,
            _ => Self::Other,
        }
    }

    /// Whether it says how a hold ended, rather than that one began.
    pub(crate) const fn ended(self) -> bool {
        matches!(
            self,
            Self::Allowed | Self::Kept | Self::Dropped | Self::TooMany | Self::Gone
        )
    }
}

/// One step of the tab's record: an action that operated the page, or what
/// no action made, as the record keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    /// Its place in the record.
    pub(crate) seq: u64,
    /// Who asked: the name the action door admitted them under, empty for
    /// the person at the window.
    pub(crate) actor: String,
    /// What was asked, by the engine's name for it (`fill`, `click`); empty
    /// for what was typed straight into the page.
    pub(crate) action: String,
    /// What it is: an action, what a person typed, or a hold.
    pub(crate) kind: StepKind,
    /// Of a hold, what the page was about to send, as the record says it:
    /// its method and where it would go, and of a download the file. Never
    /// what it carried.
    pub(crate) leaving: String,
    /// It wrote into a secret field: the record says the field was set and
    /// holds none of what was written.
    pub(crate) secret: bool,
    pub(crate) ok: bool,
    /// The element it acted on, by its name (its role when it has none).
    pub(crate) target: Option<String>,
    /// What it wrote, chose or answered: a fill's text, a select's options,
    /// the questions a form answer placed.
    pub(crate) wrote: Vec<String>,
    /// When it ended, in milliseconds since the Unix epoch.
    pub(crate) time_ms: Option<u64>,
}

impl Step {
    fn read(entry: &Value) -> Option<Self> {
        let target = entry.get("target").and_then(|target| {
            [text(target, "name"), text(target, "role")]
                .into_iter()
                .find(|said| !said.trim().is_empty())
                .map(str::to_owned)
        });
        let secret = flag(entry, "secret");
        let mut wrote: Vec<String> = Vec::new();
        // What went into a secret field is never read, whatever the entry
        // carries.
        if !secret {
            let written = text(entry, "text");
            if !written.is_empty() {
                wrote.push(written.to_owned());
            }
            wrote.extend(
                list(Some(entry), "values")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
        }
        wrote.extend(
            list(Some(entry), "answers")
                .iter()
                .map(|answer| text(answer, "name"))
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        );
        let kind = match text(entry, "how") {
            "input" => StepKind::Typed,
            "hold" => StepKind::Hold(HoldStep::read(text(entry, "action"))),
            _ => StepKind::Action,
        };
        Some(Self {
            seq: whole(entry.get("seq"))?,
            actor: text(entry, "actor").to_owned(),
            action: text(entry, "action").to_owned(),
            kind,
            leaving: if matches!(kind, StepKind::Hold(_)) {
                text(entry, "message").to_owned()
            } else {
                String::new()
            },
            secret,
            ok: entry.get("ok").and_then(Value::as_bool).unwrap_or(true),
            target,
            wrote,
            time_ms: whole(entry.get("time_ms")),
        })
    }
}

/// What was done in the tab, in order, as far as the record was read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) steps: Vec<Step>,
    /// The place to read on from.
    next: u64,
}

impl Record {
    /// The place in the tab's record the next read asks from.
    pub(crate) fn since(&self) -> u64 {
        self.next
    }

    /// Take the engine's answer to a read of the record from
    /// [`Self::since`]: the actions after that place join the ones kept.
    /// Returns whether it changed what is kept, and whether the record goes
    /// on past this answer.
    pub(crate) fn take(&mut self, data: &Value) -> (bool, bool) {
        let next = whole(data.get("next")).unwrap_or(self.next);
        let mut changed = false;
        // A record that stands before the place already read is another
        // tab's: the engine behind this window was replaced.
        if next < self.next {
            self.steps.clear();
            changed = true;
        }
        for step in list(Some(data), "entries").iter().filter_map(Step::read) {
            if self.steps.last().is_none_or(|kept| step.seq > kept.seq) {
                self.steps.push(step);
                changed = true;
            }
        }
        if self.steps.len() > MAX_STEPS {
            let over = self.steps.len() - MAX_STEPS;
            self.steps.drain(..over);
        }
        self.next = next.max(self.steps.last().map_or(0, |step| step.seq));
        (changed, flag(data, "more"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The engine's own shapes, as its tests assert them: a form two agents
    /// and a person answered, one answer rewritten behind them by the page.
    fn page() -> (Value, Value) {
        let forms = json!({"forms": [{
            "form": "form1", "label": "Apply for a visitor visa", "method": "get",
            "action": "/sent", "signature": "0a1b2c3d", "submit": ["Send application"],
            "fields": [
                {"name": "Given names", "kind": "text", "value": "Maria Elena", "element": "d1e2"},
                {"name": "Nationality", "kind": "choice", "choices": ["Kenya", "Other"],
                    "required": true, "element": "d1e4"},
                {"name": "Refused before?", "kind": "choice", "choices": ["Yes", "No"],
                    "value": ["No"], "elements": ["d1e6", "d1e7"]},
                {"name": "Portal password", "kind": "secret", "required": true,
                    "value": "<14 characters hidden>", "element": "d1e8"},
                {"name": "Arrival", "kind": "date", "value": "2027-02-04", "element": "d1e9"},
                {"name": "Accept terms", "kind": "flag", "required": true, "element": "d1e10"},
            ],
        }]});
        let fields = json!({"fields": [
            {"element": "d1e2", "role": "textbox", "name": "Given names", "value": "Maria Elena",
                "form": "form1", "question": "Given names",
                "set": {"how": "action", "by": "agent:ada", "seq": 1}},
            {"element": "d1e4", "role": "combobox", "name": "Nationality",
                "value": "Select a country", "required": true,
                "form": "form1", "question": "Nationality"},
            {"element": "d1e6", "role": "radio", "name": "Yes", "value": "unchecked",
                "form": "form1", "question": "Refused before?", "answer": "Yes"},
            {"element": "d1e7", "role": "radio", "name": "No", "value": "checked",
                "form": "form1", "question": "Refused before?", "answer": "No",
                "set": {"how": "action", "by": "agent:ben", "seq": 2.0}},
            {"element": "d1e8", "role": "textbox", "name": "Portal password",
                "value": "<14 characters hidden>", "required": true,
                "form": "form1", "question": "Portal password", "set": {"how": "input"}},
            {"element": "d1e9", "role": "textbox", "name": "Arrival", "value": "2027-02-04",
                "invalid": true, "problem": "Choose a date after today.",
                "form": "form1", "question": "Arrival",
                "set": {"how": "action", "by": "agent:ben", "seq": 2, "changed": true}},
            {"element": "d1e10", "role": "checkbox", "name": "Accept terms",
                "value": "unchecked", "required": true,
                "form": "form1", "question": "Accept terms", "set": {"how": "page"}},
            {"element": "d1e12", "role": "searchbox", "name": "Search", "value": ""},
        ]});
        (forms, fields)
    }

    fn question<'a>(fields: &'a Fields, name: &str) -> &'a Question {
        fields
            .forms
            .iter()
            .flat_map(|form| &form.questions)
            .find(|question| question.name == name)
            .unwrap_or_else(|| panic!("the review lists {name}"))
    }

    #[test]
    fn a_form_is_reviewed_by_its_questions_in_its_own_order() {
        let (forms, fields) = page();
        let review = Fields::read(Some(&forms), &fields);
        assert_eq!(review.forms.len(), 2, "the form, then what no form asks");
        let form = &review.forms[0];
        assert_eq!(
            (form.id.as_str(), form.label.as_str()),
            ("form1", "Apply for a visitor visa")
        );
        assert_eq!(
            form.questions
                .iter()
                .map(|question| question.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Given names",
                "Nationality",
                "Refused before?",
                "Portal password",
                "Arrival",
                "Accept terms"
            ]
        );
        let other = &review.forms[1];
        assert_eq!((other.id.as_str(), other.label.as_str()), ("", ""));
        assert_eq!(other.questions[0].name, "Search");
        assert_eq!(other.questions[0].set, SetBy::NoOne);
    }

    #[test]
    fn each_answer_says_what_it_holds_and_who_set_it() {
        let (forms, fields) = page();
        let review = Fields::read(Some(&forms), &fields);
        let given = question(&review, "Given names");
        assert_eq!(given.holds, Holds::Text("Maria Elena".into()));
        assert_eq!(
            given.set,
            SetBy::Action {
                actor: "agent:ada".into(),
                seq: 1
            }
        );
        assert!(!given.changed && !given.required_and_empty() && !given.invalid);
        assert_eq!(given.element.as_deref(), Some("d1e2"));

        // Radios of one name are one question: its answer is the one chosen,
        // set by whoever chose it, and a seq written as a double is whole.
        let refused = question(&review, "Refused before?");
        assert_eq!(refused.holds, Holds::Text("No".into()));
        assert_eq!(
            refused.set,
            SetBy::Action {
                actor: "agent:ben".into(),
                seq: 2
            }
        );
        assert_eq!(refused.element.as_deref(), Some("d1e7"));

        // A choice showing its empty option holds no answer.
        let nationality = question(&review, "Nationality");
        assert_eq!(nationality.holds, Holds::Nothing);
        assert!(nationality.required_and_empty());
        assert_eq!(nationality.set, SetBy::NoOne);

        // What changed after it was set, and what the page refuses, say so.
        let arrival = question(&review, "Arrival");
        assert!(arrival.changed && arrival.invalid);
        assert_eq!(
            arrival.problem.as_deref(),
            Some("Choose a date after today.")
        );

        let terms = question(&review, "Accept terms");
        assert_eq!((&terms.holds, &terms.set), (&Holds::No, &SetBy::Page));
        assert!(terms.required_and_empty());
    }

    #[test]
    fn a_secret_is_kept_as_set_or_not_and_never_as_text() {
        let (forms, fields) = page();
        let review = Fields::read(Some(&forms), &fields);
        let password = question(&review, "Portal password");
        assert_eq!(password.holds, Holds::Secret { set: true });
        assert_eq!(password.set, SetBy::Input);
        assert_eq!(review.secrets(), ["Portal password"]);

        // Whatever a read says a secret holds, only whether it is set is
        // kept: a read that carried its text would not put it on screen.
        let leaked = json!({"fields": [{"element": "d1e8", "role": "textbox",
            "name": "Portal password", "value": "hunter2-secret",
            "form": "form1", "question": "Portal password"}]});
        let review = Fields::read(Some(&forms), &leaked);
        assert_eq!(
            question(&review, "Portal password").holds,
            Holds::Secret { set: true }
        );
        assert!(!format!("{review:?}").contains("hunter2"));

        // Without the forms read a secret is still known by how it is listed.
        let alone = json!({"fields": [
            {"role": "textbox", "name": "Password", "value": "<0 characters hidden>"},
            {"role": "textbox", "name": "PIN", "value": "<4 characters hidden>"},
        ]});
        let review = Fields::read(None, &alone);
        assert_eq!(
            question(&review, "Password").holds,
            Holds::Secret { set: false }
        );
        assert_eq!(question(&review, "PIN").holds, Holds::Secret { set: true });
    }

    #[test]
    fn fields_alone_are_grouped_by_the_question_they_answer() {
        let (_, fields) = page();
        let review = Fields::read(None, &fields);
        let form = review
            .forms
            .iter()
            .find(|form| form.id == "form1")
            .expect("the form the fields read names");
        assert_eq!(form.label, "");
        assert_eq!(form.questions.len(), 6, "two radios are one question");
        assert_eq!(
            question(&review, "Refused before?").holds,
            Holds::Text("No".into())
        );
        // A question the fields read does not list is said from the forms
        // read, with nothing claimed about who set it.
        let (forms, _) = page();
        let review = Fields::read(Some(&forms), &json!({"fields": []}));
        let given = question(&review, "Given names");
        assert_eq!(given.holds, Holds::Text("Maria Elena".into()));
        assert_eq!(given.set, SetBy::Unknown);
        assert_eq!(
            question(&review, "Portal password").holds,
            Holds::Secret { set: true }
        );
    }

    /// The record says of an entry that it was typed straight into the
    /// page, and that it wrote into a secret field: of that one nothing
    /// written is read, whatever the entry carries.
    #[test]
    fn a_step_says_that_it_was_typed_and_that_it_wrote_a_secret() {
        let mut record = Record::default();
        record.take(&json!({"entries": [
            {"seq": 1, "actor": "", "how": "input", "ok": true,
                "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"},
            {"seq": 2, "actor": "agent:ada", "action": "fill", "ok": true, "secret": true,
                "target": {"role": "textbox", "name": "Password"}, "text": "leaked",
                "values": ["leaked"]},
        ], "next": 2, "more": false}));
        let typed = &record.steps[0];
        assert!(typed.kind == StepKind::Typed && !typed.secret);
        assert_eq!((typed.action.as_str(), typed.actor.as_str()), ("", ""));
        assert_eq!(typed.wrote, ["ada@example.org"]);
        let secret = &record.steps[1];
        assert!(secret.kind == StepKind::Action && secret.secret);
        assert!(secret.wrote.is_empty());
        assert!(!format!("{record:?}").contains("leaked"));
    }

    /// The record says of an entry that the window held something the page
    /// was about to send, and how that ended: what it was, and who answered.
    #[test]
    fn a_step_says_what_was_held_and_how_it_ended() {
        let mut record = Record::default();
        record.take(&json!({"entries": [
            {"seq": 1, "actor": "agent:ada", "action": "click", "ok": true,
                "message": "Clicked button \"Send\"",
                "target": {"role": "button", "name": "Send"}},
            {"seq": 2, "actor": "agent:ada", "how": "hold", "action": "submit", "ok": true,
                "message": "POST https://visa.example", "to_url": "https://visa.example"},
            {"seq": 3, "actor": "", "how": "hold", "action": "allow", "ok": true,
                "message": "POST https://visa.example", "to_url": "https://visa.example"},
        ], "next": 3, "more": false}));
        let clicked = &record.steps[0];
        assert!(clicked.kind == StepKind::Action && clicked.leaving.is_empty());
        let held = &record.steps[1];
        assert_eq!(held.kind, StepKind::Hold(HoldStep::Submit));
        assert_eq!(
            (
                held.action.as_str(),
                held.actor.as_str(),
                held.leaving.as_str()
            ),
            ("submit", "agent:ada", "POST https://visa.example")
        );
        let allowed = &record.steps[2];
        assert_eq!(allowed.kind, StepKind::Hold(HoldStep::Allowed));
        assert_eq!(
            (allowed.action.as_str(), allowed.actor.as_str()),
            ("allow", "")
        );
    }

    /// The record says why what waited was dropped, and the review reads
    /// each reason as what it is.
    #[test]
    fn a_step_says_why_what_was_held_was_dropped() {
        let mut record = Record::default();
        let dropped = |seq: u64, action: &str| {
            json!({"seq": seq, "actor": "", "how": "hold", "action": action, "ok": true,
                "message": "POST https://visa.example", "to_url": "https://visa.example"})
        };
        record.take(
            &json!({"entries": [dropped(1, "drop"), dropped(2, "too_many"), dropped(3, "gone")],
            "next": 3, "more": false}),
        );
        let said: Vec<String> = record
            .steps
            .iter()
            .map(|step| format!("{:?}", step.kind))
            .collect();
        assert_eq!(said, ["Hold(Dropped)", "Hold(TooMany)", "Hold(Gone)"]);
    }

    #[test]
    fn the_record_is_read_on_in_order_and_never_twice() {
        let mut record = Record::default();
        let first = json!({"entries": [
            {"seq": 1, "time_ms": 1_700_000_000_000_u64, "actor": "agent:ada",
                "action": "fill", "ok": true,
                "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"},
            {"seq": 2.0, "actor": "agent:ben", "action": "fill_form", "ok": false,
                "answers": [{"name": "Arrival", "value": "2027-02-03"}]},
        ], "next": 2, "more": true, "dropped": 0, "waiting": 0});
        assert_eq!(record.since(), 0);
        assert_eq!(record.take(&first), (true, true));
        assert_eq!(record.since(), 2);
        assert_eq!(record.steps[0].target.as_deref(), Some("Email"));
        assert_eq!(record.steps[0].wrote, ["ada@example.org"]);
        assert_eq!(record.steps[0].time_ms, Some(1_700_000_000_000));
        assert!(!record.steps[1].ok);
        assert_eq!(record.steps[1].wrote, ["Arrival"]);
        // The same answer again adds nothing.
        assert_eq!(record.take(&first), (false, true));
        assert_eq!(record.steps.len(), 2);
        let second = json!({"entries": [
            {"seq": 3, "actor": "", "action": "scroll", "ok": true},
        ], "next": 3, "more": false});
        assert_eq!(record.take(&second), (true, false));
        assert_eq!(
            record.steps.iter().map(|step| step.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // A record that starts over is another tab's.
        let other = json!({"entries": [{"seq": 1, "actor": "agent:cy", "action": "click"}],
            "next": 1, "more": false});
        assert_eq!(record.take(&other), (true, false));
        assert_eq!(record.steps.len(), 1);
        assert_eq!(record.steps[0].actor, "agent:cy");
    }

    /// A frame's fields are listed after the page's own, under their frame,
    /// by the question the fields read gives each. A form of a frame is its
    /// own document's: a question it shares a name with is not the page's.
    #[test]
    fn the_fields_of_a_frame_are_listed_under_their_frame() {
        let (forms, _) = page();
        let fields = json!({"fields": [
            {"element": "d1e2", "role": "textbox", "name": "Given names", "value": "Maria Elena",
                "form": "form1", "question": "Given names"},
            {"element": "d3e2", "role": "textbox", "name": "Card number",
                "value": "<16 characters hidden>", "form": "form1", "question": "Card number",
                "frame": "3", "set": {"how": "input"}},
            {"element": "d3e3", "role": "textbox", "name": "Given names", "value": "Ada",
                "form": "form1", "question": "Given names", "frame": "3"},
            {"element": "d5e1", "role": "searchbox", "name": "Search", "value": "", "frame": 5},
            // What names no frame an outline printed is no frame's field.
            {"element": "d9e1", "role": "textbox", "name": "Stray", "value": "", "frame": "x; y"},
        ]});
        let review = Fields::read(Some(&forms), &fields);
        let listed: Vec<(Option<&str>, &str, usize)> = review
            .forms
            .iter()
            .map(|form| {
                (
                    form.frame.as_deref(),
                    form.id.as_str(),
                    form.questions.len(),
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (None, "form1", 6),
                (None, "", 1),
                (Some("3"), "form1", 2),
                (Some("5"), "", 1)
            ]
        );
        assert_eq!(
            review.forms[0].questions[0].holds,
            Holds::Text("Maria Elena".into()),
            "the page's own question is answered by the page's own field"
        );
        let card = &review.forms[2].questions[0];
        assert_eq!(
            (card.name.as_str(), &card.holds, &card.set),
            ("Card number", &Holds::Secret { set: true }, &SetBy::Input)
        );
        let given = &review.forms[2].questions[1];
        assert_eq!(given.holds, Holds::Text("Ada".into()));
        assert_eq!(given.element.as_deref(), Some("d3e3"));
        assert_eq!(review.forms[3].questions[0].name, "Search");
    }
}
