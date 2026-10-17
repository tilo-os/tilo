//! The controls under a message that shares a resource. "Copy link" and
//! "Open" ask the resource's own app for its `share-link` through the action
//! door: the app that publishes the resource knows its views and asks its
//! room's host where it is served, so Chat never builds a link itself and a
//! person, an automation and an agent all get the same URL.
//!
//! "Open" opens that link in the browser. Chat has no way to show another
//! app's view in place, so the link is the one path that works for every
//! resource a message can carry.
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeArgument;
use ui::{InvocationCall, LocalState, RemoteActions, RemoteCall};

use crate::app::ChatApp;

const CLIPBOARD_REFUSED: &str = "The system clipboard did not accept the link";

/// How long a link request may run before the control gives up on it.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) struct Navigation {
    remote: RemoteActions,
    pending: Option<Pending>,
    /// What the last request came to, shown on the control that asked.
    status: Option<Status>,
    /// The last copied link and its message: the copy can fail after the
    /// control said it was copied.
    copy: ui::CopyWatch,
    copied_for: String,
}

struct Status {
    message: String,
    open: bool,
    /// The reason when it failed.
    outcome: Result<(), String>,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            remote: RemoteActions::current(),
            pending: None,
            status: None,
            copy: ui::CopyWatch::default(),
            copied_for: String::new(),
        }
    }
}

struct Pending {
    message: String,
    reference: tilo_reference::ResourceRef,
    provider: String,
    open: bool,
    started: ui::time::Instant,
    call: LocalState<InvocationCall>,
}

impl Navigation {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn remote_mut(&mut self) -> &mut RemoteActions {
        &mut self.remote
    }

    /// What the last link request on `message` came to.
    #[cfg(test)]
    pub(crate) fn status(&self, message: &str) -> Option<&Result<(), String>> {
        self.status
            .as_ref()
            .filter(|status| status.message == message)
            .map(|status| &status.outcome)
    }

    /// The caption of `message`'s copy control or, with `open`, its open
    /// control: what it does, or what it last came to. `reason` adds why a
    /// request failed, for assistive technology.
    pub(crate) fn label(&self, message: &str, open: bool, reason: bool) -> String {
        let status = self
            .status
            .as_ref()
            .filter(|status| status.message == message && status.open == open);
        match status.map(|status| &status.outcome) {
            Some(Ok(())) if !open => crate::i18n::t("chat-resource-link-copied"),
            Some(Err(why)) if reason => format!(
                "{}: {why}",
                crate::i18n::t("chat-resource-link-unavailable")
            ),
            Some(Err(_)) => crate::i18n::t("chat-resource-link-unavailable"),
            Some(Ok(())) | None if open => crate::i18n::t("chat-resource-open-link"),
            Some(Ok(())) | None => crate::i18n::t("chat-resource-copy-link"),
        }
    }
}

/// The `share-link` arguments that point at `reference`: its range, when it
/// names one, and a link that opens the resource on its own.
fn link_arguments(reference: &tilo_reference::ResourceRef) -> InvokeArgument {
    let mut args = serde_json::Map::from_iter([("mode".to_owned(), serde_json::json!("app"))]);
    if let Some(span) = reference.span {
        let unit = match span.unit {
            tilo_reference::SpanUnit::Chars => "chars",
            tilo_reference::SpanUnit::Bytes => "bytes",
        };
        args.insert("start".into(), serde_json::json!(span.start));
        args.insert("end".into(), serde_json::json!(span.end));
        args.insert("unit".into(), serde_json::json!(unit));
    }
    InvokeArgument::Json(args)
}

impl ChatApp {
    /// Ask the app behind `message`'s resource for its link, to copy it or,
    /// with `open`, to open it.
    pub(crate) fn begin_resource_navigation(&mut self, message: &str, open: bool) {
        let started = self.request_resource_link(message, open);
        if let Err(reason) = started {
            self.resource_navigation.status = Some(Status {
                message: message.to_owned(),
                open,
                outcome: Err(reason),
            });
        }
        self.settle_resource_navigation();
        self.request_relayout();
    }

