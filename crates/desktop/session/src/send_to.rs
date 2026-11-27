//! A message a window sends from its own context: the session makes the agents it names, carries
//! the message to Chat as one action the person's own grants decide, and publishes whom a message
//! can go to, which every box (an app's and the shell's) searches.
//! The window says what the box held — the recipients, what the message carries
//! ([`tilo_reference::Attached`]) and its words — and nothing of how that becomes a conversation:
//! Chat writes to each person or agent in the one direct conversation with them (`write_to`), and
//! sends in a conversation named (`send`), the message saying the words and referring to what it
//! carries. Making
//! the agents a send names is the workspace's governance and Chat's record of its agents, both of
//! which only the session holds, so it happens here first, so the message never goes to an agent
//! the workspace does not list.
//! The person's own send from a box opens where it landed: the message Chat answers with, through
//! the layout's `open`, which brings the conversation forward where it stands.
//! A box opened with the one place a report goes on it is the same send with that place on it: it
//! is filed there as Chat's report ([`crate::feedback`]), one action of the person's own,
//! and nothing opens over the window it is about.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_search::Target;
use tilo_desktop_search::model::InvokeTarget;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer};
use tilolang_tilo::runtime::blob::FileValue;
use ui::floating_surface::{Recipient, RecipientKind};

use crate::i18n::t;
use crate::{BoxAsk, Shared};

/// The resource a send is published under: one action every sender runs — a person's box, an
/// agent, a script.
pub(crate) const RESOURCE: &str = "session:send-to";

/// The session's send-to resource: what the palette and a canvas ask to send a message into a
/// conversation.
#[derive(tilo_resource::Row)]
#[row(kind = "session.send-to", label = "Send to")]
pub(crate) struct SendTo;

/// The sends the last passes carried out, newest last, that its reading keeps.
const KEPT: usize = 16;

/// What the resource offers: a send.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.send-to")]
pub(crate) enum SendToAction {
    /// Send to
    ///
    /// Sends the words and what they carry to whom it names, as a person's send-to box does: the
    /// conversation with them opens or continues in Chat, carrying the items. A recipient is a new
    /// agent, an agent, a person or a conversation, by its id, or by its name alone to be found.
    #[effect(kind = "outside")]
    #[effect(job)]
    Send {
        #[param("Who it goes to: kind new-agent, agent, person or conversation, by id or name")]
        to: Vec<Recipient>,
        #[param("What it carries besides its words")]
        attached: Option<Vec<tilo_reference::Attached>>,
        #[param("A resource it carries, by its id as the catalogue lists it")]
        item: Option<String>,
        #[param("The message")]
        text: String,
        #[param("The workspace it is sent in; the one in front without")]
        workspace: Option<String>,
    },
}

/// One send taken, waiting for the pass that carries it out: who asked, and what.
struct Sending {
    /// Who sent it: the app whose window's box it was, an agent, a script; the person sitting
    /// here without.
    by: Option<tilo_actions::ActionInvoker>,
    to: Vec<Recipient>,
    attached: Vec<tilo_reference::Attached>,
    item: Option<String>,
    text: String,
    workspace: Option<String>,
    /// The person's ask from the canvas this is, by the number [`Sends::ask`] gave it: the
    /// message Chat posts for it is kept under that number ([`Sends::posted`]), for its turn to
    /// stand where the person asked. `None` for a box's send.
    asked: Option<u64>,
    /// The caller's answer: the pass completes it once the message is in the record, or says why
    /// it never landed.
    answer: tilo_actions::remote::Completer<Result<tilo_actions::ActionOutput, ApiError>>,
    /// When it was taken: how long it waited for the pass that carried it out is logged.
    taken: tilo_clock::time::Instant,
}

/// A send as carried out: what it carried, whom it reached, and whether it landed.
#[derive(Clone, Debug, serde::Serialize)]
struct Sent {
    /// The number the pass gave it, which its answer from Chat comes back under.
    #[serde(skip)]
    number: u64,
    to: Vec<Recipient>,
    attached: Vec<tilo_reference::Attached>,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<String>,
}

/// The sends taken and carried out, and whom a message can go to, shared by the service and the
/// pass.
#[derive(Clone, Default)]
pub(crate) struct Sends {
    waiting: Rc<RefCell<Vec<Sending>>>,
    sent: Rc<RefCell<Vec<Sent>>>,
    recipients: Rc<RefCell<Vec<Recipient>>>,
    /// The search's generation [`Self::recipients`] was read at.
    read_at: Rc<Cell<Option<(u64, u64)>>>,
    /// Whether the reading moved since the service last said so.
    moved: Rc<Cell<bool>>,
    /// The number the next ask from the canvas takes.
    asks: Rc<std::cell::Cell<u64>>,
    /// The message Chat answered each ask's send with, by the ask's number: written on the
    /// thread Chat's answer lands on, read by the pass.
    posted: Posted,
    /// The sends handed to Chat whose answer has not come back, by the number the pass gave each.
    flying: Rc<RefCell<std::collections::BTreeMap<u64, Flying>>>,
    /// The number the next send handed to Chat takes.
    sent_count: Rc<Cell<u64>>,
    /// What Chat answered each send, by its number: written on the thread its answer lands on,
    /// read by the pass.
    settled: Settled,
}

