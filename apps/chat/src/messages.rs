//! The chat's messages: the verbs of a part ([`ChatMessages`]), which part each
//! changes ([`Chained`]), and the parts a device holds read as one timeline
//! ([`Messages`]).
//!
//! The framework places each message: a post with the newest, anything done
//! to a message (an edit, a reaction, a reply in its thread, an agent's log
//! of its work on it) where the message is ([`Chained::beside`]). What a part's verb keeps
//! from the workspace's chat resource — who a mention names — is filled in where it
//! is invoked ([`Chained::prepare`]), so the verb reads only its own part
//! and replays the same everywhere.
//!
//! Who posts, reacts or edits is whoever invokes the verb
//! ([`ActionCx::author`]), never an argument; when is the action's time
//! ([`ActionCx::now_ms`]), which a post carries as a hybrid logical clock of
//! its conversation ([`ChatMessages::prepare`]): it reads after every message
//! the writing device had seen there.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tilo_document::ClipBlock;
use ui::framework::Chained;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::search_index::{IndexKind, IndexPerson};
use ui::tilo_resource::{
    self, ActionCx, Atom, Id, JsonSchema, NonBlank, Refusal, SearchEntry, Segment,
};

use crate::cause::{Cause, Starting};
use crate::chat::{
    Chat, MAX_AGENT_NAME_BYTES, MAX_MESSAGE_TEXT_BYTES, Message, Posting, ProviderStream, Revision,
    ShareAccess, Step, Transition, WindowShare, Work, WorkUpdate, author, reply_id_at,
};
use crate::message_view::View;
use crate::parts::{self, ChatMessages};
use crate::question::{Choice, Question};

/// Refuses anyone but `message`'s author: editing and deleting a message is
/// its author's. Removing someone else's is moderation (`remove-message`),
/// which the workspace's roles decide.
fn own(cx: &ActionCx, message: &Message) -> Result<(), Refusal> {
    if author(cx)? == message.author {
        Ok(())
    } else {
        Err(Refusal::new(
            "only its author can change a message; a moderator can remove it",
        ))
    }
}

/// Refuses anyone who does not end the share `message` carries. Its author ends it, as they
/// change anything of their message. So does the person it was shared for
/// ([`shared_for`]), when they invoke the verb themselves, on any device of theirs: an agent
/// shares in its person's name, and that person takes the share back without removing the
/// message. Nobody else does: not another member, and not what acts for that person.
fn ends_share(cx: &ActionCx, message: &Message) -> Result<(), Refusal> {
    let invoker = author(cx)?;
    if invoker == message.author {
        return Ok(());
    }
    let themselves = cx
        .invoked_by()
        .as_ref()
        .is_none_or(tilo_actions::Acting::is_person);
    let theirs = shared_for(message.acting.as_ref())
        .is_some_and(|person| invoker.person_id() == Some(person));
    if themselves && theirs {
        Ok(())
    } else {
        Err(Refusal::new(
            "only its sharer, or the person their agent shared it for, stops sharing a window",
        ))
    }
}

/// The one person the software that posted a message acted for, as the message keeps who wrote
/// it for whom (`acting`: pinned from whoever invoked the verb that posted it, never an
/// argument). None for a person's own message and for software that acted for nobody.
pub(crate) fn shared_for(acting: Option<&tilo_actions::Acting>) -> Option<&str> {
    acting
        .filter(|acting| !acting.is_person())
        .and_then(crate::cause::led_to)
}

/// The longest name a delivered message is shown under.
const MAX_SHOWN_NAME_BYTES: usize = 128;

/// A post's time in milliseconds: when it was `written` elsewhere (in Unix
/// seconds, as a bot brings one over on someone's behalf) when `brought`,
/// never later than now; else now. A member's own post is always now, so no
/// one backdates theirs.
fn written_at(cx: &ActionCx, written: Option<i64>, brought: bool) -> i64 {
    let now = cx.now_ms();
    written
        .filter(|_| brought)
        .map_or(now, |written| written.saturating_mul(1_000).clamp(0, now))
}

/// The id of the message imported into `channel` from `origin`, written there
/// at `created_ms` saying `text`: the same message imported again names the
/// same one. It is known there by its reference or its page, else by its author.
fn imported_id(
    channel: &Id,
    origin: &ui::tilo_resource::Origin,
    created_ms: i64,
    text: &str,
) -> Id {
    let there = origin
        .reference
        .as_deref()
        .or(origin.url.as_deref())
        .unwrap_or(&origin.author);
    crate::transcript::delivered_id(
        channel,
        &format!("{}\u{1f}{there}", origin.source),
        created_ms,
        text,
    )
}

/// Oldest first: when it was written, ties by id. The time is the one the
/// action that wrote the message carried, which every replica reads from the
/// message itself.
fn written_first((a_id, a): &(&Id, &Message), (b_id, b): &(&Id, &Message)) -> std::cmp::Ordering {
    (a.ts, *a_id).cmp(&(b.ts, *b_id))
}

impl ChatMessages {
    /// The messages of this part as the conversation reads them, oldest first:
    /// when each was written, ties by id. The part holds them in the place
    /// each device made for a message against what that device had seen, which
    /// is not when it was written.
    #[must_use]
    pub fn in_written_order(&self) -> Vec<(&Id, &Message)> {
        let mut held: Vec<(&Id, &Message)> = self.messages.iter().collect();
        held.sort_by(written_first);
        held
    }

    /// What search finds in the messages this part keeps: each message's
    /// words, opening the message in the chat it was posted in.
    ///
    /// The window offers none of its own: messages are kept here, in the
    /// resource, so a device that never opened Chat still finds them, and one
    /// that deletes a message has it gone from search with it.
    #[must_use]
    pub fn search(&self) -> Vec<SearchEntry> {
        self.messages
            .iter()
            .filter_map(|(id, message)| {
                let text = message.text.trim();
                (!text.is_empty()).then(|| {
                    // Who wrote it and where are ids here: the workspace's
                    // own document holds the names, so a row says them by what
                    // those resources are called now.
                    let sender = IndexPerson {
                        name: String::new(),
                        anchor: item_resource_id::<Chat>(
                            "people",
                            &crate::chat::member_marks(&message.author),
                        ),
                        agent: !message.author.is_person(),
                        identity: message.author.id().to_owned(),
                    };
                    let place = IndexPerson {
                        name: String::new(),
                        anchor: item_resource_id::<Chat>("channels", &message.channel),
                        identity: message.channel.as_str().to_owned(),
                        agent: false,
                    };
                    SearchEntry::new(text, "")
                        .anchored(item_resource_id::<Chat>("messages", id))
                        .of_kind(IndexKind::Message)
                        .with_icon("message-circle")
                        .showing(vec![sender])
                        .in_place(place)
                        .at(message.ts.div_euclid(1_000))
                })
            })
            .collect()
    }
}

