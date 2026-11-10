//! A page's forms as actions of the page.
//!
//! Once a page's forms are known, each is an action of `browser.page` named
//! after the form, with one typed parameter per question, and a second
//! action that sends it. They are known in two ways: the page was read, by
//! its window once it had loaded or by an actor (`read-page-forms`), or the
//! site was read on an earlier visit and [site memory](crate::site_memory)
//! holds what that found. Either way the action names its form by
//! signature, and the engine answers it only while the page holds a form of
//! that signature: one that has changed is refused and nothing is placed.
//!
//! [`PageForms`] is what one window knows of its page's forms. It holds no
//! answer and no field's value: a form's shape, how it is known, and what
//! running its action taught.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use ui::tilo_actions::{ActionParam, ActionSpec, InvocationId, ParamType};
use ui::tilo_resource::Effect as _;
use ui::webview::{FormSubmit, PageRequest};

use crate::effects::BrowserEffect;
use crate::site_memory::{self, Place, RememberedForm, RememberedQuestion};

/// The code a form action is refused or withdrawn with once its form is no
/// longer on the page as the action was made for it.
pub const FORM_CHANGED: &str = "form_changed";

/// What a withdrawn form action says while it stays listed: until the
/// window's own read of the page replaces it.
const WITHDRAWN: &str = "This form asks something else since its action was made; the page \
                         reads its forms again by itself and publishes the action of the form \
                         as it is now. If none appears, read them (read-page-forms)";

/// How many form actions in flight a window keeps track of.
const MAX_RUNS: usize = 32;
/// How many remembered forms a window keeps as found gone from a page.
const MAX_GONE: usize = 64;

/// What a form's action carries: the form it answers, and its answers by
/// the action's parameters. The parameters are the form's own questions, so
/// they are no fixed list: every argument that is not the form is an answer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FormAnswers {
    /// The form's signature.
    pub form: String,
    /// The answers, by the action's parameters.
    #[serde(flatten)]
    pub answers: serde_json::Map<String, serde_json::Value>,
}

impl ui::tilo_resource::JsonSchema for FormAnswers {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "form": {
                    "type": "string",
                    "description": "The form's signature, as read-page-forms gives it",
                },
            },
            "required": ["form"],
        })
    }
}

/// One run of a form's action: its answers, or its sending.
#[derive(Clone, Debug, PartialEq)]
pub struct FormRun {
    /// The form's signature.
    pub form: String,
    /// Answers by the action's parameters.
    pub answers: serde_json::Map<String, serde_json::Value>,
    /// Send the form, and how; `None` only answers it.
    pub send: Option<FormSend>,
}

/// How a form is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormSend {
    /// The label of the button that sends it; its first button when absent.
    pub button: Option<String>,
    /// Whether it may leave for another site than the page's own.
    pub cross_origin: bool,
}

impl FormRun {
    /// The request the engine answers this run with: the one
    /// `fill-page-form` makes, naming the form by its signature, with
    /// `answers` by the names the form asks under ([`PageForms::named`]).
    pub fn request(&self, answers: serde_json::Map<String, serde_json::Value>) -> PageRequest {
        PageRequest::FillForm {
            form: self.form.clone(),
            answers,
            submit: match &self.send {
                None => FormSubmit::No,
                Some(FormSend {
                    button: Some(label),
                    ..
                }) => FormSubmit::Button(label.clone()),
                Some(FormSend { button: None, .. }) => FormSubmit::First,
            },
            cross_origin: self.send.as_ref().is_some_and(|send| send.cross_origin),
            frame: None,
        }
    }
}

/// How a published form is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The page was read and asks it.
    Read,
    /// The site was read on an earlier visit; this page has not been.
    Remembered,
    /// It was read, and has asked something else since.
    Changed,
}

/// One form as a read of the page found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadForm {
    /// The id the engine gave it in that document, such as `form1`.
    pub id: String,
    /// What it asks.
    pub shape: RememberedForm,
    /// What the read said of the answers its choice questions offer: a hash
    /// that is another one once they are other answers. Empty when the read
    /// gave none.
    pub offers: String,
    /// It asks something else since it was read.
    pub changed: bool,
}

/// What hearing a form's answer taught.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answered {
    /// The form's shape with what the answer taught, and the place to
    /// remember it for, when it taught something.
    pub learned: Option<(Place, RememberedForm)>,
    /// The answer made the form ask more, or something else.
    pub grew: bool,
    /// The form asks what it asked and offers other answers than the page
    /// publishes for it.
    pub refresh: bool,
    /// The forms the site asked after this one on an earlier visit, by
    /// label and signature, when this answer sent the form.
    pub then: Vec<(String, String)>,
}

/// The action id of the form `signature`.
pub fn answer_id(signature: &str) -> String {
    format!("form-{signature}")
}

/// The id of the action that sends the form `signature`.
pub fn send_id(signature: &str) -> String {
    format!("send-form-{signature}")
}

/// `text` as it is kept: without control characters, at most a label long.
fn bounded(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(site_memory::MAX_TEXT)
        .collect()
}

fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

/// The texts of the list `key` of `value`, bounded in number and length.
fn texts(value: &serde_json::Value, key: &str, most: usize) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(serde_json::Value::as_str)
                .take(most)
                .map(bounded)
                .collect()
        })
        .unwrap_or_default()
}

fn read_question(field: &serde_json::Value) -> Option<RememberedQuestion> {
    let name = bounded(field.get("name")?.as_str()?);
    if name.is_empty() {
        return None;
    }
    let kind = field
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .filter(|kind| site_memory::KINDS.contains(kind))
        .unwrap_or("text");
    // What the typed path refuses by the field's kind is known before any
    // answer is tried.
    let fallback = match kind {
        "secret" => "secret_field",
        "file" => "needs_files",
        _ => "",
    };
    Some(RememberedQuestion {
        name,
        kind: kind.to_owned(),
        choices: texts(field, "choices", site_memory::MAX_CHOICES),
        required: field
            .get("required")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        fallback: fallback.to_owned(),
    })
}