/// What Chat answered each send, by its number: one answer for each conversation it went to,
/// the message that landed (when the answer names it) or why it did not.
type Settled = std::sync::Arc<std::sync::Mutex<Vec<(u64, Result<Option<String>, String>)>>>;

/// A send handed to Chat, waiting for its answers: whom to tell.
struct Flying {
    /// Whether the person sitting here sent it, who is told when it did not land.
    by_person: bool,
    /// How many of the conversations it went to have not answered yet.
    waiting: usize,
    /// Why it did not land in one of them, the first that said so.
    refused: Option<String>,
    /// Whether it opens where it landed once it did: a send from the person's own box, never an
    /// ask from the canvas (its turn stands where they asked) nor a report.
    opens: bool,
    /// The first message it landed as, which opens.
    landed: Option<String>,
    answer: tilo_actions::remote::Completer<Result<tilo_actions::ActionOutput, ApiError>>,
}

/// The message Chat posted for each ask, by the ask's number.
type Posted = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<u64, String>>>;

/// What [`RESOURCE`] publishes: the send.
fn published() -> Vec<ResourceSpec> {
    vec![offer::<SendToAction>(
        tilo_resource::LiveRow::typed::<SendTo>(RESOURCE, crate::i18n::t("send-to-resource"))
            .into_resource(),
    )]
}

impl Sends {
    /// Take a send with `arguments`, as `by` (the person sitting here without), for the session's
    /// pass to carry out ([`Shared::deliver_sends`]): the one entry of `session:send-to#send`, for
    /// the service and for the canvas asking the workspace's agent alike. The caller is answered
    /// when the pass has carried the send out, so an invocation settles with the message in the
    /// record rather than staying accepted.
    ///
    /// # Errors
    /// The arguments are not a send's, or the message has nothing to say or carry.
    pub(crate) fn send(
        &self,
        by: Option<tilo_actions::ActionInvoker>,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        self.take(by, arguments, None)
    }

    /// Take the person's ask from the canvas, a send with `arguments`, as [`Self::send`] does, and
    /// answer the number it goes by: the conversation it lands in does not open, since the turn
    /// it opens stands on the canvas where the person asked (canvas.md §3), and the message Chat
    /// posts for it is [`Self::posted`] under that number once Chat answers.
    ///
    /// # Errors
    /// The arguments are not a send's, or the message has nothing to say or carry.
    pub(crate) fn ask(&self, arguments: &Map<String, Value>) -> Result<u64, ApiError> {
        let asked = self.asks.get().wrapping_add(1);
        self.asks.set(asked);
        self.take(None, arguments, Some(asked))?;
        Ok(asked)
    }

    /// The message Chat posted for the ask `asked`, once Chat answered its send with it.
    pub(crate) fn posted(&self, asked: u64) -> Option<String> {
        self.posted
            .lock()
            .ok()
            .and_then(|posted| posted.get(&asked).cloned())
    }

    fn take(
        &self,
        by: Option<tilo_actions::ActionInvoker>,
        arguments: &Map<String, Value>,
        asked: Option<u64>,
    ) -> Result<Started, ApiError> {
        let SendToAction::Send {
            to,
            attached,
            item,
            text,
            workspace,
        } = decode::<SendToAction>(&published(), RESOURCE, SEND, arguments)?;
        let attached = attached.unwrap_or_default();
        if text.trim().is_empty() && attached.is_empty() && item.is_none() {
            return Err(ApiError::InvalidArguments(
                "a message needs words or something to carry".into(),
            ));
        }
        let (answer, later) = tilo_actions::remote::pending();
        self.waiting.borrow_mut().push(Sending {
            by,
            to,
            attached,
            item,
            text,
            workspace,
            asked,
            answer,
            taken: tilo_clock::now(),
        });
        Ok(Started::Later(later))
    }
}

/// The send action as a service: it takes each send, and the session's pass carries it out
/// ([`Shared::deliver_sends`]), since a send makes agents and reaches Chat through the APIs.
pub(crate) struct SendToService {
    sends: Sends,
}

impl SendToService {
    pub(crate) fn new(sends: Sends) -> Self {
        Self { sends }
    }
}

impl Service for SendToService {
    fn resources(&self) -> Vec<ResourceSpec> {
        published()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            serde_json::json!({
                "recipients": *self.sends.recipients.borrow(),
                "sent": *self.sends.sent.borrow(),
                // Sends taken and not carried out yet: none once a pass ends, since a send goes
                // out in the pass that took it.
                "waiting": self.sends.waiting.borrow().len(),
            })
        })
    }

    fn poll(&mut self) -> bool {
        self.sends.moved.replace(false)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        self.sends.send(None, arguments)
    }

    fn invoke_as(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        self.sends.send(Some(invoker.clone()), arguments)
    }
}

/// The app that keeps a workspace's conversations.
pub use ui::apps::CHAT;

/// The session's own action that sends a message to whom it names.
pub(crate) const SEND: &str = "send";