impl ChatMessages {
    /// Append a message to `channel` by the invoker `cx` names, at `ts`, refusing what no
    /// one should see: a reply to a reply or to another channel's message,
    /// an oversized body or an invalid attachment. Whether the channel takes
    /// posts is the chat resource's to say, where the post is invoked.
    pub(crate) fn append(
        &mut self,
        cx: &ActionCx,
        id: Id,
        channel: &Id,
        ts: i64,
        posting: Posting,
    ) -> Result<Id, Refusal> {
        let author = author(cx)?;
        if posting.text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        if posting
            .attachments
            .iter()
            .any(|attached| !crate::chat::valid_attached(attached))
        {
            return Err(Refusal::new(
                "an attachment is a bounded address, words worth reading, or a file this workspace holds",
            ));
        }
        if let Some(parent) = &posting.parent {
            let parent = self
                .messages
                .get(parent)
                .ok_or_else(|| Refusal::new("the message being answered no longer exists"))?;
            if parent.parent.is_some() || parent.channel != *channel {
                return Err(Refusal::new(
                    "a reply answers a top-level message of its channel",
                ));
            }
            // A quoted span is a run of the message it answers: it stands inside that
            // message's text, on its own character boundaries, so pressing the quote reveals
            // the words it shows.
            if let Some(quoted) = &posting.quoted
                && (!quoted.valid()
                    || usize::try_from(quoted.end)
                        .ok()
                        .filter(|&end| end <= parent.text.len())
                        .and_then(|end| {
                            let start = usize::try_from(quoted.start).ok()?;
                            (start < end
                                && parent.text.is_char_boundary(start)
                                && parent.text.is_char_boundary(end))
                            .then_some(())
                        })
                        .is_none())
            {
                return Err(Refusal::new(
                    "a quoted span is a run of the message it answers",
                ));
            }
        } else if posting.quoted.is_some() {
            return Err(Refusal::new("a quote answers a message"));
        }
        self.messages.push(
            id.clone(),
            Message {
                body: posting.body,
                parent: posting.parent,
                quoted: posting.quoted,
                mentions: posting.mentions,
                attachments: posting.attachments,
                appearance: posting.appearance.map(Into::into),
                acting: cx.invoked_by().filter(|acting| !acting.is_person()),
                author_name: Some(cx.acting_name()).filter(|name| !name.trim().is_empty()),
                origin: posting.origin,
                share: posting.share,
                question: posting.question,
                cause: posting.cause,
                view: posting.view,
                ..Message::new(channel.clone(), author, ts, posting.text)
            },
        );
        Ok(id)
    }
}

impl ChatMessages {
    /// The question the message `message` asks.
    fn question_of(&mut self, message: &Id) -> Result<&mut Question, Refusal> {
        self.messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?
            .question
            .as_mut()
            .ok_or_else(|| Refusal::new("the message asks nothing"))
    }

    /// Refuses anyone but the asker of the question `message` asks: taking
    /// it back and saying what came of it are its asker's.
    fn asked_by(&self, cx: &ActionCx, message: &Id) -> Result<(), Refusal> {
        let row = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if author(cx)? == row.author {
            Ok(())
        } else {
            Err(Refusal::new("only its asker changes a question"))
        }
    }
}

/// A message to post: the parameters of [`ChatMessages::post`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NewMessage {
    /// The id the sender gives the message, so it knows which message it posted before the post
    /// is written (a part still arriving takes it later); leave it out to have one made
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    /// The channel
    pub channel: Id,
    /// The message; @Name mentions a member
    pub text: String,
    /// The message it answers in a thread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Id>,
    /// The run of the message it answers that it quotes, when it answers a span rather than the
    /// whole message: the words as they read, and where they stand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted: Option<crate::chat::Quote>,
    /// Its paragraphs as written, with their formatting
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<ClipBlock>>,
    /// What it carries besides its words: resources and parts of records it
    /// shares, words it quotes, files another action put in the store
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<tilo_reference::Attached>,
    /// Filled in where it is posted; leave it out
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions: Option<Vec<crate::mention::Mention>>,
    /// When it was written, in Unix seconds, for a message a bot brings over
    /// from elsewhere; leave it out
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written: Option<i64>,
    /// Where it was first written, when it is moved here from another chat:
    /// its author there, the tool, its page and when it was written. It
    /// shows as its author's message at that time, while the message stays
    /// the mover's in its history
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ui::tilo_resource::Origin>,
}

/// Bring a part of version 1 up: an answered question kept what its person typed beside a
/// choice (`question.answered.detail`), and a question is now answered only with one of its
/// choices, so that field goes.
fn upgrade(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    use automerge::transaction::Transactable as _;
    use automerge::{ROOT, ReadDoc as _};
    use ui::tilo_resource::evolution;
    if from != 1 {
        return Err(ui::tilo_resource::ModelError::invalid(format!(
            "no chat messages migration from {from}"
        )));
    }
    for message in evolution::collection_items(doc, &ROOT, "messages")? {
        let Some(question) = evolution::map_at(doc, &message, "question")? else {
            continue;
        };
        let Some(answered) = evolution::map_at(doc, &question, "answered")? else {
            continue;
        };
        if doc.get(&answered, "detail")?.is_some() {
            doc.delete(&answered, "detail")?;
        }
    }
    Ok(())
}

