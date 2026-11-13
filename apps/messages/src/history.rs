//! Private, transient history. Late worker answers are revalidated on the UI
//! thread; no message plaintext is written to the workspace resource.

use std::rc::Rc;

use tilo_contact_directory::routing;
use tilo_messaging::{Delivery, Direction, HistoryClient, HistoryCursor, HistoryPage};
use ui::LocalState;
use ui::framework::{ActionControl, Actions, EffectAnswer, EffectCx, EffectStatus, Effected};
use ui::i18n::ArgValue;
use ui::widget::{
    Button, Column, ConversationLine, ConversationList, DeliveryState, DeliveryStep, Element,
    ModelView, Text,
};

use crate::view::{Choice, Selection, Sources, choices};
use crate::{CATALOG, Messages, MessagesEffect};

pub(crate) struct CaptureHistory(pub HistoryPage);

#[derive(Clone, Default)]
struct Projection {
    generation: u64,
    route: Option<routing::Route>,
    page: Option<Rc<HistoryPage>>,
    /// The newest page is on its way.
    loading: bool,
    /// The page before the conversation's oldest message is on its way.
    loading_older: bool,
    /// Pages read into this conversation: the newest one, and each older
    /// page the person asked for. One page is all there was to begin with,
    /// so the conversation's start is only worth saying once this is more.
    pages: usize,
}

#[derive(Clone)]
pub(crate) struct HistoryView {
    client: Option<HistoryClient>,
    /// The conversations with the workspace's people are Chat's: they are
    /// read from Chat's messages.
    direct: bool,
    /// The person using the app, one side of each of those conversations.
    me: Option<String>,
    state: LocalState<Projection>,
    changes: LocalState<u64>,
    _subscription: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

impl HistoryView {
    pub(crate) fn new(
        client: Option<HistoryClient>,
        principal: Option<&str>,
        direct: bool,
    ) -> Self {
        let client = client.filter(|client| {
            principal.is_some_and(|person| !person.is_empty() && person == client.principal())
        });
        let changes = LocalState::new(0u64);
        let wake = changes.clone();
        let subscription = client
            .as_ref()
            .and_then(|client| {
                client.observe(std::sync::Arc::new(move || {
                    wake.set(wake.get().saturating_add(1));
                }))
            })
            .map(std::sync::Arc::from);
        Self {
            client,
            direct,
            me: principal.map(str::to_owned),
            state: LocalState::new(Projection::default()),
            changes,
            _subscription: subscription,
        }
    }
    pub(crate) fn revision(&self) -> u64 {
        self.changes.get()
    }

    pub(crate) fn stage_capture(&self, page: HistoryPage) {
        self.state.set(Projection {
            generation: 1,
            route: Some(page.route.clone()),
            page: Some(Rc::new(page)),
            loading: false,
            loading_older: false,
            pages: 1,
        });
    }

    pub(crate) fn publish(&self, page: HistoryPage) {
        let generation = self.state.get().generation.saturating_add(1);
        self.state.set(Projection {
            generation,
            route: Some(page.route.clone()),
            page: Some(Rc::new(page)),
            loading: false,
            loading_older: false,
            pages: 1,
        });
    }

    pub(crate) fn available(&self) -> bool {
        self.client.is_some() || self.direct
    }

    pub(crate) fn clear(&self) {
        let generation = self.state.get().generation.saturating_add(1);
        self.state.set(Projection {
            generation,
            ..Projection::default()
        });
    }

    pub(crate) fn status(&self) -> Vec<EffectStatus> {
        if self.available() {
            return Vec::new();
        }
        vec![EffectStatus::unavailable(
            MessagesEffect::READ_HISTORY,
            "connection-unavailable",
            CATALOG.t("messages-history-unavailable"),
        )]
    }

