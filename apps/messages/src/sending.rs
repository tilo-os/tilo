//! Sending uses the host-lent service and the same exact route as history.
use crate::{
    CATALOG, Messages, MessagesEffect,
    view::{Choice, Selection, Sources},
};
use tilo_messaging::{MessagingClient, SendRequest};
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
}
impl Sending {
    pub(crate) fn new(client: Option<MessagingClient>, principal: Option<&str>) -> Self {
        Self {
            client: client.filter(|c| principal == Some(c.principal())),
            draft: LocalState::new(String::new()),
            operation: LocalState::new(uuid::Uuid::new_v4().to_string()),
            generation: LocalState::new(0),
        }
    }
    pub(crate) fn client(&self) -> Option<MessagingClient> {
        self.client.clone()
    }
    pub(crate) fn clear(&self) {
        self.generation.set(self.generation.get().saturating_add(1));
        self.draft.set(String::new());
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