fn read_form(form: &serde_json::Value) -> Option<ReadForm> {
    let signature = form.get("signature")?.as_str()?;
    if !site_memory::is_signature(signature) {
        return None;
    }
    let text = |key: &str| {
        form.get(key)
            .and_then(serde_json::Value::as_str)
            .map(bounded)
            .unwrap_or_default()
    };
    let questions = form
        .get("fields")
        .and_then(serde_json::Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .take(site_memory::MAX_QUESTIONS)
                .filter_map(read_question)
                .collect()
        })
        .unwrap_or_default();
    Some(ReadForm {
        id: text("form"),
        shape: RememberedForm {
            signature: signature.to_owned(),
            label: text("label"),
            questions,
            submit: texts(form, "submit", site_memory::MAX_SUBMITS),
            ..RememberedForm::default()
        },
        offers: form
            .get("offers")
            .and_then(serde_json::Value::as_str)
            .filter(|offers| site_memory::is_signature(offers))
            .unwrap_or_default()
            .to_owned(),
        changed: false,
    })
}

/// The forms a read of the page answered with (`data` of `read-page-forms`),
/// as their shapes. No field's value is read: not what it holds, not who
/// set it.
pub fn read(data: &serde_json::Value) -> Vec<ReadForm> {
    data.get("forms")
        .and_then(serde_json::Value::as_array)
        .map(|forms| {
            forms
                .iter()
                .take(site_memory::MAX_PAGE_FORMS)
                .filter_map(read_form)
                .collect()
        })
        .unwrap_or_default()
}

/// Take the answers each question offers out of `data`, the answer of a
/// forms read. On a page that holds a sign-in they may be the person's own
/// (their saved addresses, their accounts), which the window does not
/// publish for a read nobody was admitted for.
pub fn without_offered_answers(data: &mut serde_json::Value) {
    let forms = data
        .get_mut("forms")
        .and_then(serde_json::Value::as_array_mut);
    for form in forms.into_iter().flatten() {
        let fields = form
            .get_mut("fields")
            .and_then(serde_json::Value::as_array_mut);
        for field in fields.into_iter().flatten() {
            if let Some(field) = field.as_object_mut() {
                field.remove("choices");
            }
        }
    }
}

/// A parameter's name for the question `name`: its words in lower case
/// joined by `_`, as the page's other actions name theirs.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let mut slug: String = slug.trim_end_matches('_').chars().take(48).collect();
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        return "question".to_owned();
    }
    if slug.starts_with(|first: char| first.is_numeric()) {
        slug.insert_str(0, "q_");
    }
    slug
}

/// `listed` without what it repeats or leaves empty, in its order.
fn distinct(listed: &[String]) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for item in listed {
        if !item.is_empty() && !kept.contains(item) {
            kept.push(item.clone());
        }
    }
    kept
}

/// What a question of `kind` takes, and as which type.
fn takes(kind: &str) -> (ParamType, &'static str) {
    match kind {
        "flag" => (ParamType::Boolean, "true or false"),
        "number" | "range" => (ParamType::Number, "a number"),
        "choices" => (ParamType::Array, "a list of the answers to choose"),
        "choice" => (ParamType::Text, "one of the answers it offers"),
        "date" => (ParamType::Text, "a date as YYYY-MM-DD"),
        "time" => (ParamType::Text, "a time as HH:MM"),
        "datetime-local" => (ParamType::Text, "a date and time as YYYY-MM-DDTHH:MM"),
        "month" => (ParamType::Text, "a month as YYYY-MM"),
        "week" => (ParamType::Text, "a week as YYYY-Www"),
        "color" => (ParamType::Text, "a colour as #rrggbb"),
        "email" => (ParamType::Text, "an email address"),
        "tel" => (ParamType::Text, "a telephone number"),
        "url" => (ParamType::Text, "a web address"),
        _ => (ParamType::Text, "text"),
    }
}

/// The typed parameter `name` that answers `question`.
fn param(name: String, question: &RememberedQuestion) -> ActionParam {
    let (ty, hint) = takes(&question.kind);
    let description = format!("{}: {hint}", quoted(&question.name));
    let param = if question.required {
        ActionParam::required_owned(name, ty, description)
    } else {
        ActionParam::optional_owned(name, ty, description)
    };
    let choices = distinct(&question.choices);
    match question.kind.as_str() {
        "choice" if !choices.is_empty() => param.with_choices(choices),
        "choices" => {
            let items = if choices.is_empty() {
                serde_json::json!({"type": "string"})
            } else {
                serde_json::json!({"type": "string", "enum": choices})
            };
            param.with_schema(serde_json::json!({"type": "array", "items": items}))
        }
        "date" => param.with_schema(serde_json::json!({"type": "string", "format": "date"})),
        _ => param,
    }
}

/// Whether the typed path places an answer to `question`. A secret and a
/// file never are parameters, whatever was remembered of them.
fn typed(question: &RememberedQuestion) -> bool {
    question.fallback.is_empty() && !matches!(question.kind.as_str(), "secret" | "file")
}

/// The argument a form's action names its form by: no question's.
const FORM: &str = "form";

/// The parameter each question of `form` would be answered through. The
/// names depend on what the form asks alone, in the order it asks it: the
/// same form has the same parameters however it is known and whatever
/// answering it has taught.
fn names(form: &RememberedForm) -> Vec<(String, &RememberedQuestion)> {
    let mut taken: Vec<String> = vec![FORM.to_owned()];
    form.questions
        .iter()
        .map(|question| {
            let base = slug(&question.name);
            let mut name = base.clone();
            let mut number = 1;
            while taken.contains(&name) {
                number += 1;
                name = format!("{base}_{number}");
            }
            taken.push(name.clone());
            (name, question)
        })
        .collect()
}

