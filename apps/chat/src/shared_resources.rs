//! The controls under a message that shares a resource: the resource as a
//! reference chip, and "Copy link".
//!
//! The chip opens the resource where it lives: its own app opens at it,
//! through the one action that opens any reference
//! ([`tilo_reference::ResourceRef::open_target`]), admitted as the person
//! under their grants.
//!
//! "Copy link" asks the resource's own app for its `share-link` through the
//! action door: the app that publishes the resource knows its views and asks
//! its room's host where it is served, so Chat never builds a link itself and
//! a person, an automation and an agent all get the same URL. A message's
//! link is asked for once its controls are shown, ahead of a click: a browser
//! copies only inside the click itself, and the room's host may answer well
//! after it.
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeArgument;
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::app::ChatApp;

const CLIPBOARD_REFUSED: &str = "The system clipboard did not accept the link";

/// How long a link request may run before the control gives up on it.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Links asked for ahead of a click that are kept at once: the messages on
/// screen, with room to scroll.
const PREFETCHED: usize = 64;

pub(crate) struct Navigation {
    remote: RemoteActions,
    pending: Option<Pending>,
    /// What the last request came to, shown on the control that asked.
    status: Option<Status>,
    /// The last copied link and its message: the copy can fail after the
    /// control said it was copied.
    copy: ui::CopyWatch,
    copied_for: String,
    /// Links asked for ahead of a click, by message.
    prefetched: std::collections::HashMap<String, Asked>,
    /// The resource being opened, and the message that shares it.
    opening: Option<(String, InvocationCall)>,
    /// The messages whose controls the last paint showed.
    shown: std::cell::RefCell<Vec<String>>,
}

/// A link asked for: of what, from where, and its answer.
struct Asked {
    reference: tilo_reference::ResourceRef,
    provider: String,
    started: ui::time::Instant,
    call: InvocationCall,
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
            prefetched: std::collections::HashMap::new(),
            opening: None,
            shown: std::cell::RefCell::new(Vec::new()),
        }
    }
}

struct Pending {
    message: String,
    asked: Asked,
}

