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
//! its workspace's host where it is served, so Chat never builds a link itself and
//! a person, an automation and an agent all get the same URL. A message's
//! link is asked for once its controls are shown, ahead of a click: a browser
//! copies only inside the click itself, and the workspace's host may answer well
//! after it. A click that comes before the answer claims the clipboard
//! ([`ui::reserve_clipboard_copy`]) and fills it once the link is in.
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::InvokeArgument;
use ui::{Arrival, InvocationCall, RemoteActions};

use crate::app::ChatApp;

const CLIPBOARD_REFUSED: &str = "The system clipboard did not accept the link";

/// How long a link request may run before the control gives up on it.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Links asked for ahead of a click that are kept at once: the messages on
/// screen, with workspace to scroll.
const PREFETCHED: usize = 64;

/// One resource a message carries: a message posts several things, and each
/// keeps its own chip, its own link and its own outcome.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Slot {
    message: String,
    attachment: usize,
}

impl Slot {
    fn of(message: &str, attachment: usize) -> Self {
        Self {
            message: message.to_owned(),
            attachment,
        }
    }
}

pub(crate) struct Navigation {
    remote: RemoteActions,
    pending: Option<Pending>,
    /// What the last request came to, shown on the control that asked.
    status: Option<Status>,
    /// The last copied link and its slot: the copy can fail after the
    /// control said it was copied.
    copy: ui::CopyWatch,
    copied_for: Option<Slot>,
    /// Links asked for ahead of a click, by slot.
    prefetched: std::collections::HashMap<Slot, Asked>,
    /// The resource being opened, and the slot that shares it.
    opening: Option<(Slot, InvocationCall)>,
    /// The slots whose controls the last paint showed.
    shown: std::cell::RefCell<Vec<Slot>>,
}

/// A link asked for: of what, from where, and its answer.
struct Asked {
    at: tilo_reference::ResourceAt,
    started: ui::time::Instant,
    call: InvocationCall,
}

struct Status {
    slot: Slot,
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
            copied_for: None,
            prefetched: std::collections::HashMap::new(),
            opening: None,
            shown: std::cell::RefCell::new(Vec::new()),
        }
    }
}

struct Pending {
    slot: Slot,
    asked: Asked,
    /// The clipboard the click claimed when its link was not in yet.
    reserved: Option<ui::clipboard::ReservedCopy>,
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

/// The `record:<app>:<record>` provider of the record `at` lives in, its
/// record empty for the app's own.
fn provider(at: &tilo_reference::ResourceAt) -> String {
    format!("record:{}:{}", at.app, at.record.as_deref().unwrap_or(""))
}

impl Navigation {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some() || self.opening.is_some()
    }

    pub(crate) fn remote_mut(&mut self) -> &mut RemoteActions {
        &mut self.remote
    }

    /// Note that a message's attachment is on screen, so its link is asked
    /// for before anyone clicks its controls.
    pub(crate) fn shown(&self, message: &str, attachment: usize) {
        let slot = Slot::of(message, attachment);
        let mut shown = self.shown.borrow_mut();
        if !shown.contains(&slot) {
            shown.push(slot);
        }
    }

    /// What the last link request on the slot came to.
    pub(crate) fn status(&self, message: &str, attachment: usize) -> Option<&Result<(), String>> {
        let slot = Slot::of(message, attachment);
        self.status
            .as_ref()
            .filter(|status| status.slot == slot)
            .map(|status| &status.outcome)
    }

    /// What copying a link came to, as the window's bar says it.
    fn copied_notice(outcome: &Result<(), String>) -> (String, ui::widget::toast::ToastKind) {
        match outcome {
            Ok(()) => (
                crate::i18n::t("chat-resource-link-copied"),
                ui::widget::toast::ToastKind::Success,
            ),
            Err(why) => (
                format!(
                    "{}: {why}",
                    crate::i18n::t("chat-resource-link-unavailable")
                ),
                ui::widget::toast::ToastKind::Error,
            ),
        }
    }

