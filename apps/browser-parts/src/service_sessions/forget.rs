//! Completion of the local fence/reap/delete barrier precedes canonical revocation.
use super::{CATALOG, EngineAdapter, Selection, ServiceView, native, wake_callback};
use ui::collaboration::browser_service_sessions::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, Client, Pending,
};

type LocalForget = native::Forget<EngineAdapter, crate::service_profiles::Profile>;

pub struct PendingForget {
    selection: Selection,
    client: Client,
    local: Option<LocalForget>,
    canonical: Option<Pending>,
    answer: Option<ui::framework::EffectReply>,
    waker: ui::WakeHandle,
    local_done: bool,
    revoked: bool,
    failed: bool,
}

impl PendingForget {
    pub fn start(
        selection: Selection,
        person: [u8; 32],
        client: Client,
        mut view: Option<&mut ServiceView>,
        answer: ui::framework::EffectReply,
        waker: ui::WakeHandle,
    ) -> Result<Self, String> {
        let cleanup = match view.as_deref_mut().filter(|view| {
            view.selection.id == selection.id
                && (!view.close_complete() || matches!(view.status, "forgetting" | "forgotten"))
        }) {
            Some(view) => Some(view.take_forget_cleanup(&selection.id)?),
            None => None,
        };
        let id = selection.id.clone();
        let local = match LocalForget::start(
            cleanup,
            move || {
                crate::service_profiles::Profile::forget_closed(person, &id)
                    .map_err(|_| CATALOG.t("browser-service-forget-retry-failed"))
            },
            wake_callback(waker.clone()),
        ) {
            Ok(local) => local,
            Err(cleanup) => {
                if let Some(view) = view {
                    view.cleanup = cleanup;
                }
                return Err(CATALOG.t("browser-service-forget-retry-failed"));
            }
        };
        Ok(Self {
            selection,
            client,
            local: Some(local),
            canonical: None,
            answer: Some(answer),
            waker,
            local_done: false,
            revoked: false,
            failed: false,
        })
    }

    /// Poll on the frame thread; neither engine shutdown nor deletion waits here.
    pub fn poll(&mut self, view: Option<&mut ServiceView>) -> bool {
        let mut view = view;
        if let Some(local) = self.local.as_ref() {
            let Some(result) = local.poll() else {
                return false;
            };
            if let Some(view) = view
                .as_deref_mut()
                .filter(|view| view.selection.id == self.selection.id)
            {
                view.cleanup = local.take_cleanup();
            }
            self.local = None;
            if let Err(error) = result {
                return self.finish(Err(error));
            }
            self.local_done = true;
            match self.client.request(
                Command::Forget {
                    id: self.selection.id.clone(),
                    setup_binding: self.selection.binding.clone(),
                },
                Some(self.waker.clone()),
            ) {
                Ok(pending) => self.canonical = Some(pending),
                Err(error) => return self.finish(Err(error)),
            }
        }
        let Some(result) = self.canonical.as_mut().and_then(Pending::poll) else {
            return false;
        };
        let result = result.and_then(|reply| confirm_forget(&self.selection, reply));
        if result.is_ok()
            && let Some(view) = view.filter(|view| view.selection.id == self.selection.id)
        {
            view.status = "forgotten";
        }
        self.finish(result)
    }

    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"session_id": self.selection.id, "service": self.selection.service,
            "status": if self.revoked { "forgotten" } else if self.failed { "forget-incomplete" } else { "forgetting" },
            "custody_revoked": self.revoked, "custody_backend_needed": !self.revoked,
            "engine_shutdown_confirmed": self.local_done, "local_only": false, "authenticated": false})
    }

    fn finish(&mut self, result: Result<(), String>) -> bool {
        self.revoked = result.is_ok();
        self.failed = result.is_err();
        if let Some(answer) = self.answer.take() {
            answer.finish(result.map(|()| ui::tilo_actions::ActionOutput {
                value: Some(self.report()),
                ..Default::default()
            }));
        }
        true
    }
}

fn confirm_forget(selection: &Selection, reply: Reply) -> Result<(), String> {
    match reply {
        Reply::Forgotten { id, setup_binding }
            if id == selection.id && setup_binding == selection.binding =>
        {
            Ok(())
        }
        _ => Err("Account revocation could not be confirmed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_confirmation_requires_selected_identity_and_setup() {
        let selection = Selection {
            id: "selected".into(),
            service: browser_continuity::sessions::Service::Slack,
            binding: "setup".into(),
        };
        assert!(
            confirm_forget(
                &selection,
                Reply::Forgotten {
                    id: "other".into(),
                    setup_binding: "setup".into()
                }
            )
            .is_err()
        );
        assert!(
            confirm_forget(
                &selection,
                Reply::Forgotten {
                    id: "selected".into(),
                    setup_binding: "other".into()
                }
            )
            .is_err()
        );
        assert!(
            confirm_forget(
                &selection,
                Reply::List {
                    sessions: Vec::new()
                }
            )
            .is_err()
        );
        assert!(
            confirm_forget(
                &selection,
                Reply::Forgotten {
                    id: "selected".into(),
                    setup_binding: "setup".into()
                }
            )
            .is_ok()
        );
    }
}
