//! Private, transient history. Late worker answers are revalidated on the UI
//! thread; no message plaintext is written to the workspace resource.

use std::rc::Rc;

use tilo_contact_directory::routing;
use tilo_messaging::{Delivery, Direction, HistoryClient, HistoryPage};
use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, EffectStatus, Effected};
use ui::widget::{Button, Card, Column, Element, ModelView, Text};

use crate::view::{Choice, Selection, Sources, choices};
use crate::{CATALOG, Messages, MessagesEffect};

pub(crate) struct CaptureHistory(pub HistoryPage);

#[derive(Clone, Default)]
struct Projection {
    generation: u64,
    route: Option<routing::Route>,
    page: Option<Rc<HistoryPage>>,
    loading: bool,
}

#[derive(Clone)]
pub(crate) struct HistoryView {
    client: Option<HistoryClient>,
    state: LocalState<Projection>,
    changes: LocalState<u64>,
    _subscription: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

impl HistoryView {
    pub(crate) fn new(client: Option<HistoryClient>, principal: Option<&str>) -> Self {
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
        });
    }

    pub(crate) fn publish(&self, page: HistoryPage) {
        let generation = self.state.get().generation.saturating_add(1);
        self.state.set(Projection {
            generation,
            route: Some(page.route.clone()),
            page: Some(Rc::new(page)),
            loading: false,
        });
    }

    pub(crate) fn available(&self) -> bool {
        self.client.is_some()
    }

    pub(crate) fn clear(&self) {
        let generation = self.state.get().generation.saturating_add(1);
        self.state.set(Projection {
            generation,
            ..Projection::default()
        });
    }

    pub(crate) fn status(&self) -> Vec<EffectStatus> {
        if self.client.is_some() {
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
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-history-unavailable"))?;
        let selected = selection.get();
        let requested: Choice =
            serde_json::from_str(choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
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
        });
        let view = self.clone();
        let sources = sources.clone();
        let selection = selection.clone();
        Ok(cx.spawn(
            move |_| async move { client.read(route).await },
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

    pub(crate) fn widget(
        &self,
        sources: &Sources,
        actions: &Actions<Messages>,
        selection: &Selection,
        choice: Option<&Choice>,
        hint: &str,
    ) -> Element {
        let sources = sources.clone();
        let selected = selection.clone();
        let choice = choice.cloned();
        let hint = hint.to_owned();
        let controls = actions.clone();
        let available = self.available();
        Box::new(ModelView::from_state_by(
            &self.state,
            |state| (state.generation, state.loading, state.page.is_some()),
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
                    body = history_rows(body, page);
                } else {
                    body = body.push(super::view::empty(&hint, "messages-transport-unavailable"));
                }
                if available && route.is_some() {
                    body = body.push(Box::new(controls.bind_effect_invocation(
                        Button::new("messages.refresh-history", "messages-refresh-history"),
                        &MessagesEffect::ReadHistory {
                            choice: selected.choice.clone(),
                        },
                    )));
                }
                Box::new(body)
            },
        ))
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

fn history_rows(mut body: Column, page: &HistoryPage) -> Column {
    if page.messages.is_empty() {
        return body.push(Box::new(Text::new("messages-no-history")));
    }
    for message in &page.messages {
        let text = message.text.clone();
        let direction = match message.direction {
            Direction::Incoming => "messages-incoming",
            Direction::Outgoing => "messages-outgoing",
        };
        let mut content = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                Text::new(direction)
                    .with_role(ui::tokens::TextRole::Caption)
                    .dimmed(),
            ))
            .push(Box::new(
                Text::dynamic(move || text.to_string()).with_role(ui::tokens::TextRole::Body),
            ));
        if let Some(delivery) = message.delivery {
            let key = match delivery {
                Delivery::Submitted => "messages-submitted",
                Delivery::ServerAcknowledged => "messages-server-acknowledged",
                Delivery::RecipientDelivered => "messages-delivered",
                Delivery::RecipientRead => "messages-read",
                Delivery::Uncertain => "messages-uncertain",
            };
            content = content.push(Box::new(
                Text::new(key)
                    .with_role(ui::tokens::TextRole::Caption)
                    .dimmed(),
            ));
        }
        body = body.push(Box::new(Card::new(Box::new(content))));
    }
    if page.more {
        body = body.push(Box::new(Text::new("messages-more-history")));
    }
    body
}