// The window's effects on a message (reveal it, read it) are offered on
// each message the chain publishes, as on the resource's other items.
#[ui::tilo_resource::resource(
    kind = "chat.messages",
    params = chat_messages,
    version = 2,
    migrate = upgrade,
    search = ChatMessages::search,
    effects = crate::effects::ChatEffect,
    item_views = crate::effects::item_views,
)]
impl ChatMessages {
    /// Post a message.
    ///
    /// `@Name` of a member becomes a mention of them.
    #[action(incoming_message, did = "posted")]
    pub fn post(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] message: NewMessage,
    ) -> Result<(), Refusal> {
        let NewMessage {
            id: named,
            channel,
            text,
            parent,
            quoted,
            body,
            attachments,
            mentions,
            written,
            origin,
        } = message;
        if text.trim().is_empty() && attachments.is_empty() && parent.is_none() {
            return Err(Refusal::new("a message says something"));
        }
        let origin = ui::tilo_resource::Origin::checked_option(origin)?;
        let id = match named {
            Some(named) if self.messages.get(&named).is_some() => {
                return Err(Refusal::new("a message with that id is already here"));
            }
            Some(named) if !valid_message_id(&named) => {
                return Err(Refusal::new(
                    "a message's id is up to 64 letters, digits and dashes",
                ));
            }
            Some(named) => named,
            None => cx.id(),
        };
        let appearance = cx.appearance();
        // A message moved from elsewhere stands where it was written there.
        let ts = match origin.as_ref().and_then(|origin| origin.created_ms) {
            Some(created_ms) => created_ms.clamp(0, cx.now_ms()),
            None => written_at(cx, written, appearance.is_some()),
        };
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text,
                body: body.unwrap_or_default(),
                parent,
                quoted,
                attachments,
                appearance,
                mentions: mentions.unwrap_or_default(),
                origin,
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Post a file.
    ///
    /// Posts a file another step handed over (a file from Files, a picture)
    /// under the name given, with words about it: a picture shows as one,
    /// anything else as a file to download.
    #[action(incoming_message, prepare = crate::chat::prepare_file_post)]
    pub fn post_file(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The file another step handed over, by its content address", content)]
        content: String,
        #[param("The file's name, with its extension", example = "report.pdf")] name: String,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("Filled in from the content where it is posted; leave it out")] attachment: Option<
            tilo_reference::Attached,
        >,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        let attachment = attachment
            .filter(|attachment| {
                attachment.blob() == Some(content.as_str()) && attachment.label() == name
            })
            .ok_or("the file is described where it is posted")?;
        let id = cx.id();
        let ts = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text: text.unwrap_or_default(),
                attachments: vec![attachment],
                appearance: cx.appearance(),
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Share live resource.
    ///
    /// Post the address of something, with words about it; whoever opens it
    /// still needs their own access to it.
    #[action(incoming_message, audience = "agents")]
    pub fn share_resource(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The thing: the app and record it lives in, and its kind, id and label")]
        attached: tilo_reference::ResourceAt,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        if !crate::chat::valid_resource_at(&attached) {
            return Err(Refusal::new(
                "the address of a resource of an existing app is required",
            ));
        }
        let label = attached.reference.label.clone();
        let text = text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| label.clone());
        let id = cx.id();
        let ts = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text,
                attachments: vec![tilo_reference::Attached::resource(attached, None, label)],
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Share a window live.
    ///
    /// Post a window's resource so everyone in the channel sees it as it is
    /// now, drawn by its own app from their own copy of its resource, and opens
    /// it with `access`: `view` (the default) or `edit`. Its sharer stops
    /// sharing it with `stop-sharing`.
    #[action(incoming_message)]
    pub fn share_window(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The window's resource: the app and record it lives in, its kind, id and label")]
        attached: tilo_reference::ResourceAt,
        #[param("What the people who open it may do: view or edit")] access: Option<ShareAccess>,
        #[param("Words accompanying it")] text: Option<String>,
        #[param("Filled in where it is posted; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        if !crate::chat::valid_resource_at(&attached) || attached.reference.span.is_some() {
            return Err(Refusal::new(
                "a window is shared as a whole resource of a resource",
            ));
        }
        let label = attached.reference.label.clone();
        let id = cx.id();
        let ts = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text: text.unwrap_or_default().trim().to_owned(),
                attachments: vec![tilo_reference::Attached::resource(attached, None, label)],
                share: Some(WindowShare {
                    access: access.unwrap_or_default(),
                    open: true,
                    ended: false,
                }),
                mentions: mentions.unwrap_or_default(),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Stop sharing.
    ///
    /// End a window you shared, or one your own agent shared for you: the
    /// message keeps saying what was shared, and no one sees or opens it
    /// from there any more.
    #[action(item = messages)]
    pub fn stop_sharing(&mut self, cx: &mut ActionCx, message: &Id) -> Result<(), Refusal> {
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        ends_share(cx, row)?;
        let share = row
            .share
            .as_mut()
            .ok_or_else(|| Refusal::new("the message shares no window"))?;
        share.ended = true;
        share.open = false;
        Ok(())
    }

    /// Change what a shared window lets people do.
    ///
    /// Switch a window you share live between `view` and `edit`, in place:
    /// the workspace grants what the new access gives from then on, and takes
    /// back what the old one gave.
    #[action(item = messages)]
    pub fn set_share_access(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("What the people who open it may do now: view or edit")] access: ShareAccess,
    ) -> Result<(), Refusal> {
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        let share = row
            .share
            .as_mut()
            .ok_or_else(|| Refusal::new("the message shares no window"))?;
        if share.ended {
            return Err(Refusal::new("the window is no longer shared"));
        }
        share.access = access;
        Ok(())
    }

    /// Say whether a shared window is open.
    ///
    /// Its sharer's Chat says so when they close the window or open it
    /// again, so the embed shows whether it is live.
    #[action(item = messages, audience = "agents")]
    pub fn share_open(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        open: bool,
    ) -> Result<(), Refusal> {
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        let share = row
            .share
            .as_mut()
            .ok_or_else(|| Refusal::new("the message shares no window"))?;
        // An ended share stays ended: reopening the window shares nothing.
        share.open = open && !share.ended;
        Ok(())
    }

    /// Deliver message.
    ///
    /// Adds a message someone outside the workspace wrote (a seeded
    /// conversation, an import, an integration relaying a line), shown to
    /// every reader under their name; it stays yours, as a bridge's post
    /// does. Given when it was written, delivering it again changes nothing.
    #[action(incoming_message, audience = "agents")]
    pub fn deliver_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("Who wrote it: the name every reader sees")] author: NonBlank,
        #[param("The message")] body: NonBlank,
        #[param("When it was written, in Unix seconds; now when absent")] ts: Option<i64>,
    ) -> Result<(), Refusal> {
        let shown = author.into_string();
        if shown.len() > MAX_SHOWN_NAME_BYTES {
            return Err(Refusal::new(
                "a delivered message names its author, in a short name",
            ));
        }
        let body = body.into_string();
        let id = match ts {
            Some(ts) => crate::transcript::delivered_id(&channel, &shown, ts, &body),
            None => cx.id(),
        };
        if self.messages.contains(&id) {
            return Ok(());
        }
        let now = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts.map_or(now, |ts| ts.saturating_mul(1_000).clamp(0, now)),
            Posting {
                text: body,
                appearance: Some(tilo_actions::Appearance {
                    name: shown,
                    avatar: None,
                    persona: None,
                }),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Import message.
    ///
    /// Adds a message written elsewhere (another chat's history, a bridged
    /// server's channel) at the time it was written there, however much newer
    /// what the conversation already holds is: it reads among them in the
    /// order they were written, shown as imported, by its author there and
    /// the tool it came from. A post written here takes now; only an import
    /// keeps an earlier time. Importing it again changes nothing.
    #[action(incoming_message, audience = "agents")]
    pub fn import_message(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("The message")] text: String,
        #[param("Where it was written: its author there, the tool, its page and when")]
        origin: ui::tilo_resource::Origin,
        #[param("The message it answers in a thread, already here")] parent: Option<Id>,
        #[param("Files another step put in the store that it carries")] attachments: Option<
            Vec<tilo_reference::Attached>,
        >,
    ) -> Result<(), Refusal> {
        let attachments = attachments.unwrap_or_default();
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(Refusal::new("a message says something"));
        }
        let origin = origin.checked()?;
        let created_ms = origin
            .created_ms
            .ok_or_else(|| Refusal::new("an imported message says when it was written"))?;
        let id = imported_id(&channel, &origin, created_ms, &text);
        if self.messages.contains(&id) {
            return Ok(());
        }
        let appearance = cx.appearance();
        self.append(
            cx,
            id,
            &channel,
            created_ms.clamp(0, cx.now_ms()),
            Posting {
                text,
                parent,
                attachments,
                appearance,
                origin: Some(origin),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Edit message.
    ///
    /// Its earlier text is kept in its history.
    #[action(item = messages)]
    pub fn edit_message(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        text: String,
        body: Option<Vec<ClipBlock>>,
        #[param("Filled in where it is edited; leave it out")] mentions: Option<
            Vec<crate::mention::Mention>,
        >,
    ) -> Result<(), Refusal> {
        if text.len() > MAX_MESSAGE_TEXT_BYTES {
            return Err(Refusal::new("the message is too long"));
        }
        let ts = cx.now_ms();
        let body = body.unwrap_or_default();
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        if row.text == text && row.body == body {
            return Ok(());
        }
        if row.text != text {
            row.history.push(Revision {
                ts,
                text: std::mem::take(&mut row.text),
            });
            row.mentions = mentions.unwrap_or_default();
            row.text = text;
        }
        row.body = body;
        row.edited = true;
        Ok(())
    }

    /// Delete message.
    ///
    /// One of your own.
    #[action(item = messages, destructive)]
    pub fn delete_message(&mut self, cx: &mut ActionCx, message: &Id) -> Result<(), Refusal> {
        let row = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        own(cx, row)?;
        self.messages
            .remove(message)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such message"))
    }

    /// Remove message.
    ///
    /// Someone else's message, as a moderator: the workspace's roles decide who
    /// may (Moderators and the owner).
    #[action(item = messages, moderation, destructive)]
    pub fn remove_message(&mut self, message: &Id) -> Result<(), Refusal> {
        self.messages
            .remove(message)
            .map(drop)
            .ok_or_else(|| Refusal::new("there is no such message"))
    }

    /// Pin message.
    ///
    /// Pinned messages are listed for everyone; unpinning takes it off the
    /// list.
    #[action(item = messages)]
    pub fn pin(&mut self, message: &Id, pinned: bool) -> Result<(), Refusal> {
        self.messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?
            .pinned = pinned;
        Ok(())
    }

    /// Ask again for an answer.
    ///
    /// The message an agent answered with a turn that failed: asks its agent again on that same
    /// message, rather than posting the question a second time. The message moves to the next
    /// attempt, so the reply the agent writes now is a reply of its own — beside the one that
    /// failed, which stays in the transcript as what happened — and the turn for the message is
    /// delivered again.
    ///
    /// It names the reply the person gave up on: only the attempt the message is in may be asked
    /// again, so a stale Retry (one already pressed, one a newer attempt replaced) changes nothing.
    #[action(item = messages)]
    pub fn retry(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("The reply the person gave up on")] reply: &Id,
    ) -> Result<(), Refusal> {
        // Everyone who asks again is a person, never the agent whose words failed.
        author(cx)?;
        let by = self
            .messages
            .get(reply)
            .map(|answered| answered.author.clone())
            .ok_or_else(|| Refusal::new("there is no such reply"))?;
        let summons = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if summons.parent.is_some() {
            return Err(Refusal::new("only a question is asked again"));
        }
        // The reply has to be the one its own author's current attempt derives: the answer
        // written for this question, and not one a later attempt replaced.
        if *reply != reply_id_at(message, &by, summons.attempts) {
            // Asked already, or replaced: the person's Retry is spent, and asking again would
            // start a turn nobody pressed for.
            return Ok(());
        }
        summons.attempts = summons.attempts.saturating_add(1);
        Ok(())
    }

    /// Record agent work.
    ///
    /// An agent's account of what it is doing with the message that
    /// summoned it, kept on its reply to that message: the reply is made
    /// the first time, and `say` gives it its words once, while it has none.
    #[action(item = messages, audience = "agents")]
    pub fn record_work(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("Where the work is and what was done")] work: WorkUpdate,
        #[param("The reply's words, when it has none yet")] say: Option<String>,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let now = cx.now_ms();
        let (state, trace) = crate::model::AgentWork::from_update(&work)
            .map_err(Refusal::new)?
            .into_parts();
        let summons = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        let channel = summons.channel.clone();
        let thread = summons.parent.clone().unwrap_or_else(|| message.clone());
        // The attempt the answer belongs to: the first unless a person asked again, so what a
        // retried turn writes never overwrites the reply that failed.
        let reply = reply_id_at(message, &me, summons.attempts);
        if !self.messages.contains(&reply) {
            self.append(
                cx,
                reply.clone(),
                &channel,
                now,
                Posting {
                    parent: Some(thread),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&reply)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if row.author != me {
            return Err(Refusal::new("only the reply's author records its work"));
        }
        let kept = row.work.get_or_insert_with(Work::default);
        let unchanged = kept
            .state
            .as_ref()
            .is_some_and(|transition| transition.state == state);
        if !unchanged {
            kept.state = Some(Transition { state, since: now });
        }
        // The run's working resource of a failure, kept beside the words the
        // reply shows; a later update without one leaves it as it was.
        if work.detail.is_some() {
            kept.failure_detail.clone_from(&work.detail);
        }
        // What answered the turn is kept as its last word on it; a later
        // update without one leaves it as it was.
        if let Some(answered) = &work.answered {
            kept.answered = Some(answered.clone());
        }
        for step in trace {
            let id = Id::from_raw(step.id.clone());
            let row = Step {
                id: Atom(step.id),
                sequence: step.sequence,
                resource: Atom(step.resource),
                label: step.label,
                outcome: Some(step.outcome),
            };
            if let Some(known) = kept.trace.find_mut(&id) {
                *known = row;
            } else {
                kept.trace.push(row);
            }
        }
        if let Some(say) = say
            && row.text.is_empty()
            && say.len() <= MAX_MESSAGE_TEXT_BYTES
        {
            row.text = say;
        }
        Ok(())
    }

    /// Ask your person.
    ///
    /// Post a message that asks the person you act for something and waits
    /// for their answer, on every device of theirs and with no time limit.
    /// `answers` are the buttons under it; the one they press on a `choice`
    /// is posted as their own message. Only they answer it, and
    /// `withdraw-question` takes it back.
    ///
    /// A question is answered only with one of its `answers`, so it offers
    /// at least one. To hear your person in their own words, ask in a plain
    /// message and read their reply. Never ask in the conversation for a
    /// password, a key or any other secret: everyone in it reads what your
    /// person writes. The vault's `ask` asks for it in a protected field on
    /// their own device instead.
    #[action(incoming_message, audience = "agents")]
    pub fn ask(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("What is asked, in words")] text: NonBlank,
        #[param("The answers offered, in order: each an id and what its button says")]
        answers: Option<Vec<Choice>>,
        #[param("The kind of question; a choice when absent")] kind: Option<String>,
        #[param("What the kind keeps with the question; leave it out for a choice")] detail: Option<
            serde_json::Value,
        >,
        #[param("The message it answers in a thread")] parent: Option<Id>,
    ) -> Result<(), Refusal> {
        let person = crate::question::asked_person(cx)?;
        let question = Question::asked(person, kind, answers.unwrap_or_default(), detail)?;
        let id = cx.id();
        let ts = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text: text.into_string(),
                parent,
                question: Some(question),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Answer a question.
    ///
    /// The answer of the person a message asks: one of the answers it
    /// offers, pressed by them on a device of their own. Nobody acting for
    /// them answers it, and nobody else does.
    #[action(item = messages, audience = "people")]
    pub fn answer_question(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("The answer, by its id")] answer: String,
        #[param("The device it is answered on; filled in where it is pressed")] device: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let at = cx.now_ms();
        self.question_of(message)?.answer(cx, &answer, device, at)
    }

    /// Withdraw a question.
    ///
    /// Take back a question you asked that nobody answered yet: it stops
    /// waiting on every device, and its message keeps saying what was asked.
    #[action(item = messages, audience = "agents")]
    pub fn withdraw_question(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("Why, in words its person reads")] why: Option<String>,
    ) -> Result<(), Refusal> {
        let at = cx.now_ms();
        self.asked_by(cx, message)?;
        self.question_of(message)?.withdraw(why, at)
    }

    /// Say what came of an answer.
    ///
    /// What you did with the answer to a question you asked: `started`
    /// before you carry it out, then `done` or `failed`, or `dropped` when
    /// nothing was done. A question settles once.
    #[action(item = messages, audience = "agents")]
    pub fn settle_question(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("What came of it: started, done, dropped or failed")] outcome: String,
        #[param("What its person is told about it")] detail: Option<String>,
    ) -> Result<(), Refusal> {
        let at = cx.now_ms();
        self.asked_by(cx, message)?;
        self.question_of(message)?.settle(&outcome, detail, at)
    }

    /// Ask an agent.
    ///
    /// Start a turn of the agents in a conversation with an instruction,
    /// for the person you are or act for: a reminder that is due, an event
    /// that fired, news of work started for them. The message says what
    /// started it, and each agent answers it like a message its person
    /// wrote, in its reply to it. One `key` is one message however often it
    /// is asked, so asking again after a restart starts nothing.
    // Not a message someone received: an automation that fires when a message
    // arrives does not fire on what asks an agent (it would fire on its own
    // ask), and a bridge does not carry an instruction out of the workspace.
    #[action]
    pub fn ask_agent(
        &mut self,
        cx: &mut ActionCx,
        // Its words stand first: a verb whose first parameter is an id is read
        // as naming one of the part's messages, and a conversation is none.
        #[param("The instruction, in words")] text: NonBlank,
        #[param("The conversation the answer goes to")] channel: Id,
        #[param("What started it: its kind, what a person calls it, and your own key for it")]
        cause: Starting,
        #[param("Filled in where it is asked; leave it out")] members: Option<
            Vec<ui::tilo_resource::MemberId>,
        >,
        #[param("The message it is carried by: filled in where it is asked; leave it out")]
        message: Option<Id>,
    ) -> Result<(), Refusal> {
        // What is kept where this message would be, so an ask under a key
        // asked before finds it; the verb derives the message itself.
        let _ = message;
        let starter = crate::cause::starter(cx)?;
        let cause = Cause::started(cause, &starter)?;
        let asker = author(cx)?;
        crate::cause::takes(
            &channel,
            members.as_deref().unwrap_or_default(),
            &cause.person,
            &asker,
        )?;
        let id = cause.message();
        if let Some(held) = self.messages.get(&id) {
            // Asked before under this key: the message stands, and nothing
            // is started twice.
            let same = held.channel == channel
                && held
                    .cause
                    .as_ref()
                    .is_some_and(|before| before.person == cause.person && before.key == cause.key);
            return if same {
                Ok(())
            } else {
                Err(Refusal::new(
                    "another message already stands where this key's would",
                ))
            };
        }
        let ts = cx.now_ms();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text: text.into_string(),
                cause: Some(cause),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Stop an agent's work.
    ///
    /// Stops the turn a message asks for: the agent working on it ends its
    /// work where it stands, and nothing answers the message until someone
    /// asks again (`retry`). Whoever the turn is for stops it, themselves
    /// or through what acts for them.
    #[action(item = messages)]
    pub fn stop_agent(&mut self, cx: &mut ActionCx, message: &Id) -> Result<(), Refusal> {
        let starter = crate::cause::starter(cx)?;
        let at = cx.now_ms();
        // Whoever shows an agent at work holds its reply, where the work is kept: a stop that
        // names the reply is a stop of the message it answers, held to the same check. An
        // answer longer than one message goes on in further messages, and a stop that names
        // any of them is a stop of the message the first of them answers.
        let answered = self.asked_with(message);
        let message = answered.as_ref().unwrap_or(message);
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        let asked_for = crate::summons::asks_for(row);
        if asked_for.is_none() || asked_for.as_deref() != starter.person.person_id() {
            return Err(Refusal::new(
                "only the person an agent works for stops its work",
            ));
        }
        let attempt = row.attempts;
        if row
            .stopped
            .as_ref()
            .is_none_or(|stopped| stopped.attempt != attempt)
        {
            row.stopped = Some(crate::summons::Stopped { attempt, at });
        }
        Ok(())
    }

    /// Post a view.
    ///
    /// Post a message that carries a view: a small piece of interface
    /// written in the app language over what the workspace's apps keep,
    /// shown live in the message to everyone who reads the conversation.
    /// `answer-with-view` says how a view is written and when one is the
    /// right answer; an agent answering the message that asked it uses that
    /// verb. `text` is what the message says in words: every reader sees
    /// it, and it is all that search, notifications and a reader that
    /// cannot show a view have, so say there what the view shows.
    #[action(incoming_message)]
    pub fn post_view(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        #[param("What the message says, in words: what the view shows")] text: NonBlank,
        #[param("The view: one `view` declaration and the functions it calls")] source: String,
        #[param("The apps the view reads, each as the view calls it")] reads: Vec<String>,
        #[param("The message it answers in a thread")] parent: Option<Id>,
    ) -> Result<(), Refusal> {
        let ts = cx.now_ms();
        let view = View::written(source, reads, ts)?;
        let id = cx.id();
        self.append(
            cx,
            id,
            &channel,
            ts,
            Posting {
                text: text.into_string(),
                parent,
                view: Some(view),
                ..Posting::default()
            },
        )
        .map(drop)
    }

    /// Answer with a view.
    ///
    /// Put a view on your reply to the message that asked you: a small
    /// piece of interface written in the app language over what the
    /// workspace's apps keep. It is shown live in your reply to everyone
    /// who reads the conversation, each as themselves: it shows what that
    /// reader may read, and it is drawn again whenever what it reads
    /// changes, whoever changed it. Answer with a view when what was asked
    /// for keeps changing or is easier seen than read: a count, a list, a
    /// status, things someone wants to keep an eye on. Answer in words
    /// alone when the answer is one fact that will not move.
    ///
    /// `source` is one `view` declaration and the functions it calls, and
    /// nothing else: no types, no actions, no `use`. It names an app by the
    /// last part of its id (`tasks` for `so.tilo.tasks`) and reads what
    /// that app keeps for the workspace, by the names its `read` shows:
    /// `view projects() { for a in tasks.tracker.areas { text(a.name) } }`.
    /// A control invokes one of the app's own actions, named as the app
    /// names it with `_` for `-`: `button("Delete", on:
    /// tasks.delete_area(tasks.tracker, a.id))`. In a view you wrote a
    /// press only suggests: the reader is shown the action with its
    /// arguments, and allows that one thing or not.
    ///
    /// `reads` lists every app the view reads, each as the view calls it
    /// (`["tasks"]`); the view is given those apps and no other. `say` is
    /// your reply's words when it has none yet: every reader sees them, and
    /// they are all that search, notifications and a reader that cannot
    /// show a view have, so say there what the view shows. Calling it again
    /// in the same turn writes the view again, as `rewrite-view` does.
    #[action(item = messages, audience = "agents")]
    pub fn answer_with_view(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("The view: one `view` declaration and the functions it calls")] source: String,
        #[param("The apps the view reads, each as the view calls it")] reads: Vec<String>,
        #[param("The reply's words, when it has none yet: what the view shows")] say: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let me = author(cx)?;
        let at = cx.now_ms();
        let summons = self
            .messages
            .get(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        let channel = summons.channel.clone();
        let thread = summons.parent.clone().unwrap_or_else(|| message.clone());
        // The reply of the attempt the message is in: the one the agent's work is kept on.
        let reply = reply_id_at(message, &me, summons.attempts);
        // Checked before the reply is made: a view that is refused leaves nothing behind.
        let first = View::written(source.clone(), reads.clone(), at)?;
        if !self.messages.contains(&reply) {
            self.append(
                cx,
                reply.clone(),
                &channel,
                at,
                Posting {
                    parent: Some(thread),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&reply)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if row.author != me {
            return Err(Refusal::new("only the reply's author gives it a view"));
        }
        match row.view.as_mut() {
            Some(view) => view.written_again(source, reads, at)?,
            None => row.view = Some(first),
        }
        if let Some(say) = say
            && row.text.is_empty()
            && say.len() <= MAX_MESSAGE_TEXT_BYTES
        {
            row.text = say;
        }
        Ok(())
    }

    /// Write a view again.
    ///
    /// Replace the view a message of yours carries by a new writing of it:
    /// the same message then shows the new view to everyone, and the form
    /// before is kept with it. `message` is the message that carries the
    /// view, not the one that asked for it. Only the message's author
    /// writes its view again.
    #[action(item = messages)]
    pub fn rewrite_view(
        &mut self,
        cx: &mut ActionCx,
        message: &Id,
        #[param("The view: one `view` declaration and the functions it calls")] source: String,
        #[param("The apps the view reads, each as the view calls it")] reads: Vec<String>,
    ) -> Result<(), Refusal> {
        let at = cx.now_ms();
        let row = self
            .messages
            .get_mut(message)
            .ok_or_else(|| Refusal::new("there is no such message"))?;
        if author(cx)? != row.author {
            return Err(Refusal::new(
                "only its author writes a message's view again",
            ));
        }
        row.view
            .as_mut()
            .ok_or_else(|| Refusal::new("the message carries no view"))?
            .written_again(source, reads, at)
    }

    /// Carry the counts of the parts before.
    ///
    /// What a part keeps of the parts before it: for each, how many
    /// top-level messages each channel has there. The chain writes it into
    /// a part as it starts, from the part before.
    #[action(housekeeping)]
    pub fn carry_counts(
        &mut self,
        #[param("Top-level messages per channel, by part number")]
        earlier: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, i64>,
        >,
    ) {
        self.earlier = earlier;
    }

    /// Record ordered provider output, never executable model actions.
    ///
    /// The invoker owns the reply; another author cannot append to it.
    /// Retrying requires a fresh reply and run, keeping partial answers intact.
    #[action(audience = "agents")]
    pub fn stream_output(
        &mut self,
        cx: &mut ActionCx,
        #[param("The channel")] channel: Id,
        reply: String,
        name: Option<NonBlank>,
        event: serde_json::Value,
    ) -> Result<(), Refusal> {
        use crate::agent_stream::{AgentOutput, AgentStream, AgentStreamEvent};
        let event: AgentStreamEvent = serde_json::from_value(event)
            .map_err(|_| Refusal::new("invalid provider output event"))?;
        if name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_AGENT_NAME_BYTES)
        {
            return Err(Refusal::new("provider display name is too long"));
        }
        let id = Id::from_raw(reply);
        let existing = self.messages.get(&id);
        if let Some(row) = existing {
            own(cx, row)?;
            if row.channel != channel {
                return Err(Refusal::new("reply belongs to another channel"));
            }
        }
        let prior = existing.and_then(|row| row.work.as_ref()?.stream.as_ref());
        let stream = prior.map_or_else(|| AgentStream::start(&event.run), ProviderStream::output);
        if !stream.accepts(&event)
            || (prior.is_none() && existing.is_some_and(|row| !row.text.is_empty()))
            || matches!(&event.event, AgentOutput::TextDelta { text }
                if existing.map_or(0, |row| row.text.len()) + text.len() > MAX_MESSAGE_TEXT_BYTES)
        {
            return Err(Refusal::new(
                "out-of-order, terminal or oversized provider output",
            ));
        }
        if existing.is_none() {
            // An answer longer than one message goes on in its author's next message, which
            // stands where the full one does.
            let parent = self.thread_of_next(&id, &author(cx)?, &channel);
            self.append(
                cx,
                id.clone(),
                &channel,
                cx.now_ms(),
                Posting {
                    parent,
                    appearance: name.map(|name| tilo_actions::Appearance {
                        name: name.into_string(),
                        avatar: None,
                        persona: None,
                    }),
                    ..Posting::default()
                },
            )?;
        }
        let row = self
            .messages
            .get_mut(&id)
            .ok_or_else(|| Refusal::new("there is no such reply"))?;
        let work = row.work.get_or_insert_with(Work::default);
        if work.state.is_none() {
            work.state = Some(Transition {
                state: crate::model::AgentWorkState::Taken,
                since: cx.now_ms(),
            });
        }
        let record = work.stream.get_or_insert_with(|| ProviderStream {
            run: event.run.clone(),
            ..ProviderStream::default()
        });
        match &event.event {
            AgentOutput::TextDelta { text } => row.text.push_str(text),
            AgentOutput::ReasoningDelta { text } => record.reasoning.push_str(text),
            AgentOutput::Progress { label } => record.progress.clone_from(label),
            AgentOutput::Finished { outcome, detail } => {
                record.finish = Some(*outcome);
                record.detail.clone_from(detail);
                work.state = Some(Transition {
                    state: crate::model::AgentWorkState::ProviderFinished { outcome: *outcome },
                    since: cx.now_ms(),
                });
            }
        }
        record.sequence = event.sequence;
        Ok(())
    }
}

/// Refuse a post to `channel` unless the chat has it as a conversation that
/// takes messages: a text channel, or a direct conversation between members
/// the chat knows.
fn takes_posts(chat: &Chat, channel: &Id) -> Result<(), Refusal> {
    if chat.conversation(channel).is_none() {
        return Err(Refusal::new("there is no such channel"));
    }
    if !chat.takes_messages(channel) {
        return Err(Refusal::new(
            "a voice channel has no conversation to post in",
        ));
    }
    Ok(())
}

impl Chained for ChatMessages {
    type Base = Chat;

    fn chain() -> ui::resource::DocumentId {
        parts::chain()
    }

    /// The message a write goes with, which keeps it beside that one: a
    /// reply beside the message it answers, a delivery or an agent's ask made
    /// before where it was put (so making it again changes nothing), a
    /// streamed reply where it started, and any change of a message where
    /// the message is.
    fn beside(action: &Self::Action) -> Option<Id> {
        use chat_messages::{
            AnswerWithView, AskAgent, DeliverMessage, EditMessage, ImportMessage, PostView,
            RecordWork, RewriteView, StreamOutput,
        };
        match &action.operation {
            ChatMessagesAction::Post(chat_messages::Post {
                message:
                    NewMessage {
                        id: None,
                        parent: Some(parent),
                        ..
                    },
            })
            | ChatMessagesAction::Ask(chat_messages::Ask {
                parent: Some(parent),
                ..
            })
            | ChatMessagesAction::PostView(PostView {
                parent: Some(parent),
                ..
            }) => Some(parent.clone()),
            ChatMessagesAction::DeliverMessage(DeliverMessage {
                channel,
                author,
                body,
                ts: Some(ts),
            }) => Some(crate::transcript::delivered_id(channel, author, *ts, body)),
            ChatMessagesAction::ImportMessage(ImportMessage {
                channel,
                text,
                origin,
                ..
            }) => origin
                .created_ms
                .map(|created_ms| imported_id(channel, origin, created_ms, text)),
            ChatMessagesAction::AskAgent(AskAgent { message, .. }) => message.clone(),
            ChatMessagesAction::StreamOutput(StreamOutput { reply, .. }) => {
                Some(Id::from_raw(reply.clone()))
            }
            ChatMessagesAction::RecordWork(RecordWork { message, .. })
            | ChatMessagesAction::EditMessage(EditMessage { message, .. })
            | ChatMessagesAction::AnswerWithView(AnswerWithView { message, .. })
            | ChatMessagesAction::RewriteView(RewriteView { message, .. }) => Some(message.clone()),
            other => Self::item_of(other),
        }
    }

    fn prepare(
        actions: &mut [Self::Action],
        chat: &Chat,
        parts: &[(u64, Rc<Self>)],
        invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), Refusal> {
        for action in &mut *actions {
            match &mut action.operation {
                ChatMessagesAction::Post(post) => {
                    let message = &mut post.message;
                    takes_posts(chat, &message.channel)?;
                    let body = message.body.clone().unwrap_or_default();
                    message.mentions = Some(crate::chat::mentions(&message.text, &body));
                }
                ChatMessagesAction::PostFile(post) => {
                    takes_posts(chat, &post.channel)?;
                    post.mentions = Some(crate::chat::mentions(
                        post.text.as_deref().unwrap_or_default(),
                        &[],
                    ));
                }
                ChatMessagesAction::ShareResource(share) => {
                    takes_posts(chat, &share.channel)?;
                    let text = share
                        .text
                        .clone()
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or_else(|| share.attached.reference.label.clone());
                    share.mentions = Some(crate::chat::mentions(&text, &[]));
                }
                ChatMessagesAction::ShareWindow(share) => {
                    takes_posts(chat, &share.channel)?;
                    share.mentions = Some(crate::chat::mentions(
                        share.text.as_deref().unwrap_or_default(),
                        &[],
                    ));
                }
                ChatMessagesAction::Ask(ask) => takes_posts(chat, &ask.channel)?,
                ChatMessagesAction::PostView(post) => takes_posts(chat, &post.channel)?,
                ChatMessagesAction::AskAgent(ask) => {
                    takes_posts(chat, &ask.channel)?;
                    // Who the conversation is with, for the verb to check its
                    // asker and the person asked for against.
                    ask.members = Some(chat.conversation_members(&ask.channel));
                    // The message an ask under this key is carried by, where it
                    // was asked before: whoever it is asked for, and the key.
                    ask.message = crate::cause::led_to(&invoker.acting).map(|person| {
                        crate::cause::message_id(
                            &ui::tilo_resource::MemberId::person(person),
                            &ask.cause.key,
                        )
                    });
                }
                ChatMessagesAction::EditMessage(edit) => {
                    let body = edit.body.clone().unwrap_or_default();
                    edit.mentions = Some(crate::chat::mentions(&edit.text, &body));
                }
                ChatMessagesAction::StreamOutput(stream) => takes_posts(chat, &stream.channel)?,
                ChatMessagesAction::DeliverMessage(delivery) => {
                    takes_posts(chat, &delivery.channel)?;
                }
                ChatMessagesAction::ImportMessage(import) => {
                    takes_posts(chat, &import.channel)?;
                }
                _ => {}
            }
        }
        // What a verb writes into a conversation reads after every message the
        // invoking device had seen there, however far the two machines' clocks
        // are apart: the action's time is a hybrid logical clock of that
        // conversation, read here on the origin so every replica applies the
        // same value and none decides it from its own state.
        let mut written: Vec<(Id, i64)> = Vec::new();
        for action in actions.iter_mut() {
            let Some(channel) = channel_written(action, parts) else {
                continue;
            };
            let mut clock = tilo_resource::HybridClock::new();
            for (_, part) in parts {
                for (_, message) in part.messages.iter() {
                    if message.channel == channel {
                        clock.see(message.ts);
                    }
                }
            }
            // A group's members take up where the one before left off.
            if let Some((_, at)) = written.iter().find(|(seen, _)| *seen == channel) {
                clock.see(*at);
            }
            action.now_ms = clock.stamp(action.now_ms);
            if let Some((_, at)) = written.iter_mut().find(|(seen, _)| *seen == channel) {
                *at = action.now_ms;
            } else {
                written.push((channel, action.now_ms));
            }
        }
        Ok(())
    }

    /// The channel a post goes to, as everyone in the workspace sees it named
    /// (`#general`). A direct conversation is never named.
    fn carry(previous: u64, part: &Self) -> Option<tilo_resource::Invoke<Self>> {
        Some(Self::CARRY_COUNTS.with(chat_messages::CarryCounts {
            earlier: part.carried_after(previous),
        }))
    }

    fn place(action: &Self::Action, chat: &Chat) -> Option<String> {
        let channel = match &action.operation {
            ChatMessagesAction::Post(post) => &post.message.channel,
            ChatMessagesAction::PostFile(post) => &post.channel,
            ChatMessagesAction::ShareResource(share) => &share.channel,
            ChatMessagesAction::ShareWindow(share) => &share.channel,
            ChatMessagesAction::Ask(ask) => &ask.channel,
            ChatMessagesAction::PostView(post) => &post.channel,
            ChatMessagesAction::AskAgent(ask) => &ask.channel,
            _ => return None,
        };
        let conversation = chat.conversation(channel)?;
        let name = conversation.name.trim().trim_start_matches('#');
        (conversation.is_channel() && !name.is_empty()).then(|| format!("#{name}"))
    }
}

/// The conversation a verb writes its time into: the one it posts to, or the
/// one holding the message it changes.
fn channel_written(
    action: &ui::tilo_resource::Stamped<ChatMessagesAction>,
    parts: &[(u64, Rc<ChatMessages>)],
) -> Option<Id> {
    let channel = match &action.operation {
        ChatMessagesAction::Post(post) => &post.message.channel,
        ChatMessagesAction::PostFile(post) => &post.channel,
        ChatMessagesAction::ShareResource(share) => &share.channel,
        ChatMessagesAction::ShareWindow(share) => &share.channel,
        ChatMessagesAction::Ask(ask) => &ask.channel,
        ChatMessagesAction::PostView(post) => &post.channel,
        ChatMessagesAction::AskAgent(ask) => &ask.channel,
        ChatMessagesAction::DeliverMessage(delivery) => &delivery.channel,
        ChatMessagesAction::ImportMessage(import) => &import.channel,
        ChatMessagesAction::StreamOutput(stream) => &stream.channel,
        _ => {
            let message = ChatMessages::beside(action)
                .or_else(|| ChatMessages::item_of(&action.operation))?;
            return message_channel(&message, parts);
        }
    };
    Some(channel.clone())
}

/// The conversation the message `message` is in, over the parts held.
fn message_channel(message: &Id, parts: &[(u64, Rc<ChatMessages>)]) -> Option<Id> {
    parts
        .iter()
        .find_map(|(_, part)| part.messages.get(message).map(|row| row.channel.clone()))
}

impl ChatMessages {
    /// The message `reply` answers, when `reply` is what an agent writes in answer on the
    /// attempt that goes now ([`reply_id_at`]): the message its turn's stop stands on.
    pub(crate) fn answered_by(&self, reply: &Id) -> Option<Id> {
        let row = self.messages.get(reply)?;
        row.work.as_ref()?;
        let thread = row.parent.as_ref()?;
        self.messages
            .iter()
            .find(|&(id, asked)| {
                asked.channel == row.channel
                    && (id == thread || asked.parent.as_ref() == Some(thread))
                    && reply_id_at(id, &row.author, asked.attempts) == *reply
            })
            .map(|(id, _)| id.clone())
    }

    /// The message that asked, when `reply` is a message of the answer to it: the agent's
    /// reply ([`Self::answered_by`]), or a message that answer went on in, each of which
    /// follows the one before it as a reply follows what it answers.
    fn asked_with(&self, reply: &Id) -> Option<Id> {
        let mut asked = self.answered_by(reply)?;
        // Each step goes to another message of the part, so the part's size ends the walk.
        for _ in self.messages.iter() {
            let asks = self
                .messages
                .get(&asked)
                .is_some_and(|message| crate::summons::asks_for(message).is_some());
            match self.answered_by(&asked) {
                Some(above) if !asks => asked = above,
                _ => break,
            }
        }
        Some(asked)
    }

    /// The message `reply` stands under when it is the next message of an answer longer than
    /// one message holds: where the message before it stands. Its id is the one derived from
    /// that message's and their author's ([`reply_id_at`]).
    fn thread_of_next(
        &self,
        reply: &Id,
        author: &tilo_resource::MemberId,
        channel: &Id,
    ) -> Option<Id> {
        self.messages
            .iter()
            .find(|&(id, before)| {
                before.channel == *channel
                    && before.author == *author
                    && before.work.is_some()
                    && reply_id_at(id, author, 0) == *reply
            })
            .and_then(|(_, before)| before.parent.clone())
    }

    /// The message a verb on one message names.
    fn item_of(action: &ChatMessagesAction) -> Option<Id> {
        match action {
            ChatMessagesAction::DeleteMessage(chat_messages::DeleteMessage { message })
            | ChatMessagesAction::RemoveMessage(chat_messages::RemoveMessage { message })
            | ChatMessagesAction::Pin(chat_messages::Pin { message, .. })
            | ChatMessagesAction::StopSharing(chat_messages::StopSharing { message })
            | ChatMessagesAction::SetShareAccess(chat_messages::SetShareAccess {
                message, ..
            })
            | ChatMessagesAction::ShareOpen(chat_messages::ShareOpen { message, .. })
            | ChatMessagesAction::AnswerQuestion(chat_messages::AnswerQuestion {
                message, ..
            })
            | ChatMessagesAction::WithdrawQuestion(chat_messages::WithdrawQuestion {
                message,
                ..
            })
            | ChatMessagesAction::SettleQuestion(chat_messages::SettleQuestion {
                message, ..
            })
            | ChatMessagesAction::AnswerWithView(chat_messages::AnswerWithView {
                message, ..
            })
            | ChatMessagesAction::RewriteView(chat_messages::RewriteView { message, .. })
            | ChatMessagesAction::StopAgent(chat_messages::StopAgent { message }) => {
                Some(message.clone())
            }
            // A field edit of one message (a reaction) names it in its path.
            ChatMessagesAction::FieldEdit(edit) => match edit.path() {
                [Segment::Field(field), Segment::Item(message), ..] if field == "messages" => {
                    Some(message.clone())
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// The parts of the chat's messages a device holds, read as one timeline:
/// every message oldest first, whichever part holds it.
///
/// The order is when each message was written ([`Self::iter`]): the time its
/// writer's action carried in milliseconds, ties by id. Every device reads the
/// same time from the same message, so two devices that wrote offline and
/// took each other's posts in the opposite order read one conversation in one
/// order, and a message reaches a device that was away in its own place.
/// Nothing is ordered by the place the storage made for a message, which is
/// what the device writing it had seen.
#[derive(Clone, Debug, Default)]
pub struct Messages {
    parts: Vec<(u64, Rc<ChatMessages>)>,
    /// The messages sent from here that wait for the part they go to, on its
    /// way from the workspace ([`ui::framework::Chain::unwritten`]): shown as
    /// being sent, in no part yet.
    sending: Option<Rc<ChatMessages>>,
}

impl Messages {
    /// The parts held, oldest first.
    #[must_use]
    pub fn new(mut parts: Vec<(u64, Rc<ChatMessages>)>) -> Self {
        parts.sort_by_key(|(index, _)| *index);
        Self {
            parts,
            sending: None,
        }
    }

    /// These parts with the messages being sent, `sending`, after them.
    #[must_use]
    pub fn with_sending(mut self, sending: Option<Rc<ChatMessages>>) -> Self {
        self.sending = sending;
        self
    }

    /// The messages being sent: written once the part they go to is here.
    pub fn sending(&self) -> impl Iterator<Item = (&Id, &Message)> + '_ {
        self.sending
            .iter()
            .flat_map(|part| part.in_written_order())
            .filter(|(id, _)| self.part_of(id).is_none())
    }

    /// One part, numbered 0: for a reader of a single part's document.
    #[must_use]
    pub fn single(part: ChatMessages) -> Self {
        Self::new(vec![(0, Rc::new(part))])
    }

    /// The parts held, oldest first.
    #[must_use]
    pub fn parts(&self) -> &[(u64, Rc<ChatMessages>)] {
        &self.parts
    }

    /// The head held: the newest part.
    #[must_use]
    pub fn head(&self) -> Option<u64> {
        self.parts.last().map(|(index, _)| *index)
    }

    /// Every message held, oldest first: when each was written, ties by id.
    /// The time is the one the writer's action carried, so every device reads
    /// one order whatever order the changes arrived in, and a message written
    /// before another reads before it however late it reaches a device.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&Id, &Message)> + '_ {
        let mut held: Vec<(&Id, &Message)> = self
            .parts
            .iter()
            .flat_map(|(_, part)| part.in_written_order())
            .collect();
        held.sort_by(written_first);
        held.into_iter()
    }

    /// How many messages are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.iter().map(|(_, part)| part.messages.len()).sum()
    }

    /// Whether no message is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|(_, part)| part.messages.is_empty())
    }

    /// The message `id`, in whichever part holds it.
    #[must_use]
    pub fn get(&self, id: &Id) -> Option<&Message> {
        self.parts
            .iter()
            .rev()
            .find_map(|(_, part)| part.messages.get(id))
    }

    /// Whether a held part has the message `id`.
    #[must_use]
    pub fn contains(&self, id: &Id) -> bool {
        self.part_of(id).is_some()
    }

    /// The number of the part holding the message `id`.
    #[must_use]
    pub fn part_of(&self, id: &Id) -> Option<u64> {
        self.parts
            .iter()
            .rev()
            .find(|(_, part)| part.messages.contains(id))
            .map(|(index, _)| *index)
    }

    /// The top-level messages of `channel` held, in every part.
    #[must_use]
    pub fn top_level(&self, channel: &Id) -> usize {
        self.iter()
            .filter(|(_, message)| message.channel == *channel && message.parent.is_none())
            .count()
    }

    /// How far someone who saw every top-level message of `channel` held
    /// has read it: the newest part with one of them, and how many it has.
    #[must_use]
    pub fn read_to_end(&self, channel: &Id) -> parts::ReadMark {
        self.parts
            .iter()
            .rev()
            .find_map(|(index, part)| {
                let seen = part
                    .messages
                    .values()
                    .filter(|message| message.channel == *channel && message.parent.is_none())
                    .count();
                (seen > 0).then(|| parts::ReadMark {
                    part: i64::try_from(*index).unwrap_or(i64::MAX),
                    seen: i64::try_from(seen).unwrap_or(i64::MAX),
                })
            })
            .unwrap_or_default()
    }

    /// The first top-level message of `channel` held after `mark`: where a
    /// reader who read up to `mark` starts reading again. When `mark` lies
    /// in a part not held, the first one held after it.
    #[must_use]
    pub fn first_unread(&self, channel: &Id, mark: parts::ReadMark) -> Option<&Id> {
        self.parts.iter().find_map(|(index, part)| {
            let index = i64::try_from(*index).unwrap_or(i64::MAX);
            let skip = match index.cmp(&mark.part) {
                std::cmp::Ordering::Less => return None,
                std::cmp::Ordering::Equal => {
                    usize::try_from(mark.seen.max(0)).unwrap_or(usize::MAX)
                }
                std::cmp::Ordering::Greater => 0,
            };
            part.in_written_order()
                .into_iter()
                .filter(|(_, message)| message.channel == *channel && message.parent.is_none())
                .nth(skip)
                .map(|(id, _)| id)
        })
    }

    /// How many top-level messages `channel` has in the parts before the
    /// oldest held, as the head carries them ([`ChatMessages::earlier`]); `None`
    /// when the head does not say for every one of them (parts written
    /// before parts carried counts).
    #[must_use]
    pub fn earlier_than_held(&self, channel: &Id) -> Option<usize> {
        let (oldest, _) = self.parts.first()?;
        let (_, newest) = self.parts.last()?;
        (0..*oldest).try_fold(0_usize, |sum, index| {
            let counts = newest.earlier.get(&index.to_string())?;
            let count = counts.get(channel.as_str()).copied().unwrap_or_default();
            Some(sum.saturating_add(usize::try_from(count).unwrap_or_default()))
        })
    }

    /// The top-level messages of `channel` after `mark`: counted in each
    /// part held, and in the parts between that are not, as the head
    /// carries them ([`ChatMessages::earlier`]).
    #[must_use]
    pub fn unread(&self, channel: &Id, mark: parts::ReadMark) -> usize {
        let Some((head, newest)) = self.parts.last() else {
            return 0;
        };
        let from = u64::try_from(mark.part.max(0)).unwrap_or_default();
        let seen = usize::try_from(mark.seen.max(0)).unwrap_or(usize::MAX);
        let part_held = |index: u64| {
            self.parts
                .binary_search_by_key(&index, |(held, _)| *held)
                .ok()
                .and_then(|at| self.parts.get(at))
                .map(|(_, part)| part)
        };
        (from..=*head)
            .map(|index| {
                let count = match part_held(index) {
                    Some(part) => part
                        .messages
                        .values()
                        .filter(|message| message.channel == *channel && message.parent.is_none())
                        .count(),
                    None => newest
                        .earlier
                        .get(&index.to_string())
                        .and_then(|counts| counts.get(channel.as_str()))
                        .and_then(|count| usize::try_from(*count).ok())
                        .unwrap_or_default(),
                };
                if index == from {
                    count.saturating_sub(seen)
                } else {
                    count
                }
            })
            .sum()
    }

    /// [`Self::unread`], counted from `counts`, the top-level messages each
    /// channel has in each part held: what a reload read when it read that
    /// part, so a count costs the parts and not their messages.
    #[must_use]
    pub fn unread_counted(
        &self,
        channel: &Id,
        mark: parts::ReadMark,
        counts: &BTreeMap<u64, BTreeMap<String, usize>>,
    ) -> usize {
        let Some((head, newest)) = self.parts.last() else {
            return 0;
        };
        let from = u64::try_from(mark.part.max(0)).unwrap_or_default();
        let seen = usize::try_from(mark.seen.max(0)).unwrap_or(usize::MAX);
        (from..=*head)
            .map(|index| {
                let count = match counts.get(&index) {
                    Some(counted) => counted.get(channel.as_str()).copied().unwrap_or_default(),
                    None => newest
                        .earlier
                        .get(&index.to_string())
                        .and_then(|counts| counts.get(channel.as_str()))
                        .and_then(|count| usize::try_from(*count).ok())
                        .unwrap_or_default(),
                };
                if index == from {
                    count.saturating_sub(seen)
                } else {
                    count
                }
            })
            .sum()
    }

    /// The top-level messages of `channel` after `mark` that `author` did not
    /// write: what a direct conversation counts as unread for them. Counted in
    /// the parts this reader holds, as a direct conversation is short and its
    /// reader holds it, in the same order as [`Self::first_unread`]: the
    /// order they were written in.
    #[must_use]
    pub fn unread_by_others(
        &self,
        channel: &Id,
        mark: parts::ReadMark,
        author: &tilo_resource::MemberId,
    ) -> usize {
        let from = u64::try_from(mark.part.max(0)).unwrap_or_default();
        let seen = usize::try_from(mark.seen.max(0)).unwrap_or(usize::MAX);
        let mut unread = 0;
        for (index, part) in &self.parts {
            if *index < from {
                continue;
            }
            let skip = if *index == from { seen } else { 0 };
            unread += part
                .in_written_order()
                .into_iter()
                .filter(|(_, message)| message.channel == *channel && message.parent.is_none())
                .skip(skip)
                .filter(|(_, message)| message.author != *author)
                .count();
        }
        unread
    }
}

/// The parts `doc` holds when it is a part's resource, read as one: for a
/// reader of one part document.
#[must_use]
pub fn read_part(doc: &tilo_actions::Doc) -> ChatMessages {
    <ChatMessages as tilo_resource::Resource>::read(doc).unwrap_or_default()
}

#[cfg(test)]
mod tests;

/// Whether `id`, named by whoever posts a message, can be a message's id: what a made one is
/// (letters and digits), at most 64 of them, dashes allowed for a UUID's form.
fn valid_message_id(id: &Id) -> bool {
    let id = id.as_str();
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}