/// The parameters of `form`'s action, each with the question it answers:
/// one per question the typed path places.
fn asked(form: &RememberedForm) -> Vec<(ActionParam, String)> {
    names(form)
        .into_iter()
        .filter(|(_, question)| typed(question))
        .map(|(name, question)| (param(name, question), question.name.clone()))
        .collect()
}

/// Why `question` is no parameter, and which action takes it instead.
fn refused(question: &RememberedQuestion) -> Option<String> {
    let name = quoted(&question.name);
    let fallback = if question.fallback.is_empty() {
        match question.kind.as_str() {
            "secret" => "secret_field",
            "file" => "needs_files",
            _ => return None,
        }
    } else {
        question.fallback.as_str()
    };
    Some(match fallback {
        "secret_field" => format!("{name} is a secret and takes fill-page-secret"),
        "needs_files" => format!("{name} takes files, through fill-page-files"),
        _ => format!("{name} is written by typing and takes fill-page"),
    })
}

/// `spec`, an effect bound to one form, as that form's own action: listed
/// as `id` and read as `label`, saying `description`, and withdrawn when the
/// form has changed.
fn finished(
    mut spec: ActionSpec,
    id: String,
    label: String,
    description: String,
    standing: Standing,
) -> ActionSpec {
    let ty = spec.type_mut();
    ty.id = id;
    ty.label = label;
    let spec = spec.with_description(description);
    match standing {
        Standing::Changed => spec.unavailable(FORM_CHANGED, WITHDRAWN),
        Standing::Read | Standing::Remembered => spec,
    }
}

/// What the action of `form` says of itself.
fn answer_description(
    form: &RememberedForm,
    standing: Standing,
    then: &[String],
    same: usize,
) -> String {
    let mut said = vec![
        "Places answers in this form by the questions it asks, and says for each whether it \
         was placed and, if not, why and which ref its own action takes."
            .to_owned(),
    ];
    said.push(if form.submit.is_empty() {
        "Nothing is sent.".to_owned()
    } else {
        format!(
            "Nothing is sent: {} sends the form.",
            send_id(&form.signature)
        )
    });
    let fallbacks: Vec<String> = form.questions.iter().filter_map(refused).collect();
    if !fallbacks.is_empty() {
        said.push(format!(
            "Not parameters, since the typed path refuses them: {}.",
            fallbacks.join("; ")
        ));
    }
    if same > 1 {
        said.push(format!(
            "{same} forms of this page ask the same; this answers the first, and \
             fill-page-form answers another by its id."
        ));
    }
    if standing == Standing::Remembered {
        said.push(
            "Published from what was remembered of this site, before this page was read: a \
             form that has changed since is refused with form_changed and nothing is placed."
                .to_owned(),
        );
    }
    if !then.is_empty() {
        let next: Vec<String> = then.iter().map(|label| quoted(label)).collect();
        said.push(format!(
            "After this form was sent, the site asked: {}.",
            next.join(", ")
        ));
    }
    said.join(" ")
}

/// The action that answers `form`: named after it, one typed parameter per
/// question the typed path places. It is the effect that answers a form,
/// bound to this form and asking for its questions.
fn answer_action(
    form: &RememberedForm,
    standing: Standing,
    then: &[String],
    same: usize,
) -> ActionSpec {
    let label = if form.label.is_empty() {
        "Answer the form".to_owned()
    } else {
        form.label.clone()
    };
    let spec = BrowserEffect::AnswerForm {
        run: FormAnswers {
            form: form.signature.clone(),
            answers: serde_json::Map::new(),
        },
    }
    .bound_asking_params(asked(form).into_iter().map(|(param, _)| param).collect());
    finished(
        spec,
        answer_id(&form.signature),
        label,
        answer_description(form, standing, then, same),
        standing,
    )
}

/// The action that sends `form`, when the form has a button that does: the
/// effect that sends a form, bound to this form, its button one of the
/// form's own and asked for only when the form has several.
fn send_action(form: &RememberedForm, standing: Standing) -> Option<ActionSpec> {
    let buttons = distinct(&form.submit);
    if buttons.is_empty() {
        return None;
    }
    let named = buttons.len() > 1;
    let label = if form.label.is_empty() {
        "Send the form".to_owned()
    } else {
        format!("Send: {}", form.label)
    };
    let effect = BrowserEffect::SendForm {
        form: form.signature.clone(),
        button: None,
        cross_origin: None,
    };
    let params = BrowserEffect::declaration(effect.id())?
        .params
        .into_iter()
        .filter(|param| param.name != FORM && (named || param.name != "button"))
        .map(|param| {
            if param.name == "button" {
                param.with_choices(buttons.clone())
            } else {
                param
            }
        })
        .collect();
    let mut said = vec![format!(
        "Sends this form with {} as it is answered now, and completes on the page it opens. \
         A form with a required question empty or an answer the page does not take is not \
         sent, and the answer says which.",
        if named {
            "the button chosen, else its first"
        } else {
            "its button"
        }
    )];
    if standing == Standing::Remembered {
        said.push(
            "Published from what was remembered of this site, before this page was read: a \
             form that has changed since is refused with form_changed and nothing is sent."
                .to_owned(),
        );
    }
    Some(finished(
        effect.bound_asking_params(params),
        send_id(&form.signature),
        label,
        said.join(" "),
        standing,
    ))
}

