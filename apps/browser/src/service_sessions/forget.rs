//! Completion of the local fence/reap/delete barrier precedes canonical revocation.
use super::*;
use ui::collaboration::browser_service_sessions::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, Client, Pending,
};

type LocalResult = Result<(), String>;
type LocalCleanup = std::sync::Arc<std::sync::Mutex<Option<NativeCleanup>>>;

pub(crate) struct PendingForget {
    selection: Selection,
    client: Client,
    local: Option<std::sync::mpsc::Receiver<LocalResult>>,
    canonical: Option<Pending>,
    cleanup: LocalCleanup,
    answer: Option<ui::framework::EffectReply>,
    waker: ui::WakeHandle,
    local_done: bool,
    revoked: bool,
    failed: bool,
}

impl PendingForget {
    pub(crate) fn start(
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
        let cleanup = std::sync::Arc::new(std::sync::Mutex::new(cleanup));
        let worker_cleanup = cleanup.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let id = selection.id.clone();
        let wake = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-service-forget".into())
            .spawn(move || {
                let mut cleanup = worker_cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let result = if let Some(cleanup) = cleanup.as_mut() {
                    // Joining the first attempt may report a retained failure. The
                    // next finish retries through that same lease, never a new owner.
                    cleanup.finish().or_else(|_| cleanup.finish())
                } else {
                    crate::service_profiles::Profile::forget_closed(person, &id)
                        .map_err(|_| CATALOG.t("browser-service-forget-retry-failed"))
                };
                // Release custody back to the frame before publishing completion.
                drop(cleanup);
                let _ = send.send(result);
                wake.wake();
            });
        if spawned.is_err() {
            if let Some(view) = view {
                view.cleanup = cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
            }
            return Err(CATALOG.t("browser-service-forget-retry-failed"));
        }
        Ok(Self {
            selection,
            client,
            local: Some(receive),
            canonical: None,
            cleanup,
            answer: Some(answer),
            waker,
            local_done: false,
            revoked: false,
            failed: false,
        })
    }

    /// Poll on the frame thread; neither engine shutdown nor deletion waits here.
    pub(crate) fn poll(&mut self, view: Option<&mut ServiceView>) -> bool {
        let mut view = view;
        if let Some(local) = self.local.as_ref() {
            let result = match local.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(_) => Err(CATALOG.t("browser-service-forget-retry-failed")),
            };
            self.local = None;
            if let Some(view) = view
                .as_deref_mut()
                .filter(|view| view.selection.id == self.selection.id)
            {
                view.cleanup = self
                    .cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
            }
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

    pub(crate) fn report(&self) -> serde_json::Value {
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
            service: Service::Slack,
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
