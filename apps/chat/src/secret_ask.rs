//! A secret asked for in the conversation: the kind [`SECRET`] of the
//! question a message asks ([`crate::question`]).
//!
//! Whoever acts for a person asks with the question's own verb (`ask`, kind
//! `secret`), and says in its detail what is asked for and where it belongs
//! ([`Asked`]). The card says so in the system's words, never the asker's.
//! To the person asked, on a device of theirs, Chat's window shows the
//! protected field ([`ui::widget::SecretPrompt`]) under the card. What they
//! type goes from that device to their own vault on the host their workspace
//! runs on (`tilo.vault`, which refuses every agent) and nowhere else. The
//! question is then answered [`FILLED`], with the reference the vault
//! answered in the answer's detail: the item's address, which names it and
//! is not the secret. The field's value never reaches the document, the
//! accessibility tree, presence or a log, and anyone else reading the
//! conversation sees the card without a field.
//!
//! One field is shown at a time: the newest question that waits for this
//! person. It waits as long as they leave it, as every question does.

use serde::{Deserialize, Serialize};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{InvokeArgument, person_vault};
use ui::Widget as _;
use ui::paint::PaintCtx;
use ui::widget::{FieldRole, Presentation, PromptOutcome, SecretAnswer, SecretPrompt};
use ui::{Arrival, InvocationCall};
use vello::kurbo::Rect;
use winit::event::{Modifiers, WindowEvent};

use crate::app::{ChatApp, Hit};
use crate::i18n::{t, t_args};
use crate::messages::{NewMessage, chat_messages};
use crate::model::Message;
use crate::parts::ChatMessages;
use crate::question::{DECLINED, FILLED, Question, QuestionState, SECRET};
use crate::question_card::{Line, QuestionShown, Reader};

/// The most places one question names, as the vault keeps at most.
const MAX_PLACES: usize = 8;
/// The longest name a question says a secret signs in with.
const MAX_NAME_BYTES: usize = 320;
/// The most a question says of where a key rides in a request.
const MAX_SENT_IN_BYTES: usize = 128;

/// What a [`SECRET`] question asks for, as its detail keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asked {
    /// What it is: a `login`, a `key`, a `code-generator`'s setup key, an
    /// `ssh-key` or a plain `value`.
    pub kind: person_vault::ItemKind,
    /// Where it belongs: a site's or a service's web address, or a server
    /// (`imap://mail.example.com:993`). The vault keeps the secret for these
    /// places and hands it out nowhere else.
    pub belongs_to: Vec<String>,
    /// The name it signs in with, when whoever asks knows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    /// Where a key rides in a request to its service, when not as a bearer
    /// token: `basic`, `header:<name>` or `query:<name>`. Only a key says
    /// so; the vault that keeps it decides whether it is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sent_in: String,
}

impl Asked {
    /// What `detail` asks for.
    ///
    /// # Errors
    /// It names no kind, no place a secret could belong to, or a name that
    /// is none: said in words whoever asked can act on.
    pub fn read(detail: Option<&serde_json::Value>) -> Result<Self, &'static str> {
        let asked: Self = detail
            .cloned()
            .and_then(|detail| serde_json::from_value(detail).ok())
            .ok_or(
                "a question for a secret says in its detail what is asked for: kind (login, key, code-generator, ssh-key or value), belongsTo and, when known, username",
            )?;
        if asked.belongs_to.is_empty()
            || asked.belongs_to.len() > MAX_PLACES
            || !asked
                .belongs_to
                .iter()
                .all(|place| person_vault::looks_like_place(place))
        {
            return Err(
                "say where the secret belongs: a web address such as https://example.com, or a server such as imap://mail.example.com:993",
            );
        }
        if asked.username.len() > MAX_NAME_BYTES || asked.username.chars().any(char::is_control) {
            return Err("that is not a name to sign in with");
        }
        if asked.sent_in.len() > MAX_SENT_IN_BYTES
            || asked
                .sent_in
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || (!asked.sent_in.is_empty() && asked.kind != person_vault::ItemKind::Key)
        {
            return Err(
                "only a key says how it is sent, as sentIn: basic, header:<name> or query:<name>",
            );
        }
        Ok(asked)
    }

    /// What `question` asks for, when it asks for a secret a vault can keep.
    #[must_use]
    pub fn of(question: &Question) -> Option<Self> {
        (question.kind.0 == SECRET)
            .then(|| Self::read(question.kept().as_ref()).ok())
            .flatten()
    }
}

