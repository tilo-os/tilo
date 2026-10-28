//! Selected account removal uses the browser's ordinary action door. Only the
//! browser may fence its engine and retire the selected local profile lease.
use ui::framework::EffectReply;
use ui::tilo_actions::remote::{
    CatalogueQuery, Confirmation, InvokeArgument, application_action_target,
};
use ui::{Arrival, CatalogueCall, InvocationCall, RemoteActions};

pub(super) const TYPE: &str = "browser-service-session";
pub(super) const ACTION: &str = "forget-service-session";

pub(super) struct Removal {
    pub id: String,
    reply: Option<EffectReply>,
    catalogue: CatalogueCall,
    invocation: Option<InvocationCall>,
}

impl Removal {
    pub fn new(id: String, reply: EffectReply, remote: &RemoteActions) -> Self {
        Self {
            id,
            reply: Some(reply),
            catalogue: remote.catalogue(CatalogueQuery {
                max_results: 0,
                ..Default::default()
            }),
            invocation: None,
        }
    }

    /// Returns only after the browser reports a terminal outcome, never merely
    /// after admission. Transport and action failures leave the selection visible.
    pub fn poll(&mut self, remote: &RemoteActions) -> Option<Result<(), String>> {
        if self.reply.as_ref().is_some_and(EffectReply::is_cancelled) {
            if let Some(call) = &self.invocation {
                if let Arrival::Ready(update) = call.with(Clone::clone) {
                    let _ = remote.cancel(update.invocation());
                }
            } else {
                return self.finish(Err("Account removal was cancelled".into()));
            }
        }
        if self.invocation.is_none() {
            let catalogue = self.catalogue.with(Clone::clone);
            match catalogue {
                Arrival::Pending { .. } => return None,
                Arrival::Failed(error) => return self.finish(Err(format!("{error:?}"))),
                Arrival::Ready(page) => {
                    let mut rooms = page
                        .providers
                        .iter()
                        .filter(|provider| provider.app_id == "so.tilo.settings")
                        .map(|provider| provider.room.as_str())
                        .collect::<Vec<_>>();
                    rooms.sort_unstable();
                    rooms.dedup();
                    let [room] = rooms.as_slice() else {
                        return self.finish(Err(
                            "The account's workspace is unavailable or ambiguous".into(),
                        ));
                    };
                    let target = application_action_target(
                        room,
                        "so.tilo.browser",
                        format!("browser.service.{}", self.id),
                        ACTION,
                    );
                    self.invocation = Some(remote.invoke(
                        target,
                        InvokeArgument::None,
                        Some(Confirmation::Confirmed),
                    ));
                }
            }
        }
        let call = self.invocation.as_ref()?.with(Clone::clone);
        match call {
            Arrival::Failed(error) => self.finish(Err(format!("{error:?}"))),
            Arrival::Ready(ui::tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                self.reply.take()?.finish(Ok(output));
                Some(Ok(()))
            }
            Arrival::Ready(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                let message = error.message.clone();
                self.reply.take()?.answer(Err(error));
                Some(Err(message))
            }
            Arrival::Ready(ui::tilo_actions::InvocationUpdate::Cancelled { message, .. }) => self
                .finish(Err(
                    message.unwrap_or_else(|| "Account removal was cancelled".into())
                )),
            Arrival::Pending { .. } | Arrival::Ready(_) => None,
        }
    }

    fn finish(&mut self, result: Result<(), String>) -> Option<Result<(), String>> {
        self.reply.take()?.finish(
            result
                .clone()
                .map(|()| ui::tilo_actions::ActionOutput::default()),
        );
        Some(result)
    }
}