impl Shared {
    /// Carry out the sends taken since the last pass ([`SendToService`]): each item named by its
    /// id is carried as the record it is, and the send goes as one action, for Chat to find whom
    /// it names. A send is settled once Chat answered it: its caller is told whether the message
    /// landed, and a send of the person's own that did not is told them, with why.
    pub(crate) fn deliver_sends(&self, content: Option<&crate::record_content::RecordContent>) {
        let waiting = std::mem::take(&mut *self.sends.waiting.borrow_mut());
        for sending in waiting {
            let workspace = sending
                .workspace
                .clone()
                .unwrap_or_else(|| self.active_workspace_id());
            let mut attached = sending.attached;
            let found = sending.item.as_deref().map(|item| self.carried(item));
            let refused = match found {
                Some(Err(missing)) => Some(missing),
                Some(Ok(item)) => {
                    attached.push(item);
                    None
                }
                None => None,
            };
            let number = self.sends.sent_count.get();
            self.sends.sent_count.set(number.wrapping_add(1));
            let started = match refused {
                Some(reason) => Err(reason),
                None => self.send_to(
                    (&workspace, number),
                    &sending.to,
                    (&attached, content),
                    &sending.text,
                    sending.asked,
                ),
            };
            // How long it waited for the pass that carried it out: none past that pass's end.
            tracing::info!(%workspace, waited_ms = sending.taken.elapsed().as_millis(), "a send went to Chat");
            let mut sent = self.sends.sent.borrow_mut();
            sent.push(Sent {
                number,
                to: sending.to,
                attached,
                text: sending.text,
                refused: None,
            });
            let over = sent.len().saturating_sub(KEPT);
            sent.drain(..over);
            drop(sent);
            self.sends.moved.set(true);
            let by_person = sending.by.as_ref().is_none_or(|by| by.acting.is_person());
            let flying = Flying {
                by_person,
                waiting: 0,
                refused: None,
                opens: false,
                landed: None,
                answer: sending.answer,
            };
            match started {
                Ok((waiting, message)) => {
                    let opens = message && by_person && sending.asked.is_none();
                    self.sends.flying.borrow_mut().insert(
                        number,
                        Flying {
                            waiting,
                            opens,
                            ..flying
                        },
                    );
                }
                Err(reason) => self.settle_send(number, flying, Err(reason)),
            }
        }
        let settled = self
            .sends
            .settled
            .lock()
            .map(|mut settled| std::mem::take(&mut *settled))
            .unwrap_or_default();
        for (number, outcome) in settled {
            let mut flying = self.sends.flying.borrow_mut();
            let Some(waiting) = flying.get_mut(&number) else {
                continue;
            };
            waiting.waiting = waiting.waiting.saturating_sub(1);
            match outcome {
                Ok(landed) => {
                    if waiting.landed.is_none() {
                        waiting.landed = landed;
                    }
                }
                Err(reason) => {
                    waiting.refused.get_or_insert(reason);
                }
            }
            if waiting.waiting > 0 {
                continue;
            }
            let Some(done) = flying.remove(&number) else {
                continue;
            };
            drop(flying);
            let outcome = done.refused.clone().map_or(Ok(()), Err);
            self.settle_send(number, done, outcome);
        }
    }

    /// Settle the send `number` with what Chat answered: the reading says whether it landed, its
    /// caller is told, and a send of the person's own that did not land is told them too, with
    /// why: the box that sent it closed as it was sent, and nothing opened where it stood.
    fn settle_send(&self, number: u64, flying: Flying, outcome: Result<(), String>) {
        let Flying {
            by_person,
            answer,
            opens,
            landed,
            ..
        } = flying;
        if outcome.is_ok()
            && opens
            && let Some(message) = landed
        {
            self.open_landed(&message);
        }
        if let Err(reason) = &outcome {
            let mut sent = self.sends.sent.borrow_mut();
            let sent = sent.iter_mut().find(|sent| sent.number == number);
            let to: Vec<&str> = sent
                .as_ref()
                .map(|sent| sent.to.iter().map(|to| to.name.trim()).collect())
                .unwrap_or_default();
            tracing::warn!(%reason, ?to, "a send did not land");
            if by_person {
                let notice = crate::notices::Notice::new(
                    t("send-to-resource"),
                    1,
                    crate::i18n::t_args("send-to-refused-title", &[("to", to.join(", ").into())]),
                    reason.clone(),
                );
                self.notifications.borrow().tell(&notice);
            }
            if let Some(sent) = sent {
                sent.refused = Some(reason.clone());
                self.sends.moved.set(true);
            }
        }
        // The caller is answered once the message is in the record: the receipt of an invocation
        // is what says the message landed, or why it did not.
        answer.complete(match outcome {
            Err(reason) => Err(ApiError::Failed(reason)),
            Ok(()) => Ok(tilo_actions::ActionOutput::default()),
        });
    }

    /// Open `message`, where a send of the person's own landed, through the layout's `open`: the
    /// conversation it is in comes forward where it stands.
    fn open_landed(&self, message: &str) {
        let mut arguments = Map::new();
        arguments.insert("resource".into(), message.into());
        let address = ActionAddress::new(tilo_session_core::layout::RESOURCE, "open");
        if let Err(error) = self.published.invoke(
            &address,
            &arguments,
            &mut tilo_session_core::surfaces::Look::none(),
        ) {
            tracing::warn!(%error, %message, "where a send landed did not open");
        }
    }

    /// Read whom a message can go to again when the search's index moved, for the next reading of
    /// [`RESOURCE`].
    pub(crate) fn follow_recipients(&self) {
        let generation = self.search.recipients_generation();
        if self.sends.read_at.get() == Some(generation) {
            return;
        }
        self.sends.read_at.set(Some(generation));
        *self.sends.recipients.borrow_mut() = self.recipients();
        self.sends.moved.set(true);
    }