    fn request_resource_link(&mut self, message: &str, open: bool) -> Result<(), String> {
        if self.resource_navigation.pending.is_some() {
            return Err("A link is already being fetched".into());
        }
        let row = self.message(message).ok_or("Message no longer exists")?;
        let reference = row.reference.clone().ok_or("Message has no resource")?;
        let provider = row
            .source_provider
            .clone()
            .ok_or("This message does not say which record its resource is in")?;
        let call = self.resource_navigation.remote.invoke(
            ui::tilo_resource::share::share_link_target(provider.clone(), reference.id.clone()),
            link_arguments(&reference),
            None,
        );
        self.resource_navigation.status = None;
        self.resource_navigation.pending = Some(Pending {
            message: message.to_owned(),
            reference,
            provider,
            open,
            started: ui::time::now(),
            call,
        });
        Ok(())
    }

    /// Finish the running link request once its answer has arrived: copy or
    /// open the link, or show why there is none.
    pub(crate) fn settle_resource_navigation(&mut self) {
        if self.resource_navigation.copy.failed() {
            self.resource_navigation.status = Some(Status {
                message: std::mem::take(&mut self.resource_navigation.copied_for),
                open: false,
                outcome: Err(CLIPBOARD_REFUSED.to_owned()),
            });
            self.request_relayout();
        }
        let Some(pending) = &self.resource_navigation.pending else {
            return;
        };
        let answer = pending.call.with(|call| match call {
            RemoteCall::Done(InvocationUpdate::Succeeded { output, .. }) => Some(
                output
                    .value
                    .as_ref()
                    .and_then(|value| value.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "The resource's app answered without a link".to_owned()),
            ),
            RemoteCall::Done(InvocationUpdate::Failed { error, .. }) => {
                Some(Err(error.message.clone()))
            }
            RemoteCall::Done(InvocationUpdate::Cancelled { .. }) => {
                Some(Err("The link request was cancelled".into()))
            }
            RemoteCall::Refused(error) => Some(Err(error.to_string())),
            RemoteCall::Done(_) | RemoteCall::Pending => None,
        });
        let answer = match answer {
            Some(answer) => answer,
            None if ui::time::now().saturating_duration_since(pending.started) >= TIMEOUT => {
                Err("The resource's app did not answer".into())
            }
            None => return,
        };
        let Some(pending) = self.resource_navigation.pending.take() else {
            return;
        };
        // The message may have changed while its link was fetched; a link
        // to what it no longer shows is not what the person asked for.
        let current = self.message(&pending.message).is_some_and(|row| {
            row.reference.as_ref() == Some(&pending.reference)
                && row.source_provider.as_deref() == Some(pending.provider.as_str())
        });
        let outcome = answer.and_then(|url| {
            if !current {
                Err("The message no longer shares this resource".to_owned())
            } else if pending.open {
                ui::widget::try_open_external_url(&url).map_err(|error| error.to_string())
            } else if self
                .resource_navigation
                .copy
                .watch(ui::try_clipboard_copy(&url))
            {
                self.resource_navigation
                    .copied_for
                    .clone_from(&pending.message);
                Ok(())
            } else {
                Err(CLIPBOARD_REFUSED.to_owned())
            }
        });
        self.resource_navigation.status = Some(Status {
            message: pending.message,
            open: pending.open,
            outcome,
        });
        self.request_relayout();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeRequest, Pending as HostPending, ReceiptStatus,
    };

    use super::*;

    #[derive(Default)]
    struct Clipboard(Mutex<Option<String>>);

    impl ui::clipboard::ClipboardBackend for Clipboard {
        fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
            *self.0.lock().unwrap() = Some(text.into());
            true
        }

        fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.0.lock().unwrap().clone()
        }

        fn name(&self) -> &'static str {
            "shared-resource-test-clipboard"
        }
    }

    const URL: &str = "https://tilo.so/app/#/host.example/room/so.tilo.notes/r/page:1";

    /// The resource's app behind the door: answers `share-link` with `URL`,
    /// or refuses with `refusal`.
    #[derive(Default)]
    struct Door {
        refusal: Option<String>,
        asked: Mutex<Vec<InvokeRequest>>,
    }

    impl ActionHost for Door {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> HostPending<Result<CataloguePage, ActionHostError>> {
            HostPending::ready(Err(ActionHostError::NotPermitted))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> HostPending<Result<InvocationReceipt, ActionHostError>> {
            let invocation = request.invocation;
            self.asked.lock().unwrap().push(request);
            let status = match &self.refusal {
                Some(message) => ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "refused".into(),
                        message: message.clone(),
                        details: None,
                        retryable: false,
                    },
                },
                None => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"url": URL})),
                        resources: Vec::new(),
                    },
                },
            };
            HostPending::ready(Ok(InvocationReceipt { invocation, status }))
        }

        fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Result<(), ActionHostError> {
            Ok(())
        }
    }

    fn shared(app: &mut ui::framework::testing::TestApp<crate::resource::ChatShell>) -> String {
        let reference = serde_json::json!({"kind":"notes.page", "id":"notes.page:1", "label":"Plan", "span":{"start":6,"end":9,"unit":"Chars"}});
        app.call_on(&crate::testing::item("channels", "c_general"), "share-resource", serde_json::json!({
            "reference":reference, "source_provider":"record:so.tilo.notes:plan", "text":"Discuss resource"
        })).unwrap();
        app.settle();
        app.state()
            .message_rows("c_general")
            .into_iter()
            .find(|row| row.text == "Discuss resource")
            .unwrap()
            .id
    }

    fn assert_native_resource_controls(
        app: &mut ui::framework::testing::TestApp<crate::resource::ChatShell>,
    ) {
        for theme in [
            ui::Theme::dark(),
            ui::Theme::light(),
            ui::Theme::high_contrast(),
        ] {
            app.harness().set_theme(theme);
            app.harness().resize((390, 844)).unwrap();
            app.settle();
            assert!(
                app.harness()
                    .a11y_node_named("Copy resource link")
                    .is_some()
            );
            assert!(
                app.harness()
                    .a11y_node_named("Open resource in browser")
                    .is_some()
            );
            let controls: Vec<_> = app
                .surface_mut::<ChatApp>()
                .hits
                .iter()
                .filter(|(_, hit)| {
                    matches!(
                        hit,
                        crate::app::Hit::CopyResourceLink(_) | crate::app::Hit::OpenResourceLink(_)
                    )
                })
                .map(|(rect, _)| *rect)
                .collect();
            assert_eq!(controls.len(), 2);
            assert!(controls.iter().all(|rect| rect.width() > 0.0
                && rect.height() > 0.0
                && rect.x0 >= 0.0
                && rect.x1 <= 390.0));
        }
    }

    #[test]
    fn copy_link_asks_the_resources_app_for_share_link_and_copies_its_url() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        let door = Arc::new(Door::default());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(Clipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        assert_native_resource_controls(&mut app);

        app.surface_mut::<ChatApp>()
            .begin_resource_navigation(&id, false);
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        let surface = app.surface_mut::<ChatApp>();
        assert_eq!(surface.resource_navigation.status(&id), Some(&Ok(())));
        assert_eq!(
            surface.resource_navigation.label(&id, false, false),
            "Link copied"
        );
        assert!(!surface.resource_navigation.pending());
        let asked = door.asked.lock().unwrap().clone();
        let request = asked.last().unwrap();
        assert_eq!(request.target.provider, "record:so.tilo.notes:plan");
        assert_eq!(request.target.address.resource, "notes.page:1");
        assert_eq!(request.target.address.action, "share-link");
        assert_eq!(
            request.argument,
            InvokeArgument::Json(
                serde_json::json!({"mode": "app", "start": 6, "end": 9, "unit": "chars"})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
    }

    #[test]
    fn a_refused_link_is_shown_on_its_message_and_copies_nothing() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        let door = Arc::new(Door {
            refusal: Some("This workspace's host serves no web page".into()),
            ..Door::default()
        });
        let _served = RemoteActions::enter(door, Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(Clipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        ui::clipboard_copy("sentinel");

        let surface = app.surface_mut::<ChatApp>();
        surface.begin_resource_navigation(&id, false);
        assert_eq!(
            surface.resource_navigation.status(&id),
            Some(&Err("This workspace's host serves no web page".into()))
        );
        assert_eq!(surface.resource_navigation.status("another message"), None);
        assert_eq!(
            surface.resource_navigation.label(&id, false, true),
            "Link unavailable: This workspace's host serves no web page"
        );
        assert_eq!(
            surface.resource_navigation.label(&id, true, true),
            "Open resource in browser"
        );
        assert_eq!(ui::clipboard_paste().as_deref(), Some("sentinel"));
    }
}
