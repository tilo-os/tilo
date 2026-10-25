//! Sending uses the host-lent service and the same exact route as history.
use crate::{
    CATALOG, Messages, MessagesEffect,
    view::{Choice, Selection, Sources},
};
use tilo_messaging::{MessagingClient, SendRequest};
use ui::framework::PersonRecord;
use ui::{
    LocalState,
    framework::{EffectAnswer, EffectCx, EffectStatus, Effected},
};
#[derive(Clone)]
pub(crate) struct Sending {
    client: Option<MessagingClient>,
    pub(crate) draft: LocalState<String>,
    pub(crate) operation: LocalState<String>,
    generation: LocalState<u64>,
    /// The person's own record of unsent words, and the conversation the live
    /// text belongs to.
    drafts: Option<PersonRecord<crate::drafts::MessageDrafts>>,
    open: LocalState<String>,
}
impl Sending {
    pub(crate) fn new(
        client: Option<MessagingClient>,
        principal: Option<&str>,
        drafts: Option<PersonRecord<crate::drafts::MessageDrafts>>,
    ) -> Self {
        Self {
            client: client.filter(|c| principal == Some(c.principal())),
            draft: LocalState::new(String::new()),
            operation: LocalState::new(uuid::Uuid::new_v4().to_string()),
            generation: LocalState::new(0),
            drafts,
            open: LocalState::new(String::new()),
        }
    }

    /// Seed the composer with the words the person left in `choice`'s
    /// conversation, when it is not the one already open.
    ///
    /// A draft follows the conversation, not the window: switching to the same
    /// person on another service starts an empty composer, and switching back
    /// finds what was typed there.
    pub(crate) fn open_draft(&self, choice: &Choice) {
        let key = crate::drafts::key(choice);
        if self.open.get() == key {
            return;
        }
        let stored = self
            .drafts
            .as_ref()
            .and_then(|record| record.get().drafts.get(&key).cloned())
            .unwrap_or_default();
        self.open.set(key);
        self.draft.set(stored);
    }

    /// Keep what the person has typed, so closing the window does not lose it.
    pub(crate) fn keep_draft(&self, body: &str) {
        let Some(record) = &self.drafts else {
            return;
        };
        let key = self.open.get();
        if key.is_empty() {
            return;
        }
        let stored = record.get().drafts.get(&key).cloned().unwrap_or_default();
        if stored == body {
            return;
        }
        let _ = record.apply(&ui::tilo_resource::Stamped {
            operation: crate::drafts::MessageDraftsAction::KeepDraft(
                crate::drafts::drafts::KeepDraft {
                    conversation: key,
                    body: body.to_owned(),
                },
            ),
            now_ms: ui::tilo_resource::origin_now_ms(),
        });
    }

    /// Drop the words typed for the conversation that is no longer there: a
    /// route the directory forgot or re-reviewed takes its draft with it.
    pub(crate) fn forget_draft(&self) {
        let Some(record) = &self.drafts else {
            return;
        };
        let key = self.open.get();
        if key.is_empty() || !record.get().drafts.contains_key(&key) {
            return;
        }
        let _ = record.apply(&ui::tilo_resource::Stamped {
            operation: crate::drafts::MessageDraftsAction::DropDraft(
                crate::drafts::drafts::DropDraft { conversation: key },
            ),
            now_ms: ui::tilo_resource::origin_now_ms(),
        });
    }

    /// The draft `key`'s conversation holds, once it has been sent: removed
    /// only while it is still exactly what was sent, so a word typed while the
    /// send was in flight is the next message rather than a lost one.
    fn sent_draft(&self, key: &str, sent: &str) {
        let Some(record) = &self.drafts else {
            return;
        };
        if record
            .get()
            .drafts
            .get(key)
            .is_some_and(|held| held == sent)
        {
            let _ = record.apply(&ui::tilo_resource::Stamped {
                operation: crate::drafts::MessageDraftsAction::DropDraft(
                    crate::drafts::drafts::DropDraft {
                        conversation: key.to_owned(),
                    },
                ),
                now_ms: ui::tilo_resource::origin_now_ms(),
            });
        }
    }