    /// The caption of the control that replays `message`'s recording: what
    /// it does, or why the recorded app did not open it. `reason` adds why,
    /// for assistive technology.
    pub(crate) fn replay_label(&self, message: &str, reason: bool) -> String {
        match self.replay_outcome(message, true) {
            Some(Err(why)) if reason => {
                format!("{}: {why}", crate::i18n::t("chat-feedback-replay-failed"))
            }
            Some(Err(_)) => crate::i18n::t("chat-feedback-replay-failed"),
            Some(Ok(())) | None => crate::i18n::t("chat-feedback-replay"),
        }
    }

    /// Whether [`Self::replay_label`] is the catalogue's copy alone, without
    /// a failure's own words.
    pub(crate) fn replay_label_is_copy(&self, message: &str) -> bool {
        !matches!(self.replay_outcome(message, true), Some(Err(_)))
    }

    /// The name the reference chip, which reads `label`, is announced by:
    /// what pressing it does, or why it did not open.
    pub(crate) fn open_label(&self, message: &str, attachment: usize, label: &str) -> String {
        match self.outcome(message, attachment, true) {
            Some(Err(why)) => format!("{}: {why}", crate::i18n::t("chat-resource-not-opened")),
            Some(Ok(())) | None => ui::i18n::I18nManager::format(
                "reference-open",
                &[("label", label.to_owned().into())],
            ),
        }
    }

    fn outcome(&self, message: &str, attachment: usize, open: bool) -> Option<&Result<(), String>> {
        let slot = Slot::of(message, attachment);
        self.status
            .as_ref()
            .filter(|status| status.slot == slot && status.open == open)
            .map(|status| &status.outcome)
    }