/// What one window knows of its page's forms.
#[derive(Default)]
pub struct PageForms {
    /// Site memory, as the document holds it.
    memory: BTreeMap<String, RememberedForm>,
    /// The forms the page was last read to ask, and where it was read.
    read: Option<(Place, Vec<ReadForm>)>,
    /// The window has shown another page since that read: the page read is
    /// gone, and coming back to its address opens it anew, unread.
    left: Cell<bool>,
    /// A form this tab sent, by site and signature: the forms the site asks
    /// next follow it.
    sent: Option<(String, String)>,
    /// Remembered forms a run found gone from a page.
    gone: Vec<(Place, String)>,
    /// Form actions in flight, by the form each names.
    runs: Vec<(InvocationId, String)>,
    /// Counts every change to what is published.
    generation: u64,
    /// The actions last built, with the place and generation they are of.
    built: RefCell<Option<(Place, u64, Vec<ActionSpec>)>>,
}

impl PageForms {
    /// Site memory, as the document holds it.
    pub fn memory(&self) -> &BTreeMap<String, RememberedForm> {
        &self.memory
    }

    /// Follow the document's site memory.
    pub fn present(&mut self, memory: &BTreeMap<String, RememberedForm>) {
        if self.memory != *memory {
            self.memory.clone_from(memory);
            self.generation += 1;
        }
    }

    /// The form action `invocation` runs names `signature`.
    pub fn started(&mut self, invocation: InvocationId, signature: &str) {
        if self.runs.len() >= MAX_RUNS {
            self.runs.remove(0);
        }
        self.runs.push((invocation, signature.to_owned()));
    }

    /// The form the form action `invocation` named, once: `None` for an
    /// invocation that is no form action's.
    pub fn take_run(&mut self, invocation: InvocationId) -> Option<String> {
        let index = self.runs.iter().position(|(run, _)| *run == invocation)?;
        Some(self.runs.remove(index).1)
    }

    /// The forms the page at `place` was read to ask, while it is still the
    /// page that was read.
    fn reading(&self, place: &Place) -> Option<&Vec<ReadForm>> {
        match &self.read {
            Some((read_at, forms)) if read_at == place && !self.left.get() => Some(forms),
            _ => None,
        }
    }

    /// The forms the page at `place` publishes, how each is known, and how
    /// many forms of the page ask the same.
    fn published(&self, place: &Place) -> Vec<(&RememberedForm, Standing, usize)> {
        if let Some(forms) = self.reading(place) {
            let mut listed: Vec<(&RememberedForm, Standing, usize)> = Vec::new();
            for form in forms {
                if let Some((_, _, same)) = listed
                    .iter_mut()
                    .find(|(known, _, _)| known.signature == form.shape.signature)
                {
                    *same += 1;
                } else {
                    let standing = if form.changed {
                        Standing::Changed
                    } else {
                        Standing::Read
                    };
                    listed.push((&form.shape, standing, 1));
                }
            }
            return listed;
        }
        site_memory::at_place(&self.memory, place)
            .into_iter()
            .filter(|form| {
                !self
                    .gone
                    .iter()
                    .any(|(at, signature)| at == place && *signature == form.signature)
            })
            .map(|form| (form, Standing::Remembered, 1))
            .collect()
    }

    fn build(&self, place: &Place) -> Vec<ActionSpec> {
        let mut actions = Vec::new();
        for (form, standing, same) in self.published(place) {
            let then: Vec<String> =
                site_memory::followers(&self.memory, &place.origin, &form.signature)
                    .into_iter()
                    .filter(|next| !next.label.is_empty())
                    .map(|next| next.label.clone())
                    .collect();
            actions.push(answer_action(form, standing, &then, same));
            actions.extend(send_action(form, standing));
        }
        actions
    }

    /// The actions the page at `place` publishes for its forms: for each an
    /// action that answers it and, when it has a button, one that sends it.
    /// Built once for what is known, not once a frame.
    pub fn actions(&self, place: &Place) -> Vec<ActionSpec> {
        if let Some((read_at, _)) = &self.read
            && read_at != place
            && !self.left.replace(true)
        {
            // The page that was read went: what was built of it goes too.
            *self.built.borrow_mut() = None;
        }
        let kept = self
            .built
            .borrow()
            .as_ref()
            .filter(|(at, generation, _)| at == place && *generation == self.generation)
            .map(|(_, _, actions)| actions.clone());
        if let Some(actions) = kept {
            return actions;
        }
        let actions = self.build(place);
        *self.built.borrow_mut() = Some((place.clone(), self.generation, actions.clone()));
        actions
    }

    /// How the form `signature` of the page at `place` is published, if it
    /// is.
    pub fn standing(&self, place: &Place, signature: &str) -> Option<Standing> {
        self.published(place)
            .into_iter()
            .find(|(form, _, _)| form.signature == signature)
            .map(|(_, standing, _)| standing)
    }

    /// `answers`, given by the parameters of the action of the form
    /// `signature`, by the names the form asks under: what the engine is
    /// answered with. A form the window knows nothing of keeps them as they
    /// came, and the engine says of each what it could not place.
    pub fn named(
        &self,
        place: &Place,
        signature: &str,
        answers: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let shape = self
            .reading(place)
            .and_then(|forms| forms.iter().find(|form| form.shape.signature == signature))
            .map(|form| &form.shape)
            .or_else(|| site_memory::get(&self.memory, &place.origin, signature));
        let Some(shape) = shape else {
            return answers.clone();
        };
        let names = names(shape);
        answers
            .iter()
            .map(|(param, value)| {
                let name = names
                    .iter()
                    .find(|(name, _)| name == param)
                    .map_or(param, |(_, question)| &question.name);
                (name.clone(), value.clone())
            })
            .collect()
    }