    pub(crate) fn read(
        &self,
        sources: &Sources,
        selection: &LocalState<Selection>,
        choice: &str,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let requested: Choice =
            serde_json::from_str(choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
        if requested.service == "tilo" {
            return read_direct(sources, selection, &requested, self, cx);
        }
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        let selected = selection.get();
        let route = current_route(sources, &selected, &requested)?;
        let generation = self
            .state
            .get()
            .generation
            .checked_add(1)
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        self.state.set(Projection {
            generation,
            route: Some(route.clone()),
            loading: true,
            page: None,
            ..Projection::default()
        });
        let view = self.clone();
        let sources = sources.clone();
        let selection = selection.clone();
        Ok(cx.spawn(
            move |_| async move { client.read(route, None).await },
            move |(), result, _| {
                // Cancellation, another request, route switches and revocation
                // all invalidate a response before any plaintext is projected.
                if view.state.get().generation != generation {
                    return Ok(Effected::done());
                }
                let route = current_route(&sources, &selection.get(), &requested);
                let Ok(route) = route else {
                    view.clear();
                    return Ok(Effected::done());
                };
                match result {
                    Ok(page) if page.route == route => view.state.set(Projection {
                        generation,
                        route: Some(route),
                        loading: false,
                        loading_older: false,
                        pages: 1,
                        page: Some(Rc::new(page)),
                    }),
                    _ => {
                        view.clear();
                        return Err(CATALOG.t("messages-history-unavailable"));
                    }
                }
                Ok(Effected::done())
            },
        ))
    }

    /// The page before the conversation's oldest message: read where that
    /// message is, and keep the messages already shown beneath it.
    ///
    /// The oldest message shown is the cursor, so a page the host answered
    /// twice cannot be read into the conversation twice, and an answer whose
    /// route or generation moved on is dropped rather than grafted on.
    pub(crate) fn read_older(
        &self,
        sources: &Sources,
        selection: &LocalState<Selection>,
        choice: &str,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        let selected = selection.get();
        let requested: Choice =
            serde_json::from_str(choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
        if requested.service == "tilo" {
            // A record holds the whole conversation it always held.
            return Ok(Effected::done());
        }
        let route = current_route(sources, &selected, &requested)?;
        let projection = self.state.get();
        if projection.loading
            || projection.loading_older
            || projection.route.as_ref() != Some(&route)
        {
            return Ok(Effected::done());
        }
        let page = projection
            .page
            .clone()
            .filter(|page| page.more)
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        let Some(cursor) = oldest(&page) else {
            return Ok(Effected::done());
        };
        let generation = projection.generation;
        self.state.set(Projection {
            loading_older: true,
            page: Some(page),
            ..projection
        });
        let asked = route.clone();
        let view = self.clone();
        let sources = sources.clone();
        let selection = selection.clone();
        Ok(cx.spawn(
            move |_| async move { client.read(asked, Some(cursor)).await },
            move |(), result, _| {
                let shown = view.state.get();
                // Another request, a route switch or revocation outranks this
                // answer: an older page belongs to the conversation it was
                // asked of, not to whichever one is open now.
                if shown.generation != generation
                    || current_route(&sources, &selection.get(), &requested).as_ref() != Ok(&route)
                {
                    return Ok(Effected::done());
                }
                let Some(page) = shown.page.clone() else {
                    return Ok(Effected::done());
                };
                match result {
                    Ok(older) if older.route == route => view.state.set(Projection {
                        loading_older: false,
                        pages: shown.pages.saturating_add(1),
                        page: Some(Rc::new(merge(&older, &page))),
                        ..shown
                    }),
                    Ok(_) | Err(_) => {
                        view.state.set(Projection {
                            loading_older: false,
                            ..shown
                        });
                        return Err(CATALOG.t("messages-history-unavailable"));
                    }
                }
                Ok(Effected::done())
            },
        ))
    }

    /// The window bar's Refresh messages: reading the selected route's
    /// history again, while it can be read.
    pub(crate) fn refresh(
        &self,
        sources: &Sources,
        selection: &Selection,
    ) -> Option<MessagesEffect> {
        let choice: Choice = serde_json::from_str(&selection.choice).ok()?;
        (self.available() && current_route(sources, selection, &choice).is_ok()).then(|| {
            MessagesEffect::ReadHistory {
                choice: selection.choice.clone(),
            }
        })
    }

    pub(crate) fn widget(
        &self,
        sources: &Sources,
        selection: &Selection,
        choice: Option<&Choice>,
        hint: &str,
        author: &str,
        actions: &Actions<Messages>,
    ) -> Element {
        let sources = sources.clone();
        let selected = selection.clone();
        let choice = choice.cloned();
        let hint = hint.to_owned();
        let author = author.to_owned();
        // The way to the page before: the app's own request, bound where the
        // page still has one.
        let older = choice.as_ref().and_then(|choice| {
            actions.effect(&MessagesEffect::ReadOlderHistory {
                choice: choice.value(),
            })
        });
        Box::new(ModelView::from_state_by(
            &self.state,
            |state| {
                (
                    state.generation,
                    state.loading,
                    state.page.is_some(),
                    state.loading_older,
                    state.page.as_ref().is_some_and(|page| page.more),
                    state.pages,
                )
            },
            move |state| {
                let route = choice
                    .as_ref()
                    .and_then(|choice| current_route(&sources, &selected, choice).ok());
                let mut body = Column::new().with_no_padding();
                if state.loading && route == state.route {
                    body = body.push(Box::new(Text::new("messages-history-loading")));
                } else if let Some(page) = state
                    .page
                    .as_ref()
                    .filter(|page| Some(&page.route) == route.as_ref())
                {
                    // What came before what shows: a button while there is a
                    // page to read, and the conversation's start once the
                    // person has gone back to it. Older SMS that merely stops
                    // reads as a conversation that ended, which it did not.
                    if page.more {
                        body = body.push(older_control(older.clone(), state.loading_older));
                    } else if state.pages > 1 {
                        body = body.push(Box::new(Text::new("messages-history-start")));
                    }
                    body = body.push(history_lines(page, &author));
                } else {
                    body = body.push(super::view::empty(&hint, "messages-transport-unavailable"));
                }
                Box::new(body)
            },
        ))
    }
}

/// The conversation with one of the workspace's people, read from Chat's
/// messages, so this window and Chat show the same messages and neither
/// keeps a copy.
fn read_direct(
    sources: &Sources,
    selection: &LocalState<Selection>,
    requested: &Choice,
    view: &HistoryView,
    cx: &mut EffectCx<Messages>,
) -> EffectAnswer {
    if !sources.tilo {
        return Err(CATALOG.t("messages-history-unavailable"));
    }
    let selected = selection.get();
    let route = current_route(sources, &selected, requested)?;
    let opened = view.clone();
    view.read_conversation(route, &selected.person, cx, move |page, _| {
        opened.publish(page);
        Ok(Effected::done())
    })
}

/// The app whose conversations the workspace's people have between them.
const CHAT: &str = "so.tilo.chat";
/// The chat record of the workspace: what Chat publishes its operations on.
const CHAT_RECORD: &str = "chat.workspace";
/// The chain of parts Chat keeps the workspace's messages in.
const CHAT_MESSAGES: &str = "derived:messages@derived:default@tab:0000000000000000";
/// Chat's operation that sends to the conversation with a person.
pub(crate) const SEND_DIRECT_MESSAGE: &str = "send-direct-message";

/// Chat's operation `operation` on the conversation with `person`, ready to
/// be called.
pub(crate) fn tilo_call(operation: &str, person: &str) -> ui::framework::Call {
    let record = ui::tilo_reference::ResourceRef::whole(CHAT, CHAT_RECORD, "Chat");
    ui::framework::Call::on(CHAT, None, &record, operation)
        .with(serde_json::json!({ "public_key": person }))
}

impl HistoryView {
    /// Read the conversation with `peer` from Chat's messages, then `then`
    /// with it as a page.
    pub(crate) fn read_conversation(
        &self,
        route: routing::Route,
        peer: &str,
        cx: &mut EffectCx<Messages>,
        then: impl FnOnce(HistoryPage, &mut EffectCx<Messages>) -> EffectAnswer + 'static,
    ) -> EffectAnswer {
        let me = self
            .me
            .clone()
            .filter(|me| !me.is_empty() && me != peer)
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        let peer = peer.to_owned();
        let read = ui::tilo_actions::remote::RecordReadRequest {
            app: CHAT.into(),
            record: CHAT_MESSAGES.into(),
            joined: true,
            after: None,
            wait_ms: None,
        };
        Ok(cx.read_record(read, move |(), readings, cx| {
            then(direct_page(route, (&me, &peer), &readings?), cx)
        }))
    }
}

/// The conversation between `me` and `peer` in `readings`, Chat's messages
/// as the record door answers them: each part's messages in it, by the order
/// key each keeps, as the list draws them.
pub(crate) fn direct_page(
    route: routing::Route,
    (me, peer): (&str, &str),
    readings: &serde_json::Value,
) -> HistoryPage {
    let mut sides = [format!("person:{me}"), format!("person:{peer}")];
    sides.sort();
    let channel = format!("direct:{}", sides.join("|"));
    let field = |value: &serde_json::Value, at: &str| value.pointer(at).cloned();
    let mut messages = Vec::new();
    let parts = readings
        .get("records")
        .and_then(serde_json::Value::as_array);
    for part in parts.into_iter().flatten() {
        let held = part
            .pointer("/value/messages")
            .and_then(serde_json::Value::as_object);
        let mut held: Vec<(&String, &serde_json::Value)> = held
            .into_iter()
            .flatten()
            .filter(|(_, entry)| field(entry, "/v/channel").is_some_and(|at| at == *channel))
            .collect();
        held.sort_by_key(|(id, entry)| (field(entry, "/k").map(|k| k.to_string()), *id));
        messages.extend(held.into_iter().map(|(id, entry)| {
            let text = field(entry, "/v/text").and_then(|text| text.as_str().map(str::to_owned));
            let ts = field(entry, "/v/ts").and_then(|ts| ts.as_u64());
            tilo_messaging::Message {
                id: id.clone(),
                text: text.unwrap_or_default().into(),
                direction: if field(entry, "/v/author").is_some_and(|author| author == peer) {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                },
                // The record keeps seconds.
                timestamp_ms: ts.unwrap_or_default().saturating_mul(1000),
                // Nothing stands between the two of them and the conversation.
                delivery: None,
            }
        }));
    }
    HistoryPage {
        route,
        messages,
        more: false,
    }
}

/// Older messages lead the conversation, so the way back to them stands
/// above it: the button that asks for the page before, or, while that page is
/// on its way, the words saying so.
fn older_control(binding: Option<ui::tilo_actions::ActionBinding<()>>, loading: bool) -> Element {
    if loading {
        return Box::new(Text::new("messages-loading-older"));
    }
    match binding {
        Some(binding) => {
            Box::new(Button::new("messages.load-older", "messages-load-older").bind_action(binding))
        }
        None => Box::new(Text::new("messages-history-unavailable")),
    }
}

/// The oldest message a page carries, whichever end of it the host wrote
/// first: where the page before it begins.
fn oldest(page: &HistoryPage) -> Option<HistoryCursor> {
    page.messages
        .iter()
        .min_by_key(|message| message.timestamp_ms)
        .map(HistoryCursor::before)
}

/// `older` beneath `shown`: the page before it first, then what showed, and
/// the older page's answer to whether anything precedes that.
fn merge(older: &HistoryPage, shown: &HistoryPage) -> HistoryPage {
    let mut messages = older.messages.clone();
    messages.extend(shown.messages.iter().cloned());
    messages.dedup_by(|a, b| a.id == b.id);
    HistoryPage {
        route: shown.route.clone(),
        messages,
        more: older.more,
    }
}

pub(crate) fn current_route(
    sources: &Sources,
    selection: &Selection,
    requested: &Choice,
) -> Result<routing::Route, String> {
    let selected: Choice = serde_json::from_str(&selection.choice)
        .map_err(|_| CATALOG.t("messages-contact-missing"))?;
    if &selected != requested
        || requested.recipient.is_empty()
        || !choices(sources, &selection.person).contains(requested)
    {
        return Err(CATALOG.t("messages-contact-missing"));
    }
    Ok(routing::Route {
        person: ui::prelude::Id::from_raw(&selection.person),
        association: ui::prelude::Id::from_raw(&requested.association),
        sender: ui::prelude::Id::from_raw(&requested.account),
        service: requested.service.clone(),
        scope: requested.scope.clone(),
        recipient: requested.recipient.clone(),
        revision: requested.revision.clone(),
    })
}

/// The conversation, as the shared message list draws it: a divider wherever
/// the day changes, the time on every line, and the transport's delivery state
/// in the one vocabulary both messaging apps speak.
///
/// The list is the toolkit's, not this app's: what Messages decides here is
/// which side a line is on and what the transport said, never where a day
/// starts or how a time reads.
fn history_lines(page: &HistoryPage, author: &str) -> Element {
    let lines: Vec<ConversationLine> = page
        .messages
        .iter()
        .map(|message| {
            let at_ms = i64::try_from(message.timestamp_ms).unwrap_or(i64::MAX);
            let body = message.text.to_string();
            let line = match message.direction {
                Direction::Incoming => {
                    ConversationLine::incoming(message.id.clone(), at_ms, author, body)
                }
                Direction::Outgoing => ConversationLine::outgoing(message.id.clone(), at_ms, body),
            };
            match message.delivery {
                Some(delivery) => line.with_delivery(delivery_state(delivery)),
                None => line,
            }
        })
        .collect();
    let name = CATALOG.t_args(
        "messages-conversation-with",
        &[("person", ArgValue::Text(author.into()))],
    );
    Box::new(ConversationList::new(lines).with_accessible_name(name))
}

/// What the transport reported, in the shared vocabulary: the words, the glyph
/// and the tone are the same ones Chat shows.
fn delivery_state(delivery: Delivery) -> DeliveryState {
    DeliveryState::of_step(match delivery {
        Delivery::Submitted => DeliveryStep::Submitted,
        Delivery::ServerAcknowledged => DeliveryStep::Acknowledged,
        Delivery::RecipientDelivered => DeliveryStep::Delivered,
        Delivery::RecipientRead => DeliveryStep::Read,
        Delivery::Uncertain => DeliveryStep::Uncertain,
    })
}

/// What one message's delivery state says: the shared vocabulary's key, the
/// same one Chat renders.
#[cfg(test)]
pub(crate) fn delivery_key(delivery: Delivery) -> &'static str {
    delivery_state(delivery).key()
}