    /// The resource `item` names, as a message carries it: the record of the app that publishes
    /// it, found in the catalogue every caller surveys.
    fn carried(&self, item: &str) -> Result<tilo_reference::Attached, String> {
        let resources = self.published.resources();
        let resource = tilo_actions::api::find_resource(&resources, item)
            .ok_or_else(|| format!("nothing is published as {item}"))?;
        let app = resource
            .app()
            .ok_or_else(|| format!("{item} is no app's resource"))?;
        Ok(tilo_reference::Attached::resource(
            tilo_reference::ResourceAt {
                app: app.to_owned(),
                record: None,
                reference: tilo_reference::ResourceRef::whole(
                    resource.kind.clone(),
                    item,
                    resource.label.clone(),
                ),
            },
            None,
            resource.label.clone(),
        ))
    }

    /// Send what a window's box holds: an agent named as a new one is made in
    /// the workspace and kept by this person, and the message reaches Chat as
    /// one action for each person, agent or conversation it names
    /// ([`send_targets`]). A report is filed where it goes, and nothing opens
    /// over the window it is about. What Chat answers is settled under `number`
    /// once every conversation answered ([`Self::deliver_sends`]); it returns
    /// how many it went to.
    ///
    /// # Errors
    /// The workspace is not open here, an agent could not be made or admitted,
    /// or the action could not be handed to Chat.
    ///
    /// Answers how many actions it handed Chat, and whether the send is a message (a report is
    /// not).
    fn send_to(
        &self,
        (workspace, number): (&str, u64),
        to: &[Recipient],
        (attached, content): (
            &[tilo_reference::Attached],
            Option<&crate::record_content::RecordContent>,
        ),
        text: &str,
        asked: Option<u64>,
    ) -> Result<(usize, bool), String> {
        let text = text.trim();
        if text.is_empty() && attached.is_empty() {
            return Err("a message needs words or something to carry".into());
        }
        let (workspace, targets, message) = match addressed(self.feedback.as_ref(), workspace, to)?
        {
            Addressed::Message { workspace, to } => {
                let resolved = self.recipients_of(&workspace, &to, &agent_name(attached))?;
                let files = self.files_kept(&workspace, attached, content)?;
                let targets = send_targets(&workspace, &resolved, (attached, &files), text)?;
                (workspace, targets, true)
            }
            // A report is read where it lands, not floated over the window it was written over: it
            // is filed in the channel and nothing opens.
            Addressed::Report(destination) => {
                let files = self.files_kept(&destination.workspace, attached, content)?;
                (
                    destination.workspace.clone(),
                    vec![report_target(&destination, (attached, &files), text)?],
                    false,
                )
            }
        };
        let sent = targets.len();
        for target in targets {
            let settled = std::sync::Arc::clone(&self.sends.settled);
            // An ask from the canvas is told which message Chat posted: its turn stands by it.
            let posted = asked.map(|asked| (asked, std::sync::Arc::clone(&self.sends.posted)));
            let wake = std::sync::Arc::clone(&self.wake_handle);
            self.search
                .perform_answered(
                    &workspace,
                    target,
                    Box::new(move |answer| {
                        // The message is what the action made: the first resource it answers.
                        let message = answer
                            .as_ref()
                            .ok()
                            .and_then(|answer| answer.resources.first())
                            .cloned();
                        if let Some((asked, posted)) = posted {
                            match message.clone() {
                                Some(message) => {
                                    if let Ok(mut posted) = posted.lock() {
                                        posted.insert(asked, message);
                                    }
                                }
                                None => {
                                    tracing::warn!(
                                        asked,
                                        "Chat's answer to an ask names no message"
                                    );
                                }
                            }
                        }
                        if let Ok(mut settled) = settled.lock() {
                            settled.push((number, answer.map(|_| message)));
                        }
                        wake();
                    }),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok((sent, message))
    }

    /// The files a message carries, kept in `workspace`'s content store where it goes, as the files
    /// Chat's actions take: the ones the session handed the box ([`crate::carried`]), which leave
    /// the device only now that the person sent them.
    ///
    /// # Errors
    /// A file is none the session handed a box, or it could not be kept there.
    fn files_kept(
        &self,
        workspace: &str,
        attached: &[tilo_reference::Attached],
        content: Option<&crate::record_content::RecordContent>,
    ) -> Result<Vec<tilolang_tilo::runtime::blob::FileValue>, String> {
        let keep = |path: &std::path::Path| {
            content
                .ok_or_else(|| "this session keeps no content to send a file with".to_owned())?
                .keep_file_under(
                    workspace,
                    tilo_broker_core::services::CONTENT_PUT_SCOPE,
                    path,
                )
        };
        attached
            .iter()
            .filter_map(|file| self.box_files.kept(file, &keep))
            .collect()
    }

    /// Who a message goes to, as the names a box holds: an agent named as a
    /// new one is made here first, so the message never goes to an agent the
    /// workspace does not list. A new agent the person did not name is named
    /// `asked`, by what it is sent.
    ///
    /// # Errors
    /// A new agent could not be made or admitted, or nobody is left to go to.
    fn recipients_of(
        &self,
        workspace: &str,
        to: &[Recipient],
        asked: &str,
    ) -> Result<Vec<Recipient>, String> {
        let feed = self.workspace_settings_feed();
        let mut resolved: Vec<Recipient> = Vec::with_capacity(to.len());
        for recipient in to {
            match recipient.kind {
                RecipientKind::NewAgent => {
                    // A new agent takes the workspace's default model and the colour its identity
                    // gives it.
                    let name = match recipient.name.trim() {
                        "" => asked,
                        named => named,
                    };
                    let made = feed.make_agent(Some(workspace), name, "")?;
                    resolved.push(Recipient::found(
                        RecipientKind::Agent,
                        crate::search::recipient_id(&made),
                        name,
                    ));
                }
                RecipientKind::Agent
                | RecipientKind::Person
                | RecipientKind::Conversation
                | RecipientKind::Feedback => {
                    resolved.push(recipient.clone());
                }
            }
        }
        if resolved.is_empty() {
            return Err("a message needs someone to go to".into());
        }
        Ok(resolved)
    }

    /// Reporting a problem, as a row of every box's list of whom a message goes to and as the chip
    /// a report's box opens with: the one place this computer sends reports, else the row that
    /// says it is told nowhere — which still leaves an agent or a person to send to.
    pub(crate) fn feedback_row(&self) -> Recipient {
        self.feedback.as_ref().map_or_else(
            || {
                Recipient::found(
                    RecipientKind::Feedback,
                    String::new(),
                    t("session-feedback-nowhere"),
                )
                .with_detail(t("session-feedback-nowhere-body"))
            },
            crate::feedback::Destination::recipient,
        )
    }

    /// Whether the shell asked for a box since the last pass, and what it asked for: the window in
    /// front then opens its box
    /// ([`ui::floating_surface::FloatingSurfaceHostMessage::ShareSelection`]).
    pub(crate) fn take_box_ask(&self) -> Option<BoxAsk> {
        self.box_ask.take()
    }

    /// The people in the workspace here, as the chips of a box that says something to them: the
    /// one search's own reading of who is about, never a second list.
    pub(crate) fn people_recipients(&self) -> Vec<Recipient> {
        people_of(self.recipients())
    }

    /// Whom a message can go to, the most recent first, with reporting a problem last: who can be
    /// found in the workspace here ([`crate::search::SessionSearch::recipients`]), the list desktop
    /// search reads too.
    pub(crate) fn recipients(&self) -> Vec<Recipient> {
        let mut found = self.search.recipients(&self.active_workspace_id());
        found.push(self.feedback_row());
        found
    }
}

/// The most people a box starts addressed to: more chips than a line of them holds, and a person
/// saying something to everyone here names the rest themselves.
const MAX_PEOPLE_CHIPS: usize = 4;

/// The rows among `rows` a box that says something to the people here opens to: the people, never
/// the agents and conversations the same search finds.
fn people_of(rows: Vec<Recipient>) -> Vec<Recipient> {
    rows.into_iter()
        .filter(|recipient| recipient.kind == RecipientKind::Person)
        .take(MAX_PEOPLE_CHIPS)
        .collect()
}

/// The most characters a new agent's name takes from what it is sent.
const AGENT_NAME_CHARS: usize = 24;

/// What a new agent nobody named is called: a short label of what it is sent — the subject chip,
/// without the number its list gives the item — else the default name. Never the message: a
/// sentence asks the agent something, it does not name it.
fn agent_name(attached: &[tilo_reference::Attached]) -> String {
    let label = attached
        .iter()
        .map(|attached| attached.label().trim())
        .find(|label| !label.is_empty())
        .unwrap_or_default();
    let label = match label.strip_prefix('#') {
        Some(numbered) if numbered.starts_with(|c: char| c.is_ascii_digit()) => numbered
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start(),
        _ => label,
    };
    let mut name = String::new();
    for word in label.split_whitespace() {
        let wanted = name
            .chars()
            .count()
            .saturating_add(usize::from(!name.is_empty()))
            .saturating_add(word.chars().count());
        if wanted > AGENT_NAME_CHARS {
            if name.is_empty() {
                name = word
                    .chars()
                    .take(AGENT_NAME_CHARS.saturating_sub(1))
                    .collect();
            }
            name.push('…');
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    if name.is_empty() {
        crate::i18n::t("send-to-agent-unnamed")
    } else {
        name
    }
}

/// Where a box's send goes, as it named whom it goes to: a report goes to the one place this
/// computer sends reports, and is filed there; any other send is a message in the workspace the box
/// was opened in.
enum Addressed {
    Message {
        workspace: String,
        to: Vec<Recipient>,
    },
    Report(crate::feedback::Destination),
}

/// Address a box's send by whom it named: the one place this computer sends
/// reports, or a message in the workspace it was opened in.
///
/// # Errors
/// A report stood beside another recipient, or this computer is told nowhere
/// to send one.
fn addressed(
    feedback: Option<&crate::feedback::Destination>,
    workspace: &str,
    to: &[Recipient],
) -> Result<Addressed, String> {
    if !to
        .iter()
        .any(|recipient| recipient.kind == RecipientKind::Feedback)
    {
        return Ok(Addressed::Message {
            workspace: workspace.to_owned(),
            to: to.to_vec(),
        });
    }
    if to.len() > 1 {
        return Err("a report goes to the one place it was opened to".to_owned());
    }
    let destination =
        feedback.ok_or_else(|| "this computer is told nowhere to send a report".to_owned())?;
    Ok(Addressed::Report(destination.clone()))
}

/// The one action a report is: Chat's `report` in the workspace that keeps reports, saying what
/// the person wrote and referring to what the window picked, as any message does
/// ([`message_body`]), with the files it carries (`files`, kept there already): its recording, the
/// first video, which the report's row offers to replay, and the rest beside it. Nothing else, and
/// no window opening over the one reported.
///
/// # Errors
/// A file could not be said as Chat takes it.
fn report_target(
    destination: &crate::feedback::Destination,
    (attached, files): (&[tilo_reference::Attached], &[FileValue]),
    text: &str,
) -> Result<Target, String> {
    let recording = files
        .iter()
        .position(|file| file.media.starts_with("video/"));
    let mut target = InvokeTarget::action(
        CHAT,
        &destination.workspace,
        tilo_app_id::chat::CONVERSATIONS,
        tilo_app_id::chat::REPORT_PROBLEM,
    )
    .with_argument("body", message_body(text, attached)?);
    if let Some(at) = recording {
        let file = files
            .get(at)
            .ok_or("the recording is not among the files")?;
        target = target.with_argument("recording", file_json(file)?);
    }
    let rest: Vec<FileValue> = files
        .iter()
        .enumerate()
        .filter(|(at, _)| Some(*at) != recording)
        .map(|(_, file)| file.clone())
        .collect();
    if !rest.is_empty() {
        target = target.with_argument("attachments", files_json(&rest)?);
    }
    Ok(Target::Invoke(Box::new(target)))
}

/// `file` as an action's argument takes it, as any caller hands an app's action API a file
/// ([`to_argument`](tilolang_tilo::runtime::value::to_argument)).
///
/// # Errors
/// The runtime could not say it.
fn file_json(file: &FileValue) -> Result<Value, String> {
    use tilolang_tilo::runtime::value::{Value as Tl, to_argument};
    to_argument(&Tl::File(std::rc::Rc::new(file.clone())))
}

/// `files` as a `List<File>` argument takes them: an array of the files.
///
/// # Errors
/// The runtime could not say them.
fn files_json(files: &[FileValue]) -> Result<Value, String> {
    files
        .iter()
        .map(file_json)
        .collect::<Result<_, _>>()
        .map(Value::Array)
}

/// The actions a send is, one for each conversation it goes to: Chat's `write_to` for a person
/// or an agent, which writes in the one direct conversation with them, and Chat's `send` in a
/// conversation named. Each carries the message as rich text: its words, then a reference to each
/// thing it carries and the words it quotes ([`message_body`]).
///
/// The files it carries (`files`, kept in the workspace already) go with each as its attachments.
///
/// # Errors
/// A file could not be said as Chat takes it.
fn send_targets(
    workspace: &str,
    to: &[Recipient],
    (attached, files): (&[tilo_reference::Attached], &[FileValue]),
    text: &str,
) -> Result<Vec<Target>, String> {
    let body = message_body(text, attached)?;
    let carried = if files.is_empty() {
        None
    } else {
        Some(files_json(files)?)
    };
    to.iter()
        .map(|recipient| {
            let target = match recipient.kind {
                RecipientKind::Person => InvokeTarget::action(
                    CHAT,
                    workspace,
                    tilo_app_id::chat::CONVERSATIONS,
                    tilo_app_id::chat::WRITE_TO,
                )
                .with_argument("who", serde_json::json!({ "person": recipient.id }))
                .with_argument("body", body.clone()),
                RecipientKind::Agent => {
                    // An agent recipient names whose agent it is (`agent:<person>/<id>`), as
                    // Chat's member is the pair.
                    let agent =
                        tilo_member_types::MemberId::of_actor_bytes(recipient.id.as_bytes())
                            .filter(tilo_member_types::MemberId::is_agent)
                            .ok_or_else(|| format!("{} names no agent", recipient.name))?;
                    InvokeTarget::action(
                        CHAT,
                        workspace,
                        tilo_app_id::chat::CONVERSATIONS,
                        tilo_app_id::chat::WRITE_TO,
                    )
                    .with_argument("who", serde_json::json!(agent))
                    .with_argument("body", body.clone())
                }
                RecipientKind::Conversation => {
                    InvokeTarget::action(CHAT, workspace, &recipient.id, tilo_app_id::chat::SEND)
                        .with_argument("body", body.clone())
                }
                RecipientKind::NewAgent | RecipientKind::Feedback => {
                    return Err(format!("{} is no conversation", recipient.name));
                }
            };
            let target = match &carried {
                Some(files) => target.with_argument("attachments", files.clone()),
                None => target,
            };
            Ok(Target::Invoke(Box::new(target)))
        })
        .collect()
}

/// What a message says, as Chat's rich text takes it: the words, then each thing it carries as a
/// reference to it and each quote in its own words, referring to where it was quoted from. Plain
/// words travel as the text they are. A file is carried as itself, beside the words
/// ([`Shared::files_kept`]), never referred to in them.
///
/// # Errors
/// The runtime could not say the rich text.
fn message_body(text: &str, attached: &[tilo_reference::Attached]) -> Result<Value, String> {
    use tilo_reference::{Attached, Embed};
    use tilolang_tilo::runtime::rich::RichValue;
    let mut body = RichValue::from_text(text);
    let referred: Vec<&Attached> = attached
        .iter()
        .filter(|carried| carried.blob().is_none())
        .collect();
    for carried in referred.iter().copied() {
        let separator = if body.is_empty() { "" } else { " " };
        match carried {
            Attached::Resource { at, label, .. } => {
                body.push_text(separator);
                body.push_reference(label, &Embed::Resource(at.clone()));
            }
            Attached::Text { text: quoted, from } => {
                body.push_text(&format!("{separator}“{quoted}”"));
                if let Some(from) = from {
                    body.push_text(" ");
                    body.push_reference(&from.reference.label, &Embed::Resource(from.clone()));
                }
            }
            Attached::Image { .. } | Attached::Video { .. } | Attached::File { .. } => {}
        }
    }
    if referred.is_empty() {
        return Ok(Value::String(text.to_owned()));
    }
    tilolang_tilo::runtime::value::to_json(&tilolang_tilo::runtime::value::Value::Rich(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::Attached;

    tilo_test_env::scenario! {
        fn a_send_to_box_lists_who_it_reaches_and_sends_one_action;

        phase a_box_to_the_people_here_holds_the_people_only;
        // Saying something to the people here addresses the people the search names about, never the
        // agents and conversations it finds beside them.
        {
            let rows = vec![
                Recipient::found(RecipientKind::Agent, "a_notes", "Notes agent"),
                Recipient::found(RecipientKind::Conversation, "c_1", "Ada and Bo"),
                Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada"),
                Recipient::found(RecipientKind::Person, "bo".repeat(32), "Bo"),
            ];
            assert_eq!(
                people_of(rows),
                vec![
                    Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada"),
                    Recipient::found(RecipientKind::Person, "bo".repeat(32), "Bo"),
                ]
            );
        }

        phase the_report_row_is_found_by_name;
        // Reporting a problem is a row of every box's list, found by what it is called.
        {
            let report = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
            let found = |query: &str| {
                ui::floating_surface::recipients_matching(std::slice::from_ref(&report), query)
            };
            assert_eq!(
                found("feed"),
                std::slice::from_ref(&report),
                "found by what it is called"
            );
            assert_eq!(
                found("TILO"),
                std::slice::from_ref(&report),
                "whatever the case typed"
            );
            assert!(
                found("ada").is_empty(),
                "and absent from a search for someone"
            );
        }

        phase each_ask_from_the_canvas_is_numbered_and_a_boxs_send_is_not;
        // The person's ask from the canvas is numbered, for its turn to stand by the message Chat
        // posts for it; a box's send is not an ask.
        {
            let sends = Sends::default();
            let arguments = serde_json::json!({
                "to": [Recipient::found(RecipientKind::Agent, "a1".to_owned(), "Assistant")],
                "text": "what is due?",
            });
            let serde_json::Value::Object(arguments) = arguments else {
                panic!("an object");
            };
            let first = sends.ask(&arguments).expect("the ask is taken");
            sends.send(None, &arguments).expect("the send is taken");
            let second = sends.ask(&arguments).expect("another ask is taken");
            let asked: Vec<Option<u64>> = sends
                .waiting
                .borrow()
                .iter()
                .map(|sending| sending.asked)
                .collect();
            assert_eq!(asked, [Some(first), None, Some(second)]);
            assert_ne!(first, second, "each ask is one of its own");
        }

        phase a_send_writes_to_each_person_and_agent_and_sends_in_a_conversation;
        // What a box holds reaches Chat as one action for each conversation: a person and an agent
        // are written to in the one direct conversation with them, a conversation named is sent in,
        // and each message says the words and refers to what it carries.
        {
            let ada =
                Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada").with_detail("online");
            let helper_member = tilo_member_types::MemberId::agent("a_helper", "ad".repeat(32));
            let helper = Recipient::found(
                RecipientKind::Agent,
                crate::search::recipient_id(&helper_member),
                "Helper",
            );
            let general = Recipient::found(
                RecipientKind::Conversation,
                "tl:so.tilo.chat:root-lmdoxbnn/c1",
                "#general",
            );
            let targets = send_targets(
                "ws",
                &[ada, helper, general],
                (&[], &[]),
                "look",
            )
            .expect("a send builds");
            let invoked: Vec<_> = targets
                .into_iter()
                .map(|target| match target {
                    Target::Invoke(target) => target,
                    _ => panic!("a send invokes Chat"),
                })
                .collect();
            assert_eq!(invoked.len(), 3, "one action for each conversation");
            for target in &invoked {
                assert_eq!(target.app, CHAT);
                assert_eq!(target.workspace, "ws");
                assert_eq!(target.arguments.get("body"), Some(&Value::String("look".into())));
            }
            assert_eq!(invoked[0].address.resource, tilo_app_id::chat::CONVERSATIONS);
            assert_eq!(invoked[0].address.action, tilo_app_id::chat::WRITE_TO);
            assert_eq!(
                invoked[0].arguments.get("who"),
                Some(&serde_json::json!({ "person": "ad".repeat(32) }))
            );
            assert_eq!(
                invoked[1].arguments.get("who"),
                Some(&serde_json::json!({ "agent": { "id": "a_helper", "person": "ad".repeat(32) } }))
            );
            assert_eq!(invoked[2].address.resource, "tl:so.tilo.chat:root-lmdoxbnn/c1");
            assert_eq!(invoked[2].address.action, tilo_app_id::chat::SEND);

            // What it carries is referred to in the message, after its words.
            let at = tilo_reference::ResourceAt {
                app: "so.tilo.tasks".to_owned(),
                record: None,
                reference: tilo_reference::ResourceRef::whole("tl.so.tilo.tasks.Task", "t1", "Ship"),
            };
            let carried = [
                Attached::resource(at, None, "Ship"),
                Attached::Text {
                    text: "issue 42".into(),
                    from: None,
                },
            ];
            let body = message_body("look", &carried).expect("a body");
            let rich = body.get("rich").expect("rich text, since it refers to something");
            assert!(rich.to_string().contains("issue 42"), "{rich}");
            assert!(rich.to_string().contains("so.tilo.tasks"), "{rich}");
            // A file goes as itself beside the words, never as a reference in them.
            let file = Attached::File {
                name: "notes.pdf".into(),
                blob: "b".into(),
                size: 1,
            };
            assert_eq!(
                message_body("look", &[file]).expect("a body"),
                Value::String("look".into())
            );
        }

        phase a_new_agent_is_named_by_what_it_is_sent_never_the_message;
        // A new agent nobody named is called by a short label of what it is sent, else the default
        // name: never the sentence it is asked.
        {
            let chip = |label: &str| Attached::Text {
                text: label.into(),
                from: None,
            };
            assert_eq!(
                agent_name(&[chip("#1 Book the room for Friday")]),
                "Book the room for Friday",
                "the subject chip names it, without its list number"
            );
            assert_eq!(agent_name(&[chip("#general")]), "#general");
            let long = agent_name(&[chip("Summarise every review comment on the release notes")]);
            assert_eq!(long, "Summarise every review…");
            assert!(long.chars().count() <= AGENT_NAME_CHARS + 1);
            assert_eq!(agent_name(&[]), "Agent", "nothing sent: the default name");
            assert_eq!(agent_name(&[chip("  ")]), "Agent");
        }

        phase a_report_is_one_report_filed_in_the_workspace_the_destination_names;
        // A report's chip is the one place this computer sends reports: one send
        // is one report, Chat's `report` in the workspace that keeps them, saying
        // the words and what the window picked, and nothing opens over the window
        // it is about.
        {
            let destination = crate::feedback::Destination {
                workspace: "w_tilo".to_owned(),
            };
            let report = destination.recipient();
            let Addressed::Report(place) =
                addressed(Some(&destination), "w_notes", std::slice::from_ref(&report))
                    .expect("a report is addressed")
            else {
                panic!("a report is not a message");
            };
            assert_eq!(place.workspace, "w_tilo", "in the workspace that keeps it");
            let carried = Attached::Text {
                text: "issue 42".into(),
                from: None,
            };
            let Target::Invoke(target) = report_target(
                &place,
                (std::slice::from_ref(&carried), &[]),
                "the button does nothing",
            )
            .expect("a report targets") else {
                panic!("a report invokes Chat");
            };
            assert_eq!(target.app, CHAT);
            assert_eq!(target.workspace, "w_tilo");
            assert_eq!(target.address.resource, tilo_app_id::chat::CONVERSATIONS);
            assert_eq!(target.address.action, tilo_app_id::chat::REPORT_PROBLEM);
            let body = target.arguments["body"].to_string();
            assert!(
                body.contains("the button does nothing") && body.contains("issue 42"),
                "the words and what the window picked go with the report: {body}"
            );

            assert!(
                target.arguments.get("recording").is_none(),
                "a report the person sent without a recording claims none"
            );

            // The files a report carries go with it as the files Chat takes: its recording, the
            // video, which its row replays, and the picture of the window beside it.
            let recording = FileValue::new("Recording.mp4", "video/mp4", 4096, "ab".repeat(32));
            let picture = FileValue::new("Screenshot.png", "image/png", 512, "cd".repeat(32));
            let Target::Invoke(target) = report_target(
                &place,
                (std::slice::from_ref(&carried), &[picture.clone(), recording.clone()]),
                "the button does nothing",
            )
            .expect("a report targets") else {
                panic!("a report invokes Chat");
            };
            let said = |file: &FileValue| {
                tilolang_tilo::runtime::value::to_argument(
                    &tilolang_tilo::runtime::value::Value::File(std::rc::Rc::new(file.clone())),
                )
                .expect("a file is an argument")
            };
            assert_eq!(
                target.arguments["recording"],
                said(&recording),
                "the recording is the report's own"
            );
            assert_eq!(
                target.arguments["attachments"],
                Value::Array(vec![said(&picture)]),
                "and the picture goes beside it"
            );

            // A report goes to the one place it goes, and a computer told nowhere sends none.
            let ada = Recipient::found(RecipientKind::Person, "ad".repeat(32), "Ada");
            assert!(
                addressed(
                    Some(&destination),
                    "w_notes",
                    &[report.clone(), ada.clone()]
                )
                .is_err(),
                "a report stands beside no other recipient"
            );
            assert!(
                addressed(None, "w_notes", std::slice::from_ref(&report)).is_err(),
                "a computer told nowhere to send a report sends none"
            );
            // Every other send is a message in the workspace its box was opened in, to whom it named.
            let Addressed::Message { workspace, to } =
                addressed(None, "w_notes", std::slice::from_ref(&ada)).expect("an ordinary send")
            else {
                panic!("an ordinary send is a message");
            };
            assert_eq!(workspace, "w_notes");
            assert_eq!(to, [ada], "and goes to whom it named");
        }
    }
}