/// The kinds a secret's detail names, as it writes them.
const KINDS: [&str; 5] = ["login", "key", "code-generator", "ssh-key", "value"];
/// What is often written where a detail says `belongsTo`.
const FOR_BELONGS_TO: [&str; 9] = [
    "belongs_to",
    "belongsto",
    "places",
    "place",
    "url",
    "urls",
    "site",
    "server",
    "origin",
];
/// What is often written where a detail says `username`.
const FOR_USERNAME: [&str; 5] = ["user", "user_name", "userName", "login", "name"];
/// What stands in the arguments written out where a kind has to be chosen.
const KIND_HERE: &str = "<login, key, code-generator, ssh-key or value>";
/// What stands there where a place has to be written.
const PLACE_HERE: &str = "<https://the-site or imap://the-server:993>";
/// The longest words of a question written out whole in the arguments that
/// work.
const MOST_WORDS_BYTES: usize = 600;

/// The texts `value` holds: itself, or each text of a list.
fn texts_of(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(one) => vec![one.clone()],
        serde_json::Value::Array(many) => many
            .iter()
            .filter_map(|one| one.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// The kind the arguments that work name, from the one that was given;
/// what was wrong with it joins `wrong`.
fn kind_written(given: Option<&serde_json::Value>, wrong: &mut Vec<String>) -> String {
    match given.and_then(serde_json::Value::as_str) {
        Some(kind) if KINDS.contains(&kind) => kind.to_owned(),
        Some("password") => {
            wrong.push("the `kind` `password` is written `login`".to_owned());
            "login".to_owned()
        }
        Some(other) => {
            wrong.push(format!(
                "`kind` is one of login, key, code-generator, ssh-key and value, and yours is \
                 `{other}`"
            ));
            KIND_HERE.to_owned()
        }
        None => {
            wrong.push(
                "`kind` is missing: one of login, key, code-generator, ssh-key and value"
                    .to_owned(),
            );
            KIND_HERE.to_owned()
        }
    }
}

/// The places the arguments that work name, from what was given; what was
/// wrong with them joins `wrong`.
fn places_written(given: Option<&serde_json::Value>, wrong: &mut Vec<String>) -> Vec<String> {
    let given = given.map(texts_of).unwrap_or_default();
    if given.is_empty() {
        wrong.push("`belongsTo` is missing: the list of places the secret belongs to".to_owned());
        return vec![PLACE_HERE.to_owned()];
    }
    given
        .into_iter()
        .map(|place| {
            if person_vault::looks_like_place(&place) {
                place
            } else {
                wrong.push(format!(
                    "a place is written with what it is reached by, such as \
                     https://example.com or imap://mail.example.com:993, and yours is `{place}`"
                ));
                PLACE_HERE.to_owned()
            }
        })
        .collect()
}

/// What whoever asked for a secret is told when the question is refused for
/// its detail: `why`, what was wrong with the detail it gave, key by key,
/// and the arguments that work, written out whole with the channel, the
/// words and whatever of the detail it gave was usable. Its next call is
/// then the right one.
#[must_use]
pub fn call_that_works(
    why: &str,
    channel: &str,
    words: &str,
    given: Option<&serde_json::Value>,
) -> String {
    let nothing = serde_json::Map::new();
    let mut wrong: Vec<String> = Vec::new();
    let given = given
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| {
            wrong.push("it is an object, and yours is none".to_owned());
            &nothing
        });
    let mut places = given.get("belongsTo");
    let mut username = given.get("username").and_then(serde_json::Value::as_str);
    for (key, value) in given {
        match key.as_str() {
            "kind" | "belongsTo" | "username" | "sentIn" => {}
            other if FOR_BELONGS_TO.contains(&other) => {
                wrong.push(format!("`{other}` is written `belongsTo`"));
                places = places.or(Some(value));
            }
            other if FOR_USERNAME.contains(&other) => {
                wrong.push(format!("`{other}` is written `username`"));
                username = username.or_else(|| value.as_str());
            }
            other => wrong.push(format!("it takes no `{other}`: leave that out")),
        }
    }
    let mut detail = serde_json::Map::new();
    let kind = kind_written(given.get("kind"), &mut wrong);
    detail.insert("kind".to_owned(), kind.into());
    let places = places_written(places, &mut wrong);
    detail.insert("belongsTo".to_owned(), places.into());
    if let Some(username) = username {
        detail.insert("username".to_owned(), username.into());
    }
    if let Some(sent) = given.get("sentIn").and_then(serde_json::Value::as_str) {
        detail.insert("sentIn".to_owned(), sent.into());
    }
    let words = if words.len() <= MOST_WORDS_BYTES {
        words
    } else {
        "<your words>"
    };
    let call = serde_json::json!({
        "channel": channel,
        "text": words,
        "kind": SECRET,
        "detail": detail,
    });
    let wrong = if wrong.is_empty() {
        String::new()
    } else {
        format!(" In your detail: {}.", wrong.join("; "))
    };
    format!(
        "{why}.{wrong} Ask again with exactly these arguments, with your own value where one \
         stands in angle brackets: {call}"
    )
}

/// A place as a person reads it: a site by its name, a server as it is.
fn shown_place(place: &str) -> &str {
    place.strip_prefix("https://").map_or(place, |site| {
        site.split_once('/').map_or(site, |(host, _)| host)
    })
}

/// What `asked` asks for and where it belongs, in the system's words.
pub(crate) fn title(asked: &Asked) -> String {
    let key = match asked.kind {
        person_vault::ItemKind::Login => "chat-secret-title-login",
        person_vault::ItemKind::Key => "chat-secret-title-key",
        person_vault::ItemKind::CodeGenerator => "chat-secret-title-code-generator",
        person_vault::ItemKind::SshKey => "chat-secret-title-ssh-key",
        person_vault::ItemKind::Value => "chat-secret-title-value",
    };
    let places = asked
        .belongs_to
        .iter()
        .map(|place| shown_place(place))
        .collect::<Vec<_>>()
        .join(", ");
    t_args(key, &[("place", places.into())])
}

/// What the protected field of a `kind` is called.
fn field_label(kind: person_vault::ItemKind) -> String {
    t(match kind {
        person_vault::ItemKind::Login => "chat-secret-field-login",
        person_vault::ItemKind::Key => "chat-secret-field-key",
        person_vault::ItemKind::CodeGenerator => "chat-secret-field-code-generator",
        person_vault::ItemKind::SshKey => "chat-secret-field-ssh-key",
        person_vault::ItemKind::Value => "chat-secret-field-value",
    })
}

/// What a [`SECRET`] question shows `reader`: what is asked for, and how it
/// stands. It offers no buttons: the person asked answers in the protected
/// field Chat's window draws under the card.
#[must_use]
pub fn shown(question: &Question, reader: &Reader<'_>) -> QuestionShown {
    let asked = Asked::of(question);
    let mut shown = QuestionShown {
        card: true,
        ..QuestionShown::default()
    };
    shown.lines.push((
        asked
            .as_ref()
            .map_or_else(|| t("chat-secret-unreadable"), title),
        Line::Title,
    ));
    let state = match question.state() {
        QuestionState::Waiting if asked.is_none() => None,
        QuestionState::Waiting if reader.mine => Some((t("chat-secret-waiting-mine"), Line::Field)),
        QuestionState::Waiting => Some((
            reader.name.map_or_else(
                || t("chat-question-waiting-someone"),
                |name| t_args("chat-question-waiting", &[("name", name.into())]),
            ),
            Line::Status,
        )),
        QuestionState::Answered(given) | QuestionState::Settled(given, _) => Some((
            t(if given.answer.0 == FILLED {
                "chat-secret-filled"
            } else {
                "chat-secret-declined"
            }),
            Line::Status,
        )),
        QuestionState::Withdrawn(withdrawn) => Some((
            if withdrawn.why.is_empty() {
                t("chat-question-withdrawn")
            } else {
                t_args(
                    "chat-question-withdrawn-why",
                    &[("why", withdrawn.why.clone().into())],
                )
            },
            Line::Status,
        )),
    };
    shown.lines.extend(state);
    shown
}

/// The protected field Chat's window shows for one question, and the vault's
/// answer to what was typed there while it is on its way.
#[derive(Default)]
pub(crate) struct SecretEntry {
    /// The question whose field is shown, by its message, and the field.
    open: std::cell::RefCell<Option<(String, Box<SecretPrompt>)>>,
    /// Whether that field has the keyboard.
    pub(crate) focused: bool,
    /// The vault's answer on its way, for the question it answers.
    saving: Option<(String, InvocationCall)>,
}

impl SecretEntry {
    /// Whether what was typed is on its way to the vault.
    pub(crate) fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The question whose field is shown.
    pub(crate) fn shown_for(&self) -> Option<String> {
        self.open
            .borrow()
            .as_ref()
            .map(|(message, _)| message.clone())
    }

    /// What the field hands out once `text` is typed into its `role`, for a
    /// test that stands in for the person's keys.
    #[cfg(test)]
    pub(crate) fn type_for_test(&self, role: FieldRole, text: &str) -> SecretAnswer {
        let mut open = self.open.borrow_mut();
        let (_, prompt) = open.as_mut().expect("a field is shown");
        assert!(prompt.edit_field(role, |field| field.set_text(text)));
        prompt.submit();
        match prompt.take_outcome() {
            Some(PromptOutcome::Submit(answer)) => answer,
            other => panic!("the field hands out what was typed: {other:?}"),
        }
    }
}

/// The protected field for what `asked` asks, under the message `message`.
fn prompt_for(message: &str, asked: &Asked) -> SecretPrompt {
    let login = asked.kind == person_vault::ItemKind::Login;
    let prompt = SecretPrompt::new(if login {
        ui::widget::SecretKind::UsernameAndSecret
    } else {
        ui::widget::SecretKind::Secret
    })
    .with_presentation(Presentation::Inline)
    .with_id(&format!("chat-secret-{message}"))
    .with_title(title(asked))
    .with_label(FieldRole::Secret, field_label(asked.kind))
    .with_primary_label(t("chat-secret-save"))
    .with_cancel_label(t("chat-secret-decline"));
    if login {
        prompt.with_username(&asked.username)
    } else {
        prompt
    }
}

impl ChatApp {
    /// What `m` asks this person for, when it asks them for a secret they
    /// have not answered.
    pub(crate) fn asks_me_for_a_secret(&self, m: &Message) -> Option<Asked> {
        let question = m.question.as_ref()?;
        (question.waiting() && self.is_me(&question.person))
            .then(|| Asked::of(question))
            .flatten()
    }

    /// What the protected field under the message `message` is called.
    pub(crate) fn secret_field_name(&self, message: &str) -> String {
        self.message(message)
            .and_then(|m| m.question.as_ref())
            .and_then(Asked::of)
            .map_or_else(String::new, |asked| title(&asked))
    }

    /// The width the field under a question takes in a column `width` wide:
    /// its card's.
    fn secret_field_width(width: f64, theme: &ui::theme::Theme) -> f64 {
        width.clamp(0.0, crate::live_share::max_card_width(theme).max(1.0))
    }

    /// The height the protected field under `m` takes in a column `width`
    /// wide, the gap under it included; zero when this window shows none
    /// there.
    pub(crate) fn secret_field_height(
        &self,
        m: &Message,
        width: f64,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) -> f64 {
        let mut open = self.secret_entry.open.borrow_mut();
        match open.as_mut() {
            Some((message, prompt)) if *message == m.id => {
                prompt.preferred_height(Self::secret_field_width(width, theme), tr, theme)
                    + f64::from(theme.tokens.spacing.s2)
            }
            _ => 0.0,
        }
    }

    /// Paint the protected field under `m`'s card from `top` in a column at
    /// `x`, `width` wide, where this window shows one there; answers where
    /// what follows starts.
    pub(crate) fn draw_secret_field(
        &self,
        paint: &mut PaintCtx<'_>,
        m: &Message,
        (x, top, width): (f64, f64, f64),
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let theme = paint.theme;
        let mut open = self.secret_entry.open.borrow_mut();
        let Some((_, prompt)) = open.as_mut().filter(|(message, _)| *message == m.id) else {
            return top;
        };
        let wide = Self::secret_field_width(width, theme);
        let height = prompt.preferred_height(wide, paint.text, theme);
        let field = Rect::new(x, top, x + wide, top + height);
        prompt.set_focus(self.secret_entry.focused && self.host_focused);
        prompt.place(field, paint.text, theme);
        prompt.paint(paint.scene, paint.transform, paint.text, theme);
        drop(open);
        ChatApp::push_hit(hits, field, Hit::SecretField(m.id.clone()));
        field.y1 + f64::from(theme.tokens.spacing.s2)
    }

    /// Keep the protected field with the newest question that waits for this
    /// person, and take the vault's answer to what they typed.
    pub(crate) fn follow_secret_asks(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.settle_secret_save(doc);
        let shown = self.secret_entry.shown_for();
        // The field stays with the question it was shown for while that one
        // waits: what the person began to type is not taken from them.
        if shown
            .as_deref()
            .and_then(|message| self.message(message))
            .is_some_and(|m| self.asks_me_for_a_secret(m).is_some())
        {
            return;
        }
        let next = self.messages.iter().rev().find_map(|m| {
            let asked = self.asks_me_for_a_secret(m)?;
            Some((m.id.clone(), Box::new(prompt_for(&m.id, &asked))))
        });
        if shown.is_some() || next.is_some() {
            self.close_secret_field();
            *self.secret_entry.open.borrow_mut() = next;
            self.request_relayout();
        }
    }

    /// Take the field away, wiping what it held.
    fn close_secret_field(&mut self) {
        let closed = self.secret_entry.open.borrow_mut().take();
        if let Some((_, mut prompt)) = closed {
            prompt.wipe();
        }
        if self.secret_entry.focused {
            self.secret_entry.focused = false;
            self.set_composer_focus();
        }
    }

    /// Hand `event` to the protected field and act on what the person
    /// decided there. Whether the field took the event.
    pub(crate) fn route_secret_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let (taken, decided) = {
            let mut open = self.secret_entry.open.borrow_mut();
            let Some((message, prompt)) = open.as_mut() else {
                return false;
            };
            let taken =
                crate::events::drive_retained_control(&mut **prompt, event, scale, modifiers, doc);
            let decided = prompt
                .take_outcome()
                .map(|outcome| (message.clone(), outcome));
            (taken, decided)
        };
        match decided {
            Some((message, PromptOutcome::Submit(answer))) => self.save_secret(&message, &answer),
            Some((message, PromptOutcome::Cancel)) => {
                self.answer_secret(doc, &message, DECLINED, None, "chat-secret-answer-declined");
                self.close_secret_field();
                self.request_relayout();
            }
            None => {}
        }
        taken
    }

    /// Hand what the person typed for the question `message` asks to their
    /// vault: the kind and the places the question names, with the secret.
    /// It goes to the host that keeps the vault and to nothing else.
    pub(crate) fn save_secret(&mut self, message: &str, answer: &SecretAnswer) {
        let Some(asked) = self
            .message(message)
            .and_then(|m| self.asks_me_for_a_secret(m))
        else {
            return;
        };
        let username = answer
            .username
            .clone()
            .unwrap_or_else(|| asked.username.clone());
        let adding = person_vault::AddItem {
            kind: asked.kind,
            belongs_to: asked.belongs_to,
            username: username.trim().to_owned(),
            sent_in: asked.sent_in,
            secret: answer.secret.to_string(),
        };
        let Ok(serde_json::Value::Object(argument)) = serde_json::to_value(&adding) else {
            return;
        };
        let call = self.resource_navigation.remote_mut().invoke(
            person_vault::target(person_vault::ADD),
            InvokeArgument::Json(argument),
            None,
        );
        if let Some((_, prompt)) = self.secret_entry.open.borrow_mut().as_mut() {
            prompt.set_busy(Some(t("chat-secret-saving")));
        }
        self.secret_entry.saving = Some((message.to_owned(), call));
        self.request_relayout();
    }

    /// The vault's answer to what was typed, once it is there: the question
    /// is answered with the reference, or the field says why nothing was
    /// kept.
    fn settle_secret_save(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some((message, call)) = self.secret_entry.saving.take() else {
            return;
        };
        let not_kept = || t("chat-secret-not-kept");
        let settled = call.with(|arrival| match arrival {
            Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(
                output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("reference"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(not_kept),
            ),
            Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                Some(Err(error.message.clone()))
            }
            Arrival::Ready(InvocationUpdate::Cancelled { .. }) => Some(Err(not_kept())),
            Arrival::Failed(error) => Some(Err(error.to_string())),
            Arrival::Pending { .. } | Arrival::Ready(_) => None,
        });
        match settled {
            None => self.secret_entry.saving = Some((message, call)),
            Some(Ok(reference)) => {
                let detail = serde_json::json!({ "reference": reference });
                self.answer_secret(
                    doc,
                    &message,
                    FILLED,
                    Some(detail),
                    "chat-secret-answer-saved",
                );
                self.close_secret_field();
                self.request_relayout();
            }
            Some(Err(reason)) => {
                if let Some((_, prompt)) = self.secret_entry.open.borrow_mut().as_mut() {
                    prompt.set_busy(None);
                    prompt.reject(reason);
                }
                self.request_relayout();
            }
        }
    }

    /// Give `answer` to the secret's question `message` asks, carrying
    /// `detail`, and say so in the conversation in this person's own words
    /// (the catalogue's `words`): what whoever asked reads next.
    fn answer_secret(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        message: &str,
        answer: &str,
        detail: Option<serde_json::Value>,
        words: &str,
    ) {
        let Some(channel) = self.message(message).map(|m| m.channel.clone()) else {
            return;
        };
        let given = self.try_apply_part(
            doc,
            ChatMessages::ANSWER_QUESTION.with(chat_messages::AnswerQuestion {
                message: ui::tilo_resource::Id::from_raw(message),
                answer: answer.to_owned(),
                device: self.device.clone().filter(|device| !device.is_empty()),
                detail,
            }),
        );
        if given.is_ok() {
            self.apply_part(
                doc,
                ChatMessages::POST.with(chat_messages::Post {
                    message: NewMessage {
                        id: None,
                        channel: ui::tilo_resource::Id::from_raw(channel),
                        text: t(words),
                        parent: None,
                        quoted: None,
                        body: None,
                        attachments: Vec::new(),
                        mentions: None,
                        written: None,
                        origin: None,
                    },
                }),
            );
        }
    }
}

#[cfg(test)]
#[path = "secret_ask_tests.rs"]
mod tests;
