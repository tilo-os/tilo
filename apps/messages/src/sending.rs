//! Sending uses the host-lent service and the same exact route as history.
use crate::drafts::{DraftRecord, MessageDrafts};
use crate::{
    CATALOG, Messages, MessagesEffect,
    view::{Choice, Selection, Sources},
};
use tilo_messaging::{MessagingClient, SendRequest};
use ui::{framework::EffectAnswer, framework::EffectCx, framework::EffectStatus, framework::Effected};
use tilo_ui_base::LocalState;
#[derive(Clone)]
pub(crate) struct Sending {
    client: Option<MessagingClient>,
    direct: bool,
    pub(crate) draft: LocalState<String>,
    pub(crate) operation: LocalState<String>,
    generation: LocalState<u64>,
    drafts: Option<DraftRecord>,
    open: LocalState<String>,
    last_stored: LocalState<Option<String>>,
}
impl Sending {
    pub(crate) fn new(
        client: Option<MessagingClient>,
        actor: Option<&ui::tilo_resource::PersonId>,
        drafts: Option<DraftRecord>,
        direct: bool,
    ) -> Self {
        Self {
            client: client.filter(|c| actor.is_some_and(|actor| actor.as_str() == c.actor())),
            direct,
            draft: LocalState::new(String::new()),
            operation: LocalState::new(uuid::Uuid::new_v4().to_string()),
            generation: LocalState::new(0),
            drafts,
            open: LocalState::new(String::new()),
            last_stored: LocalState::new(None),
        }
    }

    /// Follow the draft for the open conversation, including changes from other members.
    pub(crate) fn open_draft(&self, choice: &Choice) {
        let key = crate::drafts::key(choice);
        let stored = self
            .drafts
            .as_ref()
            .filter(|record| record.may_read())
            .map(|record| record.get().drafts.get(&key).cloned().unwrap_or_default());
        if self.open.get() == key && self.last_stored.get() == stored {
            return;
        }
        self.open.set(key);
        self.last_stored.set(stored.clone());
        self.draft.set(stored.unwrap_or_default());
    }

    /// Keep what has been typed so closing the window does not lose it.
    pub(crate) fn keep_draft(&self, body: &str) {
        let Some(record) = self.drafts.as_ref().filter(|record| record.may_read()) else {
            return;
        };
        let key = self.open.get();
        if key.is_empty() || self.last_stored.get().as_deref() == Some(body) {
            return;
        }
        if record
            .invoke(&MessageDrafts::KEEP_DRAFT.with((key, body.to_owned())))
            .is_ok()
        {
            let stored = if body.trim().is_empty() {
                String::new()
            } else {
                body.to_owned()
            };
            self.last_stored.set(Some(stored));
        }
    }

    /// Drop the draft for a route that was forgotten or re-reviewed.
    pub(crate) fn forget_draft(&self) {
        let Some(record) = self.drafts.as_ref().filter(|record| record.may_read()) else {
            return;
        };
        let key = self.open.get();
        if !key.is_empty() && record.get().drafts.contains_key(&key) {
            let _ = record.invoke(&MessageDrafts::DROP_DRAFT.with(key));
        }
    }

    /// Remove the delivered text without clearing a newer draft.
    fn sent_draft(&self, key: &str, sent: &str) {
        if let Some(record) = self.drafts.as_ref().filter(|record| record.may_read()) {
            let _ = record.invoke(&MessageDrafts::SENT.with((key.to_owned(), sent.to_owned())));
        }
    }

    pub(crate) fn client(&self) -> Option<MessagingClient> {
        self.client.clone()
    }
    pub(crate) fn clear(&self) {
        self.generation.set(self.generation.get().saturating_add(1));
        self.draft.set(String::new());
        self.open.set(String::new());
        self.last_stored.set(None);
        self.operation.set(uuid::Uuid::new_v4().to_string());
    }
    pub(crate) fn available(&self) -> bool {
        self.client.is_some() || self.direct
    }
    pub(crate) fn supports(&self, route: &tilo_contact_directory::routing::Route) -> bool {
        if route.service == "tilo" {
            return self.direct;
        }
        self.client
            .as_ref()
            .is_some_and(|client| client.supports(route))
    }
    pub(crate) fn account_state(&self, choice: &Choice) -> tilo_messaging::AccountState {
        if choice.service == "tilo" {
            return tilo_messaging::AccountState::Ready;
        }
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
        let choice: Choice =
            serde_json::from_str(&choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
        if choice.service == "tilo" {
            return self.send_direct(sources, selection, history, &choice, &operation, &text, cx);
        }
        let client = self
            .client
            .clone()
            .ok_or_else(|| CATALOG.t("messages-transport-unavailable"))?;
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
            let page=client.history().read(receipt.route.clone(),None).await.ok();
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
                        tilo_messaging::SendError::NotYourDevice=>"messages-not-own-device",
                    };Err(CATALOG.t(key))
                },
            }
        }))
    }

    /// Send `text` into the conversation with one of the workspace's people, through Chat's
    /// `write_to`: the same message Chat sends.
    fn send_direct(
        &self,
        sources: &Sources,
        selection: &LocalState<Selection>,
        history: &crate::history::HistoryView,
        choice: &Choice,
        operation: &str,
        text: &str,
        cx: &mut EffectCx<Messages>,
    ) -> EffectAnswer {
        if !sources.tilo {
            return Err(CATALOG.t("messages-transport-unavailable"));
        }
        let route = crate::history::current_route(sources, &selection.get(), choice)?;
        let peer = selection.get().person;
        let call = crate::history::write_to(&peer, text);
        // The words are the message: an attempt that is asked for twice is the same
        // message, under the operation it was asked with.
        let answered = operation.to_owned();
        let sent_text = text.to_owned();
        let sent_route = route;
        let history = history.clone();
        let sending = self.clone();
        Ok(cx.call(&call, move |(), sent, cx| {
            let _ = sent?;
            let opened = history.clone();
            let sending = sending.clone();
            let sent_text = sent_text.clone();
            let answered = answered.clone();
            history.read_conversation(sent_route, &peer, cx, move |page, _| {
                opened.publish(page);
                if sending.draft.get() == sent_text {
                    sending.draft.set(String::new());
                    sending.operation.set(uuid::Uuid::new_v4().to_string());
                    sending.sent_draft(&sending.open.get(), &sent_text);
                }
                Ok(Effected::value(serde_json::json!({
                    "operation": answered,
                    "status": "submitted",
                })))
            })
        }))
    }
}