/// What `call` came to: the link, why there is none, or `None` while it
/// runs.
fn answer(call: &InvocationCall) -> Option<Result<String, String>> {
    call.with(|call| match call {
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Some(
            output
                .value
                .as_ref()
                .and_then(|value| value.get("url"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| "The resource's app answered without a link".to_owned()),
        ),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(Err(error.message.clone())),
        Arrival::Ready(InvocationUpdate::Cancelled { .. }) => {
            Some(Err("The link request was cancelled".into()))
        }
        Arrival::Failed(error) => Some(Err(error.to_string())),
        Arrival::Ready(_) | Arrival::Pending { .. } => None,
    })
}

/// What opening came to, or `None` while it runs.
fn opened(call: &InvocationCall) -> Option<Result<(), String>> {
    call.with(|call| match call {
        Arrival::Ready(InvocationUpdate::Succeeded { .. }) => Some(Ok(())),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(Err(error.message.clone())),
        Arrival::Ready(InvocationUpdate::Cancelled { .. }) => {
            Some(Err("Opening was cancelled".into()))
        }
        Arrival::Failed(error) => Some(Err(error.to_string())),
        Arrival::Ready(_) | Arrival::Pending { .. } => None,
    })
}

impl Navigation {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some() || self.opening.is_some()
    }

    pub(crate) fn remote_mut(&mut self) -> &mut RemoteActions {
        &mut self.remote
    }

    /// Note that `message`'s controls are on screen, so its link is asked
    /// for before anyone clicks them.
    pub(crate) fn shown(&self, message: &str) {
        let mut shown = self.shown.borrow_mut();
        if !shown.iter().any(|known| known == message) {
            shown.push(message.to_owned());
        }
    }

    /// What the last link request on `message` came to.
    #[cfg(test)]
    pub(crate) fn status(&self, message: &str) -> Option<&Result<(), String>> {
        self.status
            .as_ref()
            .filter(|status| status.message == message)
            .map(|status| &status.outcome)
    }

    /// The caption of `message`'s copy control: what it does, or what it
    /// last came to. `reason` adds why a request failed, for assistive
    /// technology.
    pub(crate) fn label(&self, message: &str, reason: bool) -> String {
        match self.outcome(message, false) {
            Some(Ok(())) => crate::i18n::t("chat-resource-link-copied"),
            Some(Err(why)) if reason => format!(
                "{}: {why}",
                crate::i18n::t("chat-resource-link-unavailable")
            ),
            Some(Err(_)) => crate::i18n::t("chat-resource-link-unavailable"),
            None => crate::i18n::t("chat-resource-copy-link"),
        }
    }

    /// The name `message`'s reference chip, which reads `label`, is
    /// announced by: what pressing it does, or why it did not open.
    pub(crate) fn open_label(&self, message: &str, label: &str) -> String {
        match self.outcome(message, true) {
            Some(Err(why)) => format!("{}: {why}", crate::i18n::t("chat-resource-not-opened")),
            Some(Ok(())) | None => ui::i18n::I18nManager::format(
                "reference-open",
                &[("label", label.to_owned().into())],
            ),
        }
    }

    fn outcome(&self, message: &str, open: bool) -> Option<&Result<(), String>> {
        self.status
            .as_ref()
            .filter(|status| status.message == message && status.open == open)
            .map(|status| &status.outcome)
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
    /// Ask the app behind `message`'s resource for its link, to copy it.
    pub(crate) fn begin_resource_navigation(&mut self, message: &str) {
        let started = self.request_resource_link(message);
        self.finish_starting(message, false, started);
    }

    /// Open the resource `message` shares where it lives: its own app
    /// opens at it, launched on its record when no window shows it.
    pub(crate) fn open_shared_resource(&mut self, message: &str) {
        let started = self.request_open(message);
        self.finish_starting(message, true, started);
    }

    fn finish_starting(&mut self, message: &str, open: bool, started: Result<(), String>) {
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

    fn request_open(&mut self, message: &str) -> Result<(), String> {
        if self.resource_navigation.opening.is_some() {
            return Err("Something is already opening".into());
        }
        let (reference, provider) = self.shared_resource(message)?;
        let (app, record) = provider
            .strip_prefix("record:")
            .and_then(|rest| rest.split_once(':'))
            .ok_or("This message does not say which app keeps its resource")?;
        // The app's own record of the workspace is the one it opens on its
        // own.
        let record = match tilo_reference::link::RecordSegment::new(record) {
            Ok(tilo_reference::link::RecordSegment::Primary) => None,
            _ => Some(record),
        };
        let target = reference.open_target(&ui::framework::reference::room()?, app, record);
        let call = self
            .resource_navigation
            .remote
            .invoke(target, InvokeArgument::None, None);
        self.resource_navigation.status = None;
        self.resource_navigation.opening = Some((message.to_owned(), call));
        Ok(())
    }

    fn request_resource_link(&mut self, message: &str) -> Result<(), String> {
        if self.resource_navigation.pending.is_some() {
            return Err("A link is already being fetched".into());
        }
        let (reference, provider) = self.shared_resource(message)?;
        // A link asked for ahead answers within the click, where a browser
        // still lets it be copied; one that failed is asked for again.
        let asked = match self.resource_navigation.prefetched.remove(message) {
            Some(mut asked)
                if asked.reference == reference
                    && asked.provider == provider
                    && !matches!(answer(&asked.call), Some(Err(_))) =>
            {
                // The person waits from their click on.
                asked.started = ui::time::now();
                asked
            }
            _ => self.ask_resource_link(reference, provider),
        };
        self.resource_navigation.status = None;
        self.resource_navigation.pending = Some(Pending {
            message: message.to_owned(),
            asked,
        });
        Ok(())
    }

    /// The resource `message` shares, and the provider of its record.
    fn shared_resource(
        &self,
        message: &str,
    ) -> Result<(tilo_reference::ResourceRef, String), String> {
        let row = self.message(message).ok_or("Message no longer exists")?;
        let reference = row.reference.clone().ok_or("Message has no resource")?;
        let provider = row
            .source_provider
            .clone()
            .ok_or("This message does not say which record its resource is in")?;
        Ok((reference, provider))
    }

    /// Ask the app behind `reference`, in `provider`'s record, for its link.
    fn ask_resource_link(&self, reference: tilo_reference::ResourceRef, provider: String) -> Asked {
        let call = self.resource_navigation.remote.invoke(
            ui::tilo_resource::share::share_link_target(provider.clone(), reference.id.clone()),
            link_arguments(&reference),
            None,
        );
        Asked {
            reference,
            provider,
            started: ui::time::now(),
            call,
        }
    }

    /// Ask for the links of the messages whose controls were shown and
    /// have none asked for yet.
    fn prefetch_resource_links(&mut self) {
        let shown = std::mem::take(&mut *self.resource_navigation.shown.borrow_mut());
        for message in shown {
            if self.resource_navigation.prefetched.len() >= PREFETCHED {
                return;
            }
            let pending = self
                .resource_navigation
                .pending
                .as_ref()
                .is_some_and(|pending| pending.message == message);
            let Ok((reference, provider)) = self.shared_resource(&message) else {
                continue;
            };
            let asked = self
                .resource_navigation
                .prefetched
                .get(&message)
                .is_some_and(|asked| asked.reference == reference && asked.provider == provider);
            if pending || asked {
                continue;
            }
            let asked = self.ask_resource_link(reference, provider);
            self.resource_navigation.prefetched.insert(message, asked);
        }
    }

    /// Finish the running requests once their answers have arrived: copy
    /// the link, or show why there is none or why nothing opened.
    pub(crate) fn settle_resource_navigation(&mut self) {
        if let Some((message, call)) = &self.resource_navigation.opening
            && let Some(outcome) = opened(call)
        {
            self.resource_navigation.status = Some(Status {
                message: message.clone(),
                open: true,
                outcome,
            });
            self.resource_navigation.opening = None;
            self.request_relayout();
        }
        if self.resource_navigation.copy.failed() {
            self.resource_navigation.status = Some(Status {
                message: std::mem::take(&mut self.resource_navigation.copied_for),
                open: false,
                outcome: Err(CLIPBOARD_REFUSED.to_owned()),
            });
            self.request_relayout();
        }
        self.prefetch_resource_links();
        let Some(pending) = &self.resource_navigation.pending else {
            return;
        };
        let answer = match answer(&pending.asked.call) {
            Some(answer) => answer,
            None if ui::time::now().saturating_duration_since(pending.asked.started) >= TIMEOUT => {
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
            row.reference.as_ref() == Some(&pending.asked.reference)
                && row.source_provider.as_deref() == Some(pending.asked.provider.as_str())
        });
        let outcome = answer.and_then(|url| {
            if !current {
                Err("The message no longer shares this resource".to_owned())
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
        // The link stays known for the next click on the same message.
        if current && outcome.is_ok() {
            self.resource_navigation
                .prefetched
                .insert(pending.message.clone(), pending.asked);
        }
        self.resource_navigation.status = Some(Status {
            message: pending.message,
            open: false,
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
        app.call_on(&crate::testing::chain(), "share-resource", serde_json::json!({
            "channel": "c_chat", "reference":reference, "source_provider":"record:so.tilo.notes:plan", "text":"Discuss resource"
        })).unwrap();
        app.settle();
        crate::testing::mounted_rows(app, "c_chat")
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
            // Narrowed, the conversation stays in front; its slide lands.
            app.with_widget::<ui::widget::NavShell, _>(ui::widget::NavShell::snap);
            app.settle();
            assert!(
                app.harness()
                    .a11y_node_named("Copy resource link")
                    .is_some()
            );
            assert!(app.harness().a11y_node_named("Open Plan").is_some());
            let controls: Vec<_> = app
                .surface_mut::<ChatApp>()
                .hits
                .iter()
                .filter(|(_, hit)| {
                    matches!(
                        hit,
                        crate::app::Hit::CopyResourceLink(_) | crate::app::Hit::OpenResource(_)
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

        app.surface_mut::<ChatApp>().begin_resource_navigation(&id);
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        let surface = app.surface_mut::<ChatApp>();
        assert_eq!(surface.resource_navigation.status(&id), Some(&Ok(())));
        assert_eq!(surface.resource_navigation.label(&id, false), "Link copied");
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

    /// A browser copies only inside the click, and the host can answer
    /// after it: the link is asked for when its controls show, and the
    /// click copies it without asking again.
    #[test]
    fn a_shown_message_asks_for_its_link_before_the_click() {
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
        app.surface_mut::<ChatApp>().settle_resource_navigation();
        app.surface_mut::<ChatApp>().settle_resource_navigation();
        let asked = door.asked.lock().unwrap().len();
        assert_eq!(asked, 1, "asked once, however often the controls painted");

        let surface = app.surface_mut::<ChatApp>();
        surface.begin_resource_navigation(&id);
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        surface.begin_resource_navigation(&id);
        assert_eq!(door.asked.lock().unwrap().len(), asked);
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
        surface.begin_resource_navigation(&id);
        assert_eq!(
            surface.resource_navigation.status(&id),
            Some(&Err("This workspace's host serves no web page".into()))
        );
        assert_eq!(surface.resource_navigation.status("another message"), None);
        assert_eq!(
            surface.resource_navigation.label(&id, true),
            "Link unavailable: This workspace's host serves no web page"
        );
        assert_eq!(
            surface.resource_navigation.open_label(&id, "Plan"),
            "Open Plan"
        );
        assert_eq!(ui::clipboard_paste().as_deref(), Some("sentinel"));
    }
    /// The chip opens the shared resource where it lives, through the one
    /// action that opens any reference: the resource's `open` in the app and
    /// record the message names, in this window's workspace. What the door
    /// refuses is said on the chip.
    #[test]
    fn the_reference_chip_opens_the_resource_in_its_own_app_or_says_why_not() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        const ROOM: &str = "0b7f3a52-4c1d-4e8f-9a6b-2d5e8c1f7a30";
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", ROOM);
        let door = Arc::new(Door::default());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let id = shared(&mut app);

        let chip = app.harness().a11y_node_named("Open Plan").unwrap();
        app.harness()
            .perform_a11y_action(chip, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        let asked = door
            .asked
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.target.address.action == "open")
            .cloned()
            .expect("the chip asked the door to open the resource");
        let page = tilo_reference::ResourceRef::ranged(
            "notes.page",
            "notes.page:1",
            "Plan",
            tilo_reference::Span::chars(6, 9),
        );
        assert_eq!(
            asked.target,
            page.open_target(ROOM, "so.tilo.notes", Some("plan"))
        );
        assert_eq!(asked.argument, InvokeArgument::None);
        let surface = app.surface_mut::<ChatApp>();
        assert_eq!(surface.resource_navigation.status(&id), Some(&Ok(())));
        assert!(!surface.resource_navigation.pending());

        let door = Arc::new(Door {
            refusal: Some("You may not open this".into()),
            ..Door::default()
        });
        let _served = RemoteActions::enter(door, Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let id = shared(&mut app);
        let surface = app.surface_mut::<ChatApp>();
        surface.open_shared_resource(&id);
        assert_eq!(
            surface.resource_navigation.open_label(&id, "Plan"),
            "Could not open: You may not open this"
        );
        assert_eq!(
            surface.resource_navigation.label(&id, false),
            "Copy resource link",
            "the copy control says nothing of the open"
        );
    }
}