    /// The page at `place` was read, and `data` is what it asks. Its forms
    /// are what the page publishes from now on, in place of what was read
    /// or remembered before. Answers their shapes, to remember.
    pub fn heard_read(&mut self, place: &Place, data: &serde_json::Value) -> Vec<RememberedForm> {
        let mut forms = read(data);
        // A form this tab sent on this site led here: these forms follow it.
        if let Some((origin, sent)) = self.sent.take()
            && origin == place.origin
        {
            for form in &mut forms {
                if form.shape.signature != sent {
                    form.shape.follows.clone_from(&sent);
                }
            }
        }
        // A form that asked more once it was answered has the shape it grew
        // into now: the page opens with the one it grew from.
        if let Some(before) = self.reading(place) {
            for form in &mut forms {
                if let Some(was) = before.iter().find(|was| {
                    was.changed && was.id == form.id && was.shape.signature != form.shape.signature
                }) {
                    form.shape.grew_from.clone_from(&was.shape.signature);
                }
            }
        }
        // What answering taught on an earlier visit holds for the form as
        // read now.
        for form in &mut forms {
            let Some(known) = site_memory::get(&self.memory, &place.origin, &form.shape.signature)
            else {
                continue;
            };
            for question in &mut form.shape.questions {
                if question.fallback.is_empty()
                    && let Some(taught) = known
                        .questions
                        .iter()
                        .find(|taught| taught.name == question.name && taught.kind == question.kind)
                {
                    question.fallback.clone_from(&taught.fallback);
                }
            }
        }
        let shapes = forms.iter().map(|form| form.shape.clone()).collect();
        self.read = Some((place.clone(), forms));
        self.left.set(false);
        self.gone.retain(|(at, _)| at != place);
        self.generation += 1;
        shapes
    }