    /// The replay control's outcome, which belongs to the message rather
    /// than to any one of its attachments.
    fn replay_outcome(&self, message: &str, open: bool) -> Option<&Result<(), String>> {
        self.status
            .as_ref()
            .filter(|status| status.slot.message == message && status.open == open)
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
    /// Ask the app behind a message's attachment for its link, to copy it.
    pub(crate) fn begin_resource_navigation(&mut self, message: &str, attachment: usize) {
        let started = self.request_resource_link(message, attachment);
        self.finish_starting(message, attachment, false, started);
    }

    /// Copy the link of the resource `message` shares, as its menu offers it.
    pub(crate) fn copy_shared_link(&mut self, message: &str) {
        let shared = self.message(message).and_then(|m| {
            m.attachments
                .iter()
                .position(|attached| attached.at().is_some())
        });
        if let Some(attachment) = shared {
            self.begin_resource_navigation(message, attachment);
        }
    }

    /// Open the resource a message shares where it lives: its own app
    /// opens at it, launched on its record when no window shows it.
    pub(crate) fn open_shared_resource(&mut self, message: &str, attachment: usize) {
        let started = self.request_open(message, attachment);
        self.finish_starting(message, attachment, true, started);
    }

    /// Replay the recording the report `message` shows carries: its app
    /// opens it, read from this chat's content as any opened content is
    /// (`docs/architecture/session-replay.md`).
    pub(crate) fn replay_recording(&mut self, message: &str) {
        let started = self.request_replay(message);
        self.finish_starting(message, 0, true, started);
    }

    fn request_replay(&mut self, message: &str) -> Result<(), String> {
        if self.resource_navigation.opening.is_some() {
            return Err("Something is already opening".into());
        }
        let recording = self
            .report_recordings
            .get(message)
            .ok_or("This message carries no recording")?;
        let launch = crate::feedback::replay_launch(recording);
        let call = self.resource_navigation.remote.invoke(
            launch.target(&ui::framework::reference::workspace()?),
            launch.argument(),
            None,
        );
        self.resource_navigation.status = None;
        self.resource_navigation.opening = Some((Slot::of(message, 0), call));
        Ok(())
    }

    fn finish_starting(
        &mut self,
        message: &str,
        attachment: usize,
        open: bool,
        started: Result<(), String>,
    ) {
        if let Err(reason) = started {
            self.resource_navigation.status = Some(Status {
                slot: Slot::of(message, attachment),
                open,
                outcome: Err(reason),
            });
        }
        self.settle_resource_navigation();
        self.request_relayout();
    }

    fn request_open(&mut self, message: &str, attachment: usize) -> Result<(), String> {
        if self.resource_navigation.opening.is_some() {
            return Err("Something is already opening".into());
        }
        let at = self.shared_resource(message, attachment)?;
        let workspace = ui::framework::reference::workspace()?;
        let target = at
            .reference
            .open_target(&workspace, &at.app, at.record.as_deref());
        let call = self
            .resource_navigation
            .remote
            .invoke(target, InvokeArgument::None, None);
        self.resource_navigation.status = None;
        self.resource_navigation.opening = Some((Slot::of(message, attachment), call));
        Ok(())
    }

    fn request_resource_link(&mut self, message: &str, attachment: usize) -> Result<(), String> {
        if self.resource_navigation.pending.is_some() {
            return Err("A link is already being fetched".into());
        }
        let slot = Slot::of(message, attachment);
        let at = self.shared_resource(message, attachment)?;
        // A link asked for ahead answers within the click, where a browser
        // still lets it be copied; one that failed is asked for again.
        let asked = match self.resource_navigation.prefetched.remove(&slot) {
            Some(mut asked) if asked.at == at && !matches!(answer(&asked.call), Some(Err(_))) => {
                // The person waits from their click on.
                asked.started = ui::time::now();
                asked
            }
            _ => self.ask_resource_link(at),
        };
        let reserved = answer(&asked.call)
            .is_none()
            .then(ui::reserve_clipboard_copy);
        self.resource_navigation.status = None;
        self.resource_navigation.pending = Some(Pending {
            slot,
            asked,
            reserved,
        });
        Ok(())
    }

    /// The resource a message's attachment names.
    fn shared_resource(
        &self,
        message: &str,
        attachment: usize,
    ) -> Result<tilo_reference::ResourceAt, String> {
        self.message(message)
            .ok_or("Message no longer exists")?
            .attachments
            .get(attachment)
            .and_then(tilo_reference::Attached::at)
            .cloned()
            .ok_or_else(|| "This message does not share a resource here".to_owned())
    }

    /// Ask the app behind `at`'s resource, in its own record, for its link.
    fn ask_resource_link(&self, at: tilo_reference::ResourceAt) -> Asked {
        let call = self.resource_navigation.remote.invoke(
            ui::tilo_resource::share::share_link_target(provider(&at), at.reference.id.clone()),
            link_arguments(&at.reference),
            None,
        );
        Asked {
            at,
            started: ui::time::now(),
            call,
        }
    }

    /// Ask for the links of the attachments whose controls were shown and
    /// have none asked for yet.
    fn prefetch_resource_links(&mut self) {
        let shown = std::mem::take(&mut *self.resource_navigation.shown.borrow_mut());
        for slot in shown {
            if self.resource_navigation.prefetched.len() >= PREFETCHED {
                return;
            }
            let pending = self
                .resource_navigation
                .pending
                .as_ref()
                .is_some_and(|pending| pending.slot == slot);
            let Ok(at) = self.shared_resource(&slot.message, slot.attachment) else {
                continue;
            };
            let asked = self
                .resource_navigation
                .prefetched
                .get(&slot)
                .is_some_and(|asked| asked.at == at);
            if pending || asked {
                continue;
            }
            let asked = self.ask_resource_link(at);
            self.resource_navigation.prefetched.insert(slot, asked);
        }
    }

    /// Finish the running requests once their answers have arrived: copy
    /// the link, or show why there is none or why nothing opened.
    pub(crate) fn settle_resource_navigation(&mut self) {
        if let Some((slot, call)) = &self.resource_navigation.opening
            && let Some(outcome) = opened(call)
        {
            self.resource_navigation.status = Some(Status {
                slot: slot.clone(),
                open: true,
                outcome,
            });
            self.resource_navigation.opening = None;
            self.request_relayout();
        }
        if self.resource_navigation.copy.failed()
            && let Some(slot) = self.resource_navigation.copied_for.take()
        {
            let outcome = Err(CLIPBOARD_REFUSED.to_owned());
            self.tell_copied(&outcome);
            self.resource_navigation.status = Some(Status {
                slot,
                open: false,
                outcome,
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
        let Some(mut pending) = self.resource_navigation.pending.take() else {
            return;
        };
        let reserved = pending.reserved.take();
        // The message may have changed while its link was fetched; a link
        // to what it no longer shows is not what the person asked for.
        let current = self.message(&pending.slot.message).is_some_and(|row| {
            row.attachments
                .get(pending.slot.attachment)
                .and_then(tilo_reference::Attached::at)
                == Some(&pending.asked.at)
        });
        let outcome = answer.and_then(|url| {
            if !current {
                Err("The message no longer shares this resource".to_owned())
            } else if self.resource_navigation.copy.watch(match reserved {
                Some(reserved) => reserved.fill(&url),
                None => ui::try_clipboard_copy(&url),
            }) {
                self.resource_navigation.copied_for = Some(pending.slot.clone());
                Ok(())
            } else {
                Err(CLIPBOARD_REFUSED.to_owned())
            }
        });
        // The link stays known for the next click on the same slot.
        if current && outcome.is_ok() {
            self.resource_navigation
                .prefetched
                .insert(pending.slot.clone(), pending.asked);
        }
        self.tell_copied(&outcome);
        self.resource_navigation.status = Some(Status {
            slot: pending.slot,
            open: false,
            outcome,
        });
        self.request_relayout();
    }

    /// Say in the window's bar what copying a link came to.
    fn tell_copied(&self, outcome: &Result<(), String>) {
        if let Some(bar) = &self.bar {
            let (text, kind) = Navigation::copied_notice(outcome);
            bar.toast(text, kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tilo_actions::remote::{
        ActionHostError, Caller, InvocationReceipt, Pending as HostPending, ReceiptStatus, TestHost,
    };

    use super::*;
    use crate::testing::MountedChat;

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

    const URL: &str = "https://tilo.so/app/#/host.example/workspace/so.tilo.notes/r/page:1";

    type Answer = tilo_actions::remote::Completer<Result<InvocationReceipt, ActionHostError>>;

    /// The resource's app behind the door: answers `share-link` with `URL`.
    fn door() -> TestHost {
        TestHost::new().completing(|_| tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"url": URL})),
            resources: Vec::new(),
        })
    }

    /// The resource's app behind the door, refusing with `message`.
    fn refusing(message: &str) -> TestHost {
        let message = message.to_owned();
        TestHost::new().answering(move |_, request| {
            HostPending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "refused".into(),
                        message: message.clone(),
                        details: None,
                        retryable: false,
                    },
                },
            }))
        })
    }

    /// The resource's app behind the door, answering only once the test
    /// hands over the answers it keeps.
    fn holding() -> (TestHost, Arc<Mutex<Vec<Answer>>>) {
        let held = Arc::new(Mutex::new(Vec::new()));
        let keep = Arc::clone(&held);
        let door = TestHost::new().answering(move |_, _| {
            let (answer, pending) = tilo_actions::remote::pending();
            keep.lock().unwrap().push(answer);
            pending
        });
        (door, held)
    }

    fn shared(app: &mut ui::framework::testing::TestApp<crate::resource::ChatShell>) -> String {
        let attached = serde_json::json!({"app":"so.tilo.notes", "record":"plan", "kind":"notes.page", "id":"notes.page:1", "label":"Plan", "span":{"start":6,"end":9,"unit":"Chars"}});
        app.call_on(
            &crate::testing::chain(),
            "share-resource",
            serde_json::json!({
                "channel": "c_chat", "attached":attached, "text":"Discuss resource"
            }),
        )
        .unwrap();
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
            assert!(app.harness().a11y_node_named("Open Plan").is_some());
            let controls: Vec<_> = app
                .chat()
                .hits
                .iter()
                .filter(|(_, hit)| matches!(hit, crate::app::Hit::OpenResource { .. }))
                .map(|(rect, _)| *rect)
                .collect();
            assert_eq!(
                controls.len(),
                1,
                "the chip alone: its link is in the message's menu"
            );
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
        let door = Arc::new(door());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(Clipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        assert_native_resource_controls(&mut app);

        // The chip's menu, the message's own, offers its link.
        app.chat()
            .open_message_menu(id.clone(), vello::kurbo::Point::new(40.0, 40.0));
        assert!(
            app.chat()
                .menu
                .as_ref()
                .is_some_and(|menu| menu.items.contains(&crate::app::MenuAction::CopyLink))
        );
        app.chat().menu = None;
        app.chat().begin_resource_navigation(&id, 0);
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        {
            let surface = app.chat();
            assert_eq!(surface.resource_navigation.status(&id, 0), Some(&Ok(())));
            assert!(!surface.resource_navigation.pending());
        }
        app.settle();
        assert_eq!(
            app.with_widget::<ui::widget::NavShell, _>(|shell| shell
                .toast_text()
                .map(str::to_owned))
                .flatten()
                .as_deref(),
            Some("Link copied"),
            "the window's bar says so"
        );
        let asked = door.asked();
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
        let door = Arc::new(door());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(Clipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        assert_native_resource_controls(&mut app);
        app.chat().settle_resource_navigation();
        app.chat().settle_resource_navigation();
        let asked = door.asked().len();
        assert_eq!(asked, 1, "asked once, however often the controls painted");

        let mut surface = app.chat();
        surface.begin_resource_navigation(&id, 0);
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        surface.begin_resource_navigation(&id, 0);
        assert_eq!(door.asked().len(), asked);
    }

    /// A browser's clipboard: it takes a copy only inside the click, or one
    /// the click claimed and filled later.
    #[derive(Default)]
    struct ClickClipboard {
        text: Arc<Mutex<Option<String>>>,
    }

    impl ui::clipboard::ClipboardBackend for ClickClipboard {
        fn set_text(&self, _: ui::clipboard::ClipboardTarget, _: &str) -> bool {
            false
        }

        fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
            self.text.lock().unwrap().clone()
        }

        fn reserve_text(&self) -> Option<ui::clipboard::ReservedCopy> {
            let text = Arc::clone(&self.text);
            Some(ui::clipboard::ReservedCopy::new(move |filled| {
                let copied = filled.is_some();
                if let Some(filled) = filled {
                    *text.lock().unwrap() = Some(filled.to_owned());
                }
                ui::clipboard::ClipboardCopy::settled(copied)
            }))
        }

        fn name(&self) -> &'static str {
            "shared-resource-click-clipboard"
        }
    }

    /// A click that comes before the host answered claims the clipboard in
    /// the click, so the link still lands there once the answer arrives.
    #[test]
    fn a_click_before_the_link_arrived_copies_it_once_it_does() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        let (door, held) = holding();
        let door = Arc::new(door);
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(ClickClipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        app.chat().begin_resource_navigation(&id, 0);
        assert!(app.chat().resource_navigation.pending());
        assert_eq!(ui::clipboard_paste(), None);

        let answers = std::mem::take(&mut *held.lock().unwrap());
        assert!(!answers.is_empty(), "the link was asked for");
        let asked = door.asked();
        for (answer, request) in answers.into_iter().zip(asked) {
            answer.complete(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({"url": URL})),
                        resources: Vec::new(),
                    },
                },
            }));
        }
        app.settle();
        app.chat().settle_resource_navigation();
        let surface = app.chat();
        assert_eq!(ui::clipboard_paste().as_deref(), Some(URL));
        assert_eq!(surface.resource_navigation.status(&id, 0), Some(&Ok(())));
        assert!(!surface.resource_navigation.pending());
    }

    #[test]
    fn a_refused_link_is_shown_on_its_message_and_copies_nothing() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        let door = Arc::new(refusing("This workspace's host serves no web page"));
        let _served = RemoteActions::enter(door, Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let services =
            ui::host::HostServices::null().with_clipboard(Arc::new(Clipboard::default()));
        let _scope = ui::host::enter(&services);
        let id = shared(&mut app);
        ui::clipboard_copy("sentinel");

        let mut surface = app.chat();
        surface.begin_resource_navigation(&id, 0);
        assert_eq!(
            surface.resource_navigation.status(&id, 0),
            Some(&Err("This workspace's host serves no web page".into()))
        );
        assert_eq!(
            surface.resource_navigation.status("another message", 0),
            None
        );
        assert_eq!(
            surface.resource_navigation.open_label(&id, 0, "Plan"),
            "Open Plan"
        );
        drop(surface);
        assert_eq!(ui::clipboard_paste().as_deref(), Some("sentinel"));
        app.settle();
        assert_eq!(
            app.with_widget::<ui::widget::NavShell, _>(|shell| shell
                .toast_text()
                .map(str::to_owned))
                .flatten()
                .as_deref(),
            Some("Link unavailable: This workspace's host serves no web page")
        );
    }
    /// The chip opens the shared resource where it lives, through the one
    /// action that opens any reference: the resource's `open` in the app and
    /// record the message names, in this window's workspace. What the door
    /// refuses is said on the chip.
    #[test]
    fn the_reference_chip_opens_the_resource_in_its_own_app_or_says_why_not() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        const WORKSPACE_ID: &str = "0b7f3a52-4c1d-4e8f-9a6b-2d5e8c1f7a30";
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", WORKSPACE_ID);
        let door = Arc::new(door());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let id = shared(&mut app);

        let chip = app.harness().a11y_node_named("Open Plan").unwrap();
        app.harness()
            .perform_a11y_action(chip, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        let asked = door
            .asked()
            .into_iter()
            .find(|request| request.target.address.action == "open")
            .expect("the chip asked the door to open the resource");
        let page = tilo_reference::ResourceRef::ranged(
            "notes.page",
            "notes.page:1",
            "Plan",
            tilo_reference::Span::chars(6, 9),
        );
        assert_eq!(
            asked.target,
            page.open_target(WORKSPACE_ID, "so.tilo.notes", Some("plan"))
        );
        assert_eq!(asked.argument, InvokeArgument::None);
        let surface = app.chat();
        assert_eq!(surface.resource_navigation.status(&id, 0), Some(&Ok(())));
        assert!(!surface.resource_navigation.pending());

        let door = Arc::new(refusing("You may not open this"));
        let _served = RemoteActions::enter(door, Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let id = shared(&mut app);
        let mut surface = app.chat();
        surface.open_shared_resource(&id, 0);
        assert_eq!(
            surface.resource_navigation.open_label(&id, 0, "Plan"),
            "Could not open: You may not open this"
        );
    }

    /// Words quoted from a record get the same controls as a whole resource:
    /// the chip opens the record they came from.
    #[test]
    fn a_quoted_run_opens_the_record_it_came_from() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        const WORKSPACE_ID: &str = "0b7f3a52-4c1d-4e8f-9a6b-2d5e8c1f7a30";
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_WORKSPACE_ID", WORKSPACE_ID);
        let door = Arc::new(door());
        let _served = RemoteActions::enter(door.clone(), Caller::local_person("owner"));
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let _scope = ui::host::enter(&ui::host::HostServices::null());
        app.call_on(
            &crate::testing::chain(),
            "post",
            serde_json::json!({
                "channel": "c_chat",
                "text": "About the plan",
                "attachments": [{"kind": "text", "text": "ship on Friday",
                    "from": {"app": "so.tilo.notes", "record": "plan", "kind": "notes.page",
                             "id": "notes.page:1", "label": "Plan"}}],
            }),
        )
        .unwrap();
        app.settle();

        let open = app
            .harness()
            .a11y_node_named("Open ship on Friday")
            .expect("the quoted run has the controls a resource has");
        app.harness()
            .perform_a11y_action(open, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        let page = tilo_reference::ResourceRef::whole("notes.page", "notes.page:1", "Plan");
        assert_eq!(
            door.asked()
                .into_iter()
                .find(|request| request.target.address.action == "open")
                .expect("the chip asked the door to open the record")
                .target,
            page.open_target(WORKSPACE_ID, "so.tilo.notes", Some("plan"))
        );
    }
}