    /// Run the conversation's send, from whatever asked for it: the Send
    /// button's effect, or Enter in the composer.
    pub(crate) fn submit(
        &self,
        sources: &Sources,
        selection: &LocalState<Selection>,
        history: &crate::history::HistoryView,
        choice: &str,
        text: &str,
        jobs: &ui::framework::Jobs<Messages>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let effect = MessagesEffect::Send {
            choice: choice.to_owned(),
            operation: self.operation.get(),
            text: text.to_owned(),
        };
        let sending = self.clone();
        let sources = sources.clone();
        let selection = selection.clone();
        let history = history.clone();
        jobs.start::<()>("messages.send", move |cx| {
            sending.send(&sources, &selection, &history, effect, cx)
        });
    }
    pub(crate) fn client(&self) -> Option<MessagingClient> {
        self.client.clone()
    }
    pub(crate) fn clear(&self) {
        self.generation.set(self.generation.get().saturating_add(1));
        self.draft.set(String::new());
        self.open.set(String::new());
        self.operation.set(uuid::Uuid::new_v4().to_string());
    }
    pub(crate) fn available(&self) -> bool {
        self.client.is_some()
    }
    pub(crate) fn supports(&self, route: &tilo_contact_directory::routing::Route) -> bool {
        self.client
            .as_ref()
            .is_some_and(|client| client.supports(route))
    }
    pub(crate) fn account_state(&self, choice: &Choice) -> tilo_messaging::AccountState {
        self.client
            .as_ref()
            .map_or(tilo_messaging::AccountState::Unavailable, |client| {
                client.account_state(&tilo_messaging::ServiceAccount::new(
                    &choice.service,
                    &choice.account,
                ))
            })
    }
    pub(crate) fn history(&self) -> Option<tilo_messaging::HistoryClient> {
        self.client.as_ref().map(MessagingClient::history)
    }
    pub(crate) fn status(&self) -> Vec<EffectStatus> {
        if self.available() {
            Vec::new()
        } else {
            vec![EffectStatus::unavailable(
                MessagesEffect::SEND,
                "connection-unavailable",
                CATALOG.t("messages-transport-unavailable"),
            )]
        }
    }
    pub(crate) fn send(
        &self,
        sources: &Sources,
        selection: &LocalState<Selection>,
        history: &crate::history::HistoryView,
        request: MessagesEffect,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        let MessagesEffect::Send {
            choice,
            operation,
            text,
        } = request
        else {
            return Err(CATALOG.t("messages-transport-unavailable"));
        };
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-transport-unavailable"))?;
        let choice: Choice =
            serde_json::from_str(&choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
        let route = crate::history::current_route(sources, &selection.get(), &choice)?;
        let sent_route = route.clone();
        let sent_operation = operation.clone();
        let sent_text = text.clone();
        let sources = sources.clone();
        let selection = selection.clone();
        let history = history.clone();
        let draft = self.draft.clone();
        let draft_operation = self.operation.clone();
        let sent_key = self.open.get();
        let sent_drafts = self.clone();
        let generation = self.generation.clone();
        let admitted = generation.get();
        let request = SendRequest {
            route,
            operation,
            text: text.into(),
        };
        Ok(cx.spawn(move |_|async move {
            let receipt=client.send(request).await?;
            let page=client.history().read(receipt.route.clone()).await.ok();
            Ok::<_,tilo_messaging::SendError>((receipt,page))
        },move |(),result,_|{
            match result {
                Ok((receipt,page))=>{
                    if generation.get() == admitted && crate::history::current_route(&sources,&selection.get(),&choice).as_ref()==Ok(&sent_route) {
                        if let Some(page)=page {history.publish(page);}
                        if draft_operation.get()==sent_operation && draft.get()==sent_text {
                            draft.set(String::new());draft_operation.set(uuid::Uuid::new_v4().to_string());
                            sent_drafts.sent_draft(&sent_key, &sent_text);
                        }
                    }
                    Ok(Effected::value(serde_json::json!({"operation":receipt.operation,"status":match receipt.delivery {
                    tilo_messaging::Delivery::Submitted=>"submitted",tilo_messaging::Delivery::ServerAcknowledged=>"server-acknowledged",_=>"uncertain",
                }})))
                },
                Err(error)=>{
                    if generation.get() == admitted && matches!(error,tilo_messaging::SendError::Revoked|tilo_messaging::SendError::Unavailable){history.clear();}
                    let key=match error {
                        tilo_messaging::SendError::Revoked=>"messages-contact-missing",
                        tilo_messaging::SendError::Unavailable=>"messages-transport-unavailable",
                        tilo_messaging::SendError::InvalidRequest=>"messages-send-invalid",
                        tilo_messaging::SendError::AlreadyReserved=>"messages-send-reserved",
                        tilo_messaging::SendError::Uncertain=>"messages-send-uncertain",
                    };Err(CATALOG.t(key))
                },
            }
        }))
    }
}