    /// A form of the page at `place` was answered, and `data` says what
    /// came of it (`data` of `fill-page-form`): which questions the typed
    /// path refused for what they are, whether the form asks more now, and
    /// whether it was sent.
    pub fn heard_answer(&mut self, place: &Place, data: &serde_json::Value) -> Answered {
        let Some(signature) = data
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .filter(|signature| site_memory::is_signature(signature))
        else {
            return Answered::default();
        };
        let refusals: Vec<(&str, &str)> = data
            .get("results")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|result| {
                let code = result.get("code")?.as_str()?;
                let name = result.get("name")?.as_str()?;
                site_memory::FALLBACKS
                    .contains(&code)
                    .then_some((name, code))
            })
            .collect();
        let teach = |shape: &mut RememberedForm| {
            let mut taught = false;
            for (name, code) in &refusals {
                if let Some(question) = shape
                    .questions
                    .iter_mut()
                    .find(|question| question.name == *name && question.fallback != *code)
                {
                    (*code).clone_into(&mut question.fallback);
                    taught = true;
                }
            }
            taught
        };
        // What a read of the form gives once the answers were placed: its
        // signature, which is another one when it asks something else, and
        // the hash of the answers it offers.
        let became = |key: &str| {
            data.get("became")
                .and_then(|became| became.get(key))
                .and_then(serde_json::Value::as_str)
                .filter(|said| site_memory::is_signature(said))
        };
        let grew = data
            .get("now_asks")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|asks| !asks.is_empty())
            || became("signature").is_some_and(|after| after != signature);
        let mut refresh = false;
        let sent = data
            .get("submitted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut origin = place.origin.clone();
        let mut learned = None;
        let mut read_here = false;
        if !self.left.get()
            && let Some((read_at, forms)) = &mut self.read
            && let Some(form) = forms
                .iter_mut()
                .find(|form| form.shape.signature == signature)
        {
            read_here = true;
            origin.clone_from(&read_at.origin);
            form.changed |= grew;
            refresh = !grew
                && !form.offers.is_empty()
                && became("offers").is_some_and(|offers| offers != form.offers);
            if teach(&mut form.shape) {
                learned = Some((read_at.clone(), form.shape.clone()));
            }
        }
        if !read_here && let Some(known) = site_memory::get(&self.memory, &origin, signature) {
            let mut shape = known.clone();
            if teach(&mut shape) {
                // The answer may arrive on the page the form led to: the form
                // is remembered for a page it was read on.
                let page = if shape.pages.contains(&place.page) {
                    place.page.clone()
                } else {
                    shape
                        .pages
                        .last()
                        .cloned()
                        .unwrap_or_else(|| place.page.clone())
                };
                learned = Some((
                    Place {
                        origin: origin.clone(),
                        page,
                    },
                    shape,
                ));
            }
        }
        let then = if sent {
            site_memory::followers(&self.memory, &origin, signature)
                .into_iter()
                .map(|next| (next.label.clone(), next.signature.clone()))
                .collect()
        } else {
            Vec::new()
        };
        if sent {
            self.sent = Some((origin, signature.to_owned()));
        }
        self.generation += 1;
        Answered {
            learned,
            grew,
            refresh,
            then,
        }
    }

    /// Whether what the engine says of the page's forms (`told`, the forms
    /// of a `page_changed`) is not what the page at `place` publishes: a
    /// form the read did not find, one that asks or offers something else
    /// now, one that is gone. A page that was not read publishes nothing
    /// that was read from it, so any form the engine names is news.
    pub fn told_otherwise(&self, place: &Place, told: &[ui::webview::PageFormChange]) -> bool {
        let Some(read) = self.reading(place) else {
            return !told.is_empty();
        };
        told.iter().any(|told| {
            let Some(known) = read.iter().find(|form| form.id == told.form) else {
                return !told.gone;
            };
            told.gone
                || known.changed
                || told.signature.as_deref() != Some(known.shape.signature.as_str())
                || told
                    .offers
                    .as_deref()
                    .is_some_and(|offers| !known.offers.is_empty() && offers != known.offers)
        })
    }

    /// A form action found no form `signature` on the page at `place`: the
    /// form changed, or this page never held it. Its action is withdrawn
    /// until the page is read, which its window does next. Answers the
    /// form's name and how it was known.
    pub fn heard_gone(&mut self, place: &Place, signature: &str) -> (String, Standing) {
        let mut label = String::new();
        let mut standing = Standing::Remembered;
        if !self.left.get()
            && let Some((read_at, forms)) = &mut self.read
            && read_at == place
        {
            for form in forms
                .iter_mut()
                .filter(|form| form.shape.signature == signature)
            {
                form.changed = true;
                label.clone_from(&form.shape.label);
                standing = Standing::Read;
            }
        }
        if standing == Standing::Remembered {
            if let Some(known) = site_memory::get(&self.memory, &place.origin, signature) {
                label.clone_from(&known.label);
            }
            let noted = self
                .gone
                .iter()
                .any(|(at, gone)| at == place && gone == signature);
            if !noted {
                if self.gone.len() >= MAX_GONE {
                    self.gone.remove(0);
                }
                self.gone.push((place.clone(), signature.to_owned()));
            }
        }
        self.generation += 1;
        (label, standing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visa() -> serde_json::Value {
        serde_json::json!({"forms": [{
            "form": "form1", "label": "Apply for a visitor visa", "method": "POST",
            "action": "/apply", "signature": "5f1c2a9b", "submit": ["Send application"],
            "fields": [
                {"name": "Full name", "kind": "text", "required": true, "value": "Ada Lovelace",
                    "element": "d1e2"},
                {"name": "Date of birth: Day", "kind": "number", "value": "", "element": "d1e3"},
                {"name": "Arrival", "kind": "date", "required": true, "value": "",
                    "element": "d1e4"},
                {"name": "Nationality", "kind": "choice", "choices": ["Kenya", "Other"],
                    "value": "", "element": "d1e5"},
                {"name": "Languages", "kind": "choices",
                    "choices": ["English", "Dutch", "German"], "value": [],
                    "elements": ["d1e6", "d1e7", "d1e8"]},
                {"name": "I agree", "kind": "flag", "required": true, "value": false,
                    "element": "d1e9"},
                {"name": "Passcode", "kind": "secret", "required": true,
                    "value": "<4 characters hidden>", "element": "d1e10"},
                {"name": "Passport scan", "kind": "file", "value": [], "element": "d1e11"},
            ],
        }]})
    }

    fn place(url: &str) -> Place {
        Place::of(url).unwrap()
    }

    fn params(spec: &ActionSpec) -> Vec<(String, ParamType, bool)> {
        spec.params()
            .iter()
            .map(|param| (param.name.to_string(), param.ty, param.required))
            .collect()
    }

    /// A read keeps a form's shape and none of what its fields hold.
    #[test]
    fn a_read_keeps_the_shape_and_no_value() {
        let forms = read(&visa());
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].id, "form1");
        let shape = &forms[0].shape;
        assert_eq!(shape.label, "Apply for a visitor visa");
        assert_eq!(shape.submit, ["Send application"]);
        assert_eq!(shape.questions.len(), 8);
        assert_eq!(shape.questions[6].fallback, "secret_field");
        assert_eq!(shape.questions[7].fallback, "needs_files");
        let kept = serde_json::to_string(shape).unwrap();
        for held in ["Ada Lovelace", "characters hidden", "d1e2"] {
            assert!(!kept.contains(held), "{held} is no part of a form's shape");
        }
        // A form without a signature the engine could print is not one.
        assert!(read(&serde_json::json!({"forms": [{"signature": "no good"}]})).is_empty());
        assert!(read(&serde_json::json!({})).is_empty());
    }

    /// On a page that holds a sign-in the window's own read publishes a
    /// form's questions without the answers they offer.
    #[test]
    fn a_read_without_offered_answers_keeps_every_question() {
        let mut data = visa();
        without_offered_answers(&mut data);
        let forms = read(&data);
        assert_eq!(forms[0].shape.questions.len(), 8);
        assert!(
            forms[0]
                .shape
                .questions
                .iter()
                .all(|question| question.choices.is_empty())
        );
        let mut page = PageForms::default();
        let at = place("https://visa.test/apply");
        page.heard_read(&at, &data);
        let answer = &page.actions(&at)[0];
        let nationality = answer
            .params()
            .iter()
            .find(|param| param.name == "nationality")
            .expect("the question is still a parameter");
        assert!(nationality.choices.is_empty());
        // Data that is no forms read is left as it is.
        let mut other = serde_json::json!({"forms": "none"});
        without_offered_answers(&mut other);
        assert_eq!(other, serde_json::json!({"forms": "none"}));
    }

    /// A parameter is named by its question's words, and two questions
    /// never share a name.
    #[test]
    fn parameters_are_named_by_their_questions() {
        assert_eq!(slug("Date of birth: Day"), "date_of_birth_day");
        assert_eq!(slug("  E-mail (work)  "), "e_mail_work");
        assert_eq!(slug("2nd name"), "q_2nd_name");
        assert_eq!(slug("???"), "question");
        assert_eq!(slug("Straße"), "straße");
        let question = |name: &str, kind: &str| RememberedQuestion {
            name: name.into(),
            kind: kind.into(),
            ..RememberedQuestion::default()
        };
        let twice = RememberedForm {
            signature: "aa".into(),
            questions: vec![
                question("Name", "secret"),
                question("name!", "text"),
                question("Form", "text"),
            ],
            ..RememberedForm::default()
        };
        let named: Vec<String> = asked(&twice)
            .into_iter()
            .map(|(param, _)| param.name.to_string())
            .collect();
        assert_eq!(
            named,
            ["name_2", "form_2"],
            "a question the typed path refuses keeps its name, and `form` is the action's own"
        );
    }

    /// A read form is an action named after it with one typed parameter per
    /// question; a secret and a file are no parameters and are said to take
    /// their own actions.
    #[test]
    fn a_form_is_an_action_with_a_typed_parameter_per_question() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        forms.heard_read(&at, &visa());
        let actions = forms.actions(&at);
        let ids: Vec<&str> = actions.iter().map(|action| action.id.as_str()).collect();
        assert_eq!(ids, ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
        let answer = &actions[0];
        assert_eq!(answer.label, "Apply for a visitor visa");
        assert_eq!(
            params(answer),
            [
                ("full_name".to_owned(), ParamType::Text, true),
                ("date_of_birth_day".to_owned(), ParamType::Number, false),
                ("arrival".to_owned(), ParamType::Text, true),
                ("nationality".to_owned(), ParamType::Text, false),
                ("languages".to_owned(), ParamType::Array, false),
                ("i_agree".to_owned(), ParamType::Boolean, true),
            ]
        );
        assert_eq!(answer.params()[3].choices, ["Kenya", "Other"]);
        let description = answer.description.clone().unwrap();
        assert!(
            description.contains("\"Passcode\" is a secret and takes fill-page-secret"),
            "{description}"
        );
        assert!(
            description.contains("\"Passport scan\" takes files, through fill-page-files"),
            "{description}"
        );
        assert!(description.contains("send-form-5f1c2a9b sends the form"));
        assert!(answer.contract.validate_declaration().is_empty());
        assert!(answer.contract.availability.is_available());
        assert!(answer.reaches_people() && answer.reaches_agents());

        // Typed: a wrong type, an answer the question does not offer, a date
        // that is none and a missing required answer are refused before
        // anything reaches the page.
        let full = serde_json::json!({
            "full_name": "Ada Lovelace", "arrival": "2027-02-03", "i_agree": true,
        });
        let with = |name: &str, value: serde_json::Value| {
            let mut args = full.as_object().unwrap().clone();
            args.insert(name.into(), value);
            answer.build_json(&args)
        };
        assert!(answer.build_json(full.as_object().unwrap()).is_ok());
        assert!(with("i_agree", serde_json::json!("yes")).is_err());
        assert!(with("nationality", serde_json::json!("Mars")).is_err());
        assert!(with("languages", serde_json::json!(["Klingon"])).is_err());
        assert!(with("arrival", serde_json::json!("next week")).is_err());
        assert!(with("passcode", serde_json::json!("0000")).is_err());
        assert!(
            answer
                .build_json(serde_json::json!({"full_name": "Ada"}).as_object().unwrap())
                .is_err()
        );
    }

    /// Running the action names the form by signature and answers it by
    /// the names the form asks under; sending it is the other action.
    #[test]
    fn the_actions_run_as_answers_by_the_forms_own_names() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        forms.heard_read(&at, &visa());
        let actions = forms.actions(&at);
        let run = |action: &ActionSpec, args: serde_json::Value| {
            let encoded = action.build_json(args.as_object().unwrap()).unwrap();
            match BrowserEffect::from_payload(&encoded.payload)
                .unwrap()
                .operation()
            {
                crate::effects::BrowserOperation::Form(run) => run,
                other => panic!("a form action runs a form, got {other:?}"),
            }
        };
        let answered = run(
            &actions[0],
            serde_json::json!({
                "full_name": "Ada Lovelace", "arrival": "2027-02-03", "i_agree": true,
                "date_of_birth_day": 10, "languages": ["English", "German"],
            }),
        );
        assert_eq!(answered.form, "5f1c2a9b");
        assert!(answered.answers.contains_key("full_name"));
        assert_eq!(
            answered.request(forms.named(&at, &answered.form, &answered.answers)),
            PageRequest::FillForm {
                form: "5f1c2a9b".into(),
                answers: serde_json::json!({
                    "Full name": "Ada Lovelace", "Arrival": "2027-02-03", "I agree": true,
                    "Date of birth: Day": 10, "Languages": ["English", "German"],
                })
                .as_object()
                .unwrap()
                .clone(),
                submit: FormSubmit::No,
                cross_origin: false,
                frame: None,
            }
        );
        let sent = run(&actions[1], serde_json::json!({"cross_origin": true}));
        assert_eq!(
            sent.request(forms.named(&at, &sent.form, &sent.answers)),
            PageRequest::FillForm {
                form: "5f1c2a9b".into(),
                answers: serde_json::Map::new(),
                submit: FormSubmit::First,
                cross_origin: true,
                frame: None,
            }
        );
    }

    /// What the engine tells of the page's forms is news only when it is
    /// not what the page publishes: a form no read found, one that asks or
    /// offers something else, one that is gone.
    #[test]
    fn what_the_engine_tells_of_a_form_is_news_only_when_the_page_publishes_otherwise() {
        let told = |form: &str, signature: Option<&str>, offers: Option<&str>, gone: bool| {
            [ui::webview::PageFormChange {
                form: form.into(),
                signature: signature.map(str::to_owned),
                offers: offers.map(str::to_owned),
                gone,
            }]
        };
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        assert!(!forms.told_otherwise(&at, &[]));
        assert!(
            forms.told_otherwise(&at, &told("form1", Some("5f1c2a9b"), None, false)),
            "a page nobody read publishes nothing that was read"
        );
        let mut data = visa();
        data["forms"][0]["offers"] = "aaaa1111".into();
        forms.heard_read(&at, &data);
        for (news, change) in [
            (
                false,
                told("form1", Some("5f1c2a9b"), Some("aaaa1111"), false),
            ),
            (false, told("form1", Some("5f1c2a9b"), None, false)),
            (
                true,
                told("form1", Some("0badc0de"), Some("aaaa1111"), false),
            ),
            (
                true,
                told("form1", Some("5f1c2a9b"), Some("bbbb2222"), false),
            ),
            (true, told("form1", None, None, true)),
            (true, told("form2", Some("0a11ce00"), None, false)),
            (false, told("form2", None, None, true)),
        ] {
            assert_eq!(forms.told_otherwise(&at, &change), news, "{change:?}");
        }
        // Another page's forms are not this page's.
        let elsewhere = place("https://visa.test/status");
        assert!(forms.told_otherwise(
            &elsewhere,
            &told("form1", Some("5f1c2a9b"), Some("aaaa1111"), false)
        ));
    }

    /// The actions follow the page: another page has none of them, and a
    /// read that finds another form replaces them.
    #[test]
    fn the_actions_follow_the_page() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        forms.heard_read(&at, &visa());
        assert_eq!(forms.actions(&at).len(), 2);
        assert!(
            forms
                .actions(&place("https://visa.test/elsewhere"))
                .is_empty()
        );
        assert!(forms.actions(&place("https://other.test/apply")).is_empty());
        assert!(
            forms.actions(&at).is_empty(),
            "the page that was read went: its address opens another, unread"
        );
        forms.heard_read(&at, &visa());
        assert_eq!(forms.actions(&at).len(), 2);
        let mut changed = visa();
        changed["forms"][0]["signature"] = "0badc0de".into();
        changed["forms"][0]["submit"] = serde_json::json!([]);
        forms.heard_read(&at, &changed);
        let ids: Vec<String> = forms
            .actions(&at)
            .iter()
            .map(|action| action.id.clone())
            .collect();
        assert_eq!(ids, ["form-0badc0de"], "a form with no button has no send");
    }

    /// An answer that makes the form ask more withdraws its action until
    /// the page is read, which its window does next, and the shape it has
    /// then grew from the one it had.
    #[test]
    fn a_form_that_asks_more_is_withdrawn_until_it_is_read_again() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        forms.heard_read(&at, &visa());
        let heard = forms.heard_answer(
            &at,
            &serde_json::json!({
                "form": "form1", "signature": "5f1c2a9b",
                "results": [{"name": "Nationality", "ok": true}],
                "set": [{"name": "Nationality", "value": "Other"}],
                "submitted": false, "now_asks": ["Which country?"], "unanswered": [],
            }),
        );
        assert!(heard.grew && heard.learned.is_none() && heard.then.is_empty());
        assert_eq!(forms.standing(&at, "5f1c2a9b"), Some(Standing::Changed));
        let withdrawn = &forms.actions(&at)[0];
        assert!(!withdrawn.contract.availability.is_available());
        let mut grown = visa();
        grown["forms"][0]["signature"] = "0badc0de".into();
        let shapes = forms.heard_read(&at, &grown);
        assert_eq!(shapes[0].grew_from, "5f1c2a9b");
        assert_eq!(forms.standing(&at, "0badc0de"), Some(Standing::Read));
    }

    /// What a field refused for what it is (an editor written by typing) is
    /// taught once, and the question is no parameter from then on.
    #[test]
    fn a_field_the_typed_path_refused_is_no_parameter_afterwards() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        forms.heard_read(&at, &visa());
        let heard = forms.heard_answer(
            &at,
            &serde_json::json!({
                "form": "form1", "signature": "5f1c2a9b",
                "results": [
                    {"name": "Full name", "ok": false, "code": "needs_typing",
                        "message": "\"Full name\" is written by typing", "element": "d1e2"},
                    {"name": "Nationality", "ok": false, "code": "no_option",
                        "message": "no answer \"Mars\""},
                ],
                "set": [], "submitted": false, "now_asks": [], "unanswered": [],
            }),
        );
        let (learned_at, learned) = heard.learned.expect("the refusal is the field's own");
        assert_eq!(learned_at, at);
        assert_eq!(learned.questions[0].fallback, "needs_typing");
        assert_eq!(
            learned.questions[3].fallback, "",
            "an answer the question does not offer says nothing of the field"
        );
        let answer = &forms.actions(&at)[0];
        assert!(
            !params(answer)
                .iter()
                .any(|(name, _, _)| name == "full_name")
        );
        assert!(
            answer
                .description
                .clone()
                .unwrap()
                .contains("\"Full name\" is written by typing and takes fill-page")
        );
    }

    /// An answer says what the form became. One that became another form
    /// grew, though it opened no question by name; one that asks the same
    /// and offers other answers is to be read again; one that is what the
    /// page publishes teaches nothing.
    #[test]
    fn an_answer_says_whether_the_form_became_another_or_offers_other_answers() {
        let mut forms = PageForms::default();
        let at = place("https://visa.test/apply");
        let mut data = visa();
        data["forms"][0]["offers"] = "aaaa1111".into();
        forms.heard_read(&at, &data);
        let answered = |became: serde_json::Value| {
            serde_json::json!({"form": "form1", "signature": "5f1c2a9b", "results": [],
                "set": [], "submitted": false, "now_asks": [], "unanswered": [],
                "became": became})
        };
        for (became, grew, refresh) in [
            (
                serde_json::json!({"signature": "5f1c2a9b", "offers": "aaaa1111"}),
                false,
                false,
            ),
            (
                serde_json::json!({"signature": "5f1c2a9b", "offers": "bbbb2222"}),
                false,
                true,
            ),
            // What no engine prints is not taken for a signature.
            (
                serde_json::json!({"signature": "no good", "offers": 7}),
                false,
                false,
            ),
            (serde_json::json!(null), false, false),
        ] {
            let heard = forms.heard_answer(&at, &answered(became.clone()));
            assert_eq!((heard.grew, heard.refresh), (grew, refresh), "{became}");
            assert_eq!(forms.standing(&at, "5f1c2a9b"), Some(Standing::Read));
        }
        let heard = forms.heard_answer(
            &at,
            &answered(serde_json::json!({"signature": "0badc0de", "offers": "bbbb2222"})),
        );
        assert!(heard.grew && !heard.refresh);
        assert_eq!(forms.standing(&at, "5f1c2a9b"), Some(Standing::Changed));
    }
}
