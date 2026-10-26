//! Device-local permission and download surfaces backed by engine outcomes.
use std::collections::BTreeMap;
use ui::icons::Icon;
use ui::tokens::TextRole;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{Button, Column, Container, EmptyState, ListRow, Row, Text, Widget};

/// An extension's request for more access (`chrome.permissions.request`),
/// as the person is asked it: its name and what it could then do, one line
/// each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionQuestion {
    pub request: u32,
    pub name: String,
    pub lines: Vec<String>,
}

/// Extensions' questions, which their answer takes away; `version` moves
/// with each change, so the window redraws.
#[derive(Default)]
struct ExtensionQuestions {
    open: BTreeMap<u32, ExtensionQuestion>,
    version: u64,
}

impl ExtensionQuestions {
    /// Tell the engine the person's answer to `request`, which goes.
    fn answer(&mut self, engine: &SharedEngine, request: u32, allow: bool) {
        if self.open.remove(&request).is_some() {
            self.version = self.version.wrapping_add(1);
            engine.send(WebViewCommand::ExtensionPermissionDecision { request, allow });
        }
    }
}

pub struct Capabilities {
    permissions: BTreeMap<u32, (String, String)>,
    extension_questions: std::rc::Rc<std::cell::RefCell<ExtensionQuestions>>,
    observed_question_version: u64,
    // Native events are drained on the UI thread. Tabs share this UI model;
    // background download workers communicate through the engine protocol.
    downloads: std::rc::Rc<std::cell::RefCell<DownloadStore>>,
    observed_download_version: u64,
    notice: ui::LocalState<String>,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            permissions: BTreeMap::new(),
            extension_questions: std::rc::Rc::default(),
            observed_question_version: 0,
            downloads: std::rc::Rc::default(),
            observed_download_version: 0,
            notice: ui::LocalState::new(String::new()),
        }
    }
}

struct DownloadStore {
    prompts: BTreeMap<u32, (String, String)>,
    records: BTreeMap<u32, Download>,
    admitted: u32,
    detached: u32,
    accepting: bool,
    runtime_known: bool,
    terminal_notice: String,
    version: u64,
}

impl Default for DownloadStore {
    fn default() -> Self {
        Self {
            prompts: BTreeMap::new(),
            records: BTreeMap::new(),
            admitted: 0,
            detached: 0,
            accepting: true,
            runtime_known: false,
            terminal_notice: String::new(),
            version: 0,
        }
    }
}

/// Ask the host to open a finished download, or reveal its folder. The host
/// answers on the returned channel; the UI thread never waits for it.
fn open_download(
    path: &std::path::Path,
    reveal: bool,
) -> Result<ui::portal::LaunchChannel, String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("The downloaded file is no longer available.".into());
    }
    let request = if reveal {
        ui::portal::LaunchRequest::reveal(path)
    } else {
        ui::portal::LaunchRequest::open(path)
    };
    Ok(request.thread_name("browser-open-download").send())
}

/// Open a finished download, or reveal its folder, and say in the downloads'
/// notice why that did not work.
fn launch_download(path: &std::path::Path, reveal: bool, notice: &ui::LocalStateWriter<String>) {
    match open_download(path, reveal) {
        Err(message) => notice.set(message),
        Ok(channel) => {
            let notice = notice.clone();
            let _ = std::thread::Builder::new()
                .name("browser-open-download-result".into())
                .spawn(move || {
                    notice.set(match channel.recv() {
                        Ok(ui::portal::LaunchOutcome::Opened) => String::new(),
                        Ok(ui::portal::LaunchOutcome::Failed(reason)) => reason,
                        Err(_) => "Could not open the download. Try revealing its folder.".into(),
                    });
                });
        }
    }
}

#[derive(Default)]
struct Download {
    name: String,
    url: String,
    retryable: bool,
    state: String,
    path: String,
    received: u64,
    total: u64,
    error: String,
}
impl Capabilities {
    /// Ask the person an extension's question, after those already asked.
    pub fn ask_for_extension(&self, question: ExtensionQuestion) {
        let mut questions = self.extension_questions.borrow_mut();
        if questions.open.len() < 32 {
            questions.open.insert(question.request, question);
            questions.version = questions.version.wrapping_add(1);
        }
    }
    /// Ask the person extensions' requests for more access, from their
    /// popups or their pages in this window; whether the questions on offer
    /// changed since last asked, by these or an answer.
    pub fn ask(&mut self, questions: Vec<ExtensionQuestion>) -> bool {
        for question in questions {
            self.ask_for_extension(question);
        }
        self.extension_questions_changed()
    }
    /// Whether an extension's question came or was answered since asked.
    pub fn extension_questions_changed(&mut self) -> bool {
        let version = self.extension_questions.borrow().version;
        let changed = version != self.observed_question_version;
        self.observed_question_version = version;
        changed
    }
    /// Whether an extension's question waits for the person: the window asks
    /// it over the extension's own popup, which waits for the answer.
    pub fn asks_for_extension(&self) -> bool {
        !self.extension_questions.borrow().open.is_empty()
    }
    pub fn downloads_changed(&mut self) -> bool {
        let version = self.downloads.borrow().version;
        let changed = version != self.observed_download_version;
        self.observed_download_version = version;
        changed
    }
    /// Whether the Library shows this engine's downloads: those it has, or
    /// what it says about them. An engine with neither leaves the section to
    /// the saved list, with the downloads of earlier sessions.
    pub fn owns_download_presentation(&self) -> bool {
        let downloads = self.downloads.borrow();
        (downloads.runtime_known && !downloads.accepting)
            || !downloads.records.is_empty()
            || !downloads.prompts.is_empty()
            || !downloads.terminal_notice.is_empty()
    }
    pub fn toolbar_status(&self) -> Option<String> {
        let downloads = self.downloads.borrow();
        let active: Vec<_> = downloads
            .records
            .values()
            .filter(|download| download.state == "progress")
            .collect();
        let count = active.len() + downloads.prompts.len();
        if count > 0 {
            let determinate =
                downloads.prompts.is_empty() && active.iter().all(|download| download.total > 0);
            if determinate {
                let received = active
                    .iter()
                    .map(|download| u128::from(download.received))
                    .sum::<u128>();
                let total = active
                    .iter()
                    .map(|download| u128::from(download.total))
                    .sum::<u128>();
                let percent = received
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0)
                    .min(100);
                return Some(format!(
                    "{count} {} at {percent}%",
                    if count == 1 { "download" } else { "downloads" }
                ));
            }
            return Some(format!(
                "{count} active {}",
                if count == 1 { "download" } else { "downloads" }
            ));
        }
        (!downloads.terminal_notice.is_empty()).then(|| downloads.terminal_notice.clone())
    }
    pub fn exit_requires_choice(&self) -> bool {
        let downloads = self.downloads.borrow();
        downloads.admitted > 0
            || downloads.detached > 0
            || !downloads.prompts.is_empty()
            || downloads
                .records
                .values()
                .any(|download| download.state == "progress")
    }
    pub fn exit_cleanup_complete(&self) -> bool {
        let downloads = self.downloads.borrow();
        downloads.runtime_known
            && !downloads.accepting
            && downloads.admitted == 0
            && downloads.detached == 0
            && downloads.prompts.is_empty()
            && downloads
                .records
                .values()
                .all(|download| download.state != "progress")
    }
    pub fn cancel_all(engine: &SharedEngine) {
        engine.send(WebViewCommand::CancelAllDownloads);
    }
    pub fn apply(&mut self, event: WebViewEvent) {
        match event {
            WebViewEvent::PermissionRequested {
                id,
                origin,
                permissions,
            } => {
                if self.permissions.len() < 32 {
                    self.permissions.insert(id, (origin, permissions));
                }
            }
            WebViewEvent::PermissionClosed { id } => {
                self.permissions.remove(&id);
            }
            // The page that asked closed first: the extension was told no,
            // so the question goes without an answer.
            WebViewEvent::ExtensionPermissionClosed { request } => {
                let mut questions = self.extension_questions.borrow_mut();
                if questions.open.remove(&request).is_some() {
                    questions.version = questions.version.wrapping_add(1);
                }
            }
            WebViewEvent::NativeDownloadRequested { id, url, name } => {
                self.request_download(id, url, name);
            }
            WebViewEvent::ExtensionDownloadOpen { id, reveal } => {
                self.open_for_extension(id, reveal);
            }
            WebViewEvent::DownloadUpdated {
                id,
                state,
                path,
                name,
                url,
                retryable,
                received,
                total,
                error,
            } => {
                let mut downloads = self.downloads.borrow_mut();
                downloads.version = downloads.version.wrapping_add(1);
                let prompt = downloads.prompts.get(&id).cloned();
                if !path.is_empty() || state != "progress" {
                    downloads.prompts.remove(&id);
                }
                if downloads.records.len() >= 100
                    && !downloads.records.contains_key(&id)
                    && let Some(old) = downloads
                        .records
                        .iter()
                        .find(|(_, d)| d.state != "progress")
                        .map(|(id, _)| *id)
                {
                    downloads.records.remove(&old);
                }
                if downloads.records.len() < 100 || downloads.records.contains_key(&id) {
                    {
                        let record = downloads.records.entry(id).or_default();
                        if !name.is_empty() {
                            record.name = name;
                        } else if record.name.is_empty() {
                            record.name = prompt
                                .as_ref()
                                .map(|(_, name)| name.clone())
                                .unwrap_or_default();
                        }
                        if !url.is_empty() {
                            record.url = url;
                        } else if record.url.is_empty() {
                            record.url = prompt
                                .as_ref()
                                .map(|(url, _)| url.clone())
                                .unwrap_or_default();
                        }
                        record.retryable |= retryable;
                        if !path.is_empty() {
                            record.path = path;
                        }
                        if received > 0 || record.received == 0 {
                            record.received = received;
                        }
                        if total > 0 || record.total == 0 {
                            record.total = total;
                        }
                        if !error.is_empty() {
                            record.error = error;
                        }
                        record.state.clone_from(&state);
                    }
                    if state != "progress" {
                        downloads.terminal_notice = match state.as_str() {
                            "complete" => "Download complete".into(),
                            "failed" => "Download failed".into(),
                            "cancelled" => "Download cancelled".into(),
                            _ => String::new(),
                        };
                    }
                }
            }
            WebViewEvent::DownloadRuntime {
                admitted,
                detached,
                accepting,
                error,
            } => {
                self.apply_runtime(admitted, detached, accepting, &error);
            }
            _ => {}
        }
    }
    fn request_download(&self, id: u32, url: String, name: String) {
        let mut downloads = self.downloads.borrow_mut();
        downloads.version = downloads.version.wrapping_add(1);
        if downloads.prompts.len() < 32 {
            downloads.prompts.insert(id, (url, name));
        }
    }
    fn apply_runtime(&self, admitted: u32, detached: u32, accepting: bool, error: &str) {
        let mut downloads = self.downloads.borrow_mut();
        downloads.version = downloads.version.wrapping_add(1);
        downloads.admitted = admitted;
        downloads.detached = detached;
        downloads.accepting = accepting;
        downloads.runtime_known = true;
        if !error.is_empty() {
            downloads.prompts.clear();
            for record in downloads.records.values_mut() {
                if record.state == "progress" {
                    record.state = "failed".into();
                    error.clone_into(&mut record.error);
                }
            }
            downloads.terminal_notice = "Download failed".into();
        }
    }
    pub fn prompt(&self, engine: &SharedEngine) -> Option<Box<dyn Widget>> {
        if let Some(question) = self.extension_questions.borrow().open.values().next() {
            return Some(self.extension_prompt(question, engine));
        }
        let downloads = self.downloads.borrow();
        let (id, title, permission) =
            if let Some((&id, (origin, rights))) = self.permissions.first_key_value() {
                (
                    id,
                    format!("{origin} requests {rights}. Expires in 60 seconds."),
                    true,
                )
            } else if let Some((&id, (url, name))) = downloads.prompts.first_key_value() {
                (
                    id,
                    format!("Download {name} from {url}? Save in a new folder in Downloads."),
                    false,
                )
            } else {
                return None;
            };
        drop(downloads);
        let mut row = Row::new();
        for (label, allow) in [
            (if permission { "Allow" } else { "Save" }, true),
            ("Deny", false),
        ] {
            let engine = engine.clone();
            row = row.push(Box::new(
                Button::new(
                    &format!("browser.decision.{permission}.{id}.{allow}"),
                    label,
                )
                .control_size(ui::widget::ControlSize::Regular)
                .on_effect(move || {
                    engine.send(if permission {
                        WebViewCommand::PermissionDecision { id, allow }
                    } else {
                        WebViewCommand::DownloadDecision { id, allow }
                    });
                }),
            ));
        }
        Some(Box::new(
            Container::new().with_background().with_child(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .push(Box::new(Text::new(&title).with_role(ui::TextRole::Body)))
                    .push(Box::new(row)),
            )),
        ))
    }
    /// An extension's question: what it could then do, and the same two
    /// answers a site's question has.
    fn extension_prompt(
        &self,
        question: &ExtensionQuestion,
        engine: &SharedEngine,
    ) -> Box<dyn Widget> {
        let request = question.request;
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Sibling)
            .push(Box::new(
                Text::new(&crate::CATALOG.t_args(
                    "browser-extension-asks",
                    &[("name", ui::i18n::ArgValue::Text(question.name.clone()))],
                ))
                .with_role(ui::TextRole::Body),
            ));
        for line in &question.lines {
            column = column.push(Box::new(
                Text::new(line).with_role(TextRole::Caption).dimmed(),
            ));
        }
        let mut row = Row::new();
        for (label, allow) in [("Allow", true), ("Deny", false)] {
            let engine = engine.clone();
            let questions = self.extension_questions.clone();
            row = row.push(Box::new(
                Button::new(
                    &format!("browser.decision.extension.{request}.{allow}"),
                    label,
                )
                .control_size(ui::widget::ControlSize::Regular)
                .on_effect(move || questions.borrow_mut().answer(&engine, request, allow)),
            ));
        }
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(column.push(Box::new(row)))),
        )
    }
    /// Whether the question on offer is a permission request, a site's or an
    /// extension's, rather than a file the page offers.
    pub fn prompt_is_permission(&self) -> bool {
        !self.permissions.is_empty() || self.asks_for_extension()
    }
    /// An extension asked to open the engine's download `id`, or show it in
    /// its folder (`chrome.downloads.open`, `show`), as the person's Open and
    /// Show in Files do. The engine asks only for a complete one; the file
    /// it was saved to must still be there. Whether it was opened.
    pub fn open_for_extension(&self, id: u32, reveal: bool) -> bool {
        let downloads = self.downloads.borrow();
        let Some(path) = downloads
            .records
            .get(&id)
            .filter(|download| download.state == "complete" && !download.path.is_empty())
            .map(|download| std::path::PathBuf::from(&download.path))
        else {
            tracing::warn!("an extension's download {id} was not opened: it is not complete here");
            return false;
        };
        drop(downloads);
        launch_download(&path, reveal, &self.notice.writer());
        true
    }
    /// The person dismissed the question on offer: a permission is set aside
    /// unanswered, so the site may ask again; an extension's request and a
    /// download are declined.
    pub fn decline_prompt(&self, engine: &SharedEngine) {
        let extension = self
            .extension_questions
            .borrow()
            .open
            .keys()
            .next()
            .copied();
        if let Some(request) = extension {
            self.extension_questions
                .borrow_mut()
                .answer(engine, request, false);
        } else if let Some((&id, _)) = self.permissions.first_key_value() {
            engine.send(WebViewCommand::PermissionDismissed { id });
        } else if let Some((&id, _)) = self.downloads.borrow().prompts.first_key_value() {
            engine.send(WebViewCommand::DownloadDecision { id, allow: false });
        }
    }
    #[cfg(test)]
    pub fn downloads(&self, engine: &SharedEngine, theme: &ui::theme::Theme) -> Box<dyn Widget> {
        self.downloads_filtered(engine, theme, "")
    }
    pub fn downloads_filtered(
        &self,
        engine: &SharedEngine,
        theme: &ui::theme::Theme,
        query: &str,
    ) -> Box<dyn Widget> {
        let downloads = self.downloads.borrow();
        let mut column = Column::new().with_no_padding().with_intrinsic_height();
        column = column.push(Box::new(
            Text::from_state(&self.notice, Clone::clone)
                .with_id("browser.download.notice")
                .with_role(TextRole::Body),
        ));
        if !downloads.terminal_notice.is_empty() {
            column = column.push(Box::new(
                Text::new(&downloads.terminal_notice)
                    .with_id("browser.download.terminal-status")
                    .with_role(TextRole::Caption)
                    .dimmed(),
            ));
        }
        if downloads.records.is_empty() {
            drop(downloads);
            return Box::new(column.push(Box::new(
                EmptyState::new("No downloads yet").with_icon(Icon::Download),
            )));
        }
        let mut rows: Vec<Box<dyn Widget>> = Vec::new();
        for (&id, d) in downloads.records.iter().rev().filter(|(_, item)| {
            crate::rows::library_filter_matches(
                query,
                &[&item.name, &item.url, &item.path, &item.state],
            )
        }) {
            let status = match d.state.as_str() {
                "progress" if d.total > 0 => format!(
                    "{} of {}",
                    ui::format::size(d.received),
                    ui::format::size(d.total)
                ),
                "progress" => format!("{} downloaded", ui::format::size(d.received)),
                "complete" => format!("Saved to {}", d.path),
                "cancelled" => "Cancelled".to_owned(),
                _ if d.error.is_empty() => "The download failed".to_owned(),
                _ => d.error.clone(),
            };
            let mut row = ListRow::new(format!("browser.download.row.{id}"), &d.name);
            let mut buttons = Vec::new();
            let mut bar = None;
            if d.state == "progress" {
                let engine = engine.clone();
                bar = Some(crate::rows::transfer_bar(
                    d.received,
                    (d.total > 0).then_some(d.total),
                ));
                buttons.push(
                    Button::new(&format!("browser.download.cancel.{id}"), "Cancel").on_effect(
                        move || {
                            engine.send(WebViewCommand::CancelNativeDownload { id });
                        },
                    ),
                );
            }
            if d.state == "complete" && !d.path.is_empty() {
                for (name, label, reveal) in
                    [("open", "Open", false), ("reveal", "Show in Files", true)]
                {
                    let path = std::path::PathBuf::from(&d.path);
                    let notice = self.notice.writer();
                    buttons.push(
                        Button::new(&format!("browser.download.{name}.{id}"), label)
                            .on_effect(move || launch_download(&path, reveal, &notice)),
                    );
                }
            }
            if matches!(d.state.as_str(), "failed" | "cancelled") {
                if d.retryable {
                    let engine = engine.clone();
                    buttons.push(
                        Button::new(&format!("browser.download.retry.{id}"), "Retry").on_effect(
                            move || {
                                engine.send(WebViewCommand::RetryDownload { id });
                            },
                        ),
                    );
                } else {
                    row = row.meta("Can’t retry");
                }
            }
            row = row.secondary(status);
            let line = crate::rows::with_buttons(row, buttons);
            rows.push(match bar {
                Some(bar) => crate::rows::with_progress(
                    line,
                    bar.with_label(format!("Downloading {}", d.name)),
                    theme,
                ),
                None => Box::new(line),
            });
        }
        drop(downloads);
        if rows.is_empty() {
            return Box::new(column.push(Box::new(
                EmptyState::new("No matching items").with_icon(Icon::Search),
            )));
        }
        Box::new(column.push(crate::rows::separated(rows, theme)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::HostDelivery as _;
    fn contains(widget: &dyn Widget, id: &str) -> bool {
        ui::widget::find_test_id(widget, id).is_some()
    }
    #[test]
    fn an_extensions_question_is_asked_first_and_answered_once() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut capabilities = Capabilities::default();
        capabilities.apply(WebViewEvent::PermissionRequested {
            id: 1,
            origin: "https://site.test".into(),
            permissions: "camera".into(),
        });
        for request in [4, 5] {
            capabilities.ask_for_extension(ExtensionQuestion {
                request,
                name: "Permit".into(),
                lines: vec!["Read your browsing history".into()],
            });
        }
        assert!(capabilities.extension_questions_changed());
        assert!(!capabilities.extension_questions_changed());
        assert!(capabilities.asks_for_extension() && capabilities.prompt_is_permission());
        // Its question comes before the site's, and an agent or a screen
        // reader answers it as the person does.
        let mut prompt = capabilities.prompt(&engine).unwrap();
        assert!(contains(
            prompt.as_ref(),
            "browser.decision.extension.4.true"
        ));
        assert!(!contains(prompt.as_ref(), "browser.decision.true.1.true"));
        prompt
            .start_encoded(
                ui::tilo_actions::OperationKind::Effect,
                "tilo.ui.control.invoke",
                &serde_json::to_vec("browser.decision.extension.4.true").unwrap(),
                &mut automerge::AutoCommit::new(),
                &automerge::ROOT,
            )
            .unwrap();
        assert_eq!(
            capture.commands(),
            [WebViewCommand::ExtensionPermissionDecision {
                request: 4,
                allow: true
            }]
        );
        assert!(capabilities.extension_questions_changed());
        // Dismissed, the next is declined; then the site's is on offer.
        capabilities.decline_prompt(&engine);
        assert_eq!(
            capture.commands().last(),
            Some(&WebViewCommand::ExtensionPermissionDecision {
                request: 5,
                allow: false
            })
        );
        assert!(!capabilities.asks_for_extension());
        let prompt = capabilities.prompt(&engine).unwrap();
        assert!(contains(prompt.as_ref(), "browser.decision.true.1.true"));
        // An answered question is answered once.
        capabilities
            .extension_questions
            .borrow_mut()
            .answer(&engine, 5, true);
        assert_eq!(capture.commands().len(), 2);
    }
    #[test]
    fn an_extensions_question_whose_page_closed_goes_unanswered() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut capabilities = Capabilities::default();
        for request in [4, 5] {
            capabilities.ask_for_extension(ExtensionQuestion {
                request,
                name: "Permit".into(),
                lines: vec!["Read your browsing history".into()],
            });
        }
        assert!(capabilities.extension_questions_changed());
        // Its popup closed itself before the person answered: the engine
        // told the extension no, and its question is no longer on offer.
        capabilities.apply(WebViewEvent::ExtensionPermissionClosed { request: 4 });
        assert!(capabilities.extension_questions_changed());
        let prompt = capabilities.prompt(&engine).unwrap();
        assert!(!contains(
            prompt.as_ref(),
            "browser.decision.extension.4.true"
        ));
        assert!(contains(
            prompt.as_ref(),
            "browser.decision.extension.5.true"
        ));
        // A question the window no longer holds changes nothing.
        capabilities.apply(WebViewEvent::ExtensionPermissionClosed { request: 4 });
        assert!(!capabilities.extension_questions_changed());
        capabilities.apply(WebViewEvent::ExtensionPermissionClosed { request: 5 });
        assert!(!capabilities.asks_for_extension());
        assert!(capabilities.prompt(&engine).is_none());
        // Nothing was answered for the person.
        assert!(capture.commands().is_empty());
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn download_records_outlive_origin_and_offer_host_actions_when_complete() {
        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture.clone();
            let mut capabilities = Capabilities::default();
            capabilities.apply(WebViewEvent::PermissionRequested {
                id: 1,
                origin: "https://site.test".into(),
                permissions: "camera".into(),
            });
            assert!(capabilities.prompt(&engine).is_some());
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 7,
                state: "progress".into(),
                path: "/tmp/file".into(),
                name: "file".into(),
                url: "https://downloads.test/file".into(),
                retryable: true,
                received: 10,
                total: 20,
                error: String::new(),
            });
            assert!(capabilities.downloads_changed());
            let mut rows = capabilities.downloads(&engine, &ui::Theme::dark());
            assert!(contains(rows.as_ref(), "browser.download.cancel.7"));
            let result = rows
                .start_encoded(
                    ui::tilo_actions::OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.download.cancel.7").unwrap(),
                    &mut automerge::AutoCommit::new(),
                    &automerge::ROOT,
                )
                .unwrap();
            assert!(result.is_some());
            assert!(
                capture
                    .commands()
                    .contains(&WebViewCommand::CancelNativeDownload { id: 7 })
            );
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 7,
                state: "complete".into(),
                path: "/tmp/file".into(),
                name: "file".into(),
                url: "https://downloads.test/file".into(),
                retryable: true,
                received: 20,
                total: 20,
                error: String::new(),
            });
            assert!(contains(
                capabilities.downloads(&engine, &ui::Theme::dark()).as_ref(),
                "browser.download.open.7"
            ));
            for id in 8..150 {
                capabilities.apply(WebViewEvent::DownloadUpdated {
                    id,
                    state: "complete".into(),
                    path: "/tmp/file".into(),
                    name: "file".into(),
                    url: "https://downloads.test/file".into(),
                    retryable: true,
                    received: 20,
                    total: 20,
                    error: String::new(),
                });
            }
            assert_eq!(capabilities.downloads.borrow().records.len(), 100);
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("download.txt");
            std::fs::write(&path, b"done").unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_launch(ui::portal::LaunchOutcome::Failed("no handler".into()));
            let opened = open_download(&path, false).unwrap();
            assert_eq!(
                opened.recv(),
                Ok(ui::portal::LaunchOutcome::Failed("no handler".into()))
            );
            let revealed = open_download(&path, true).unwrap();
            assert_eq!(revealed.recv(), Ok(ui::portal::LaunchOutcome::Opened));
            let requests = recorder.launch_requests();
            assert_eq!(requests.len(), 2);
            assert!(!requests[0].is_reveal() && requests[1].is_reveal());
            assert_eq!(requests[0].path(), path.as_path());
            assert!(
                open_download(&dir.path().join("missing.txt"), false).is_err(),
                "a vanished download never reaches the host"
            );
            assert_eq!(recorder.launch_requests().len(), 2);
        }

        {
            let engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
            let mut capabilities = Capabilities::default();
            for state in ["progress", "failed", "complete"] {
                capabilities.apply(WebViewEvent::DownloadUpdated {
                    id: 7,
                    state: state.into(),
                    path: "/tmp/test-download.txt".into(),
                    name: "test".into(),
                    url: "https://downloads.test/file".into(),
                    retryable: true,
                    received: 8,
                    total: 8,
                    error: String::new(),
                });
                let view = capabilities.downloads(&engine, &ui::Theme::dark());
                assert_eq!(
                    contains(view.as_ref(), "browser.download.open.7"),
                    state == "complete"
                );
                assert_eq!(
                    contains(view.as_ref(), "browser.download.reveal.7"),
                    state == "complete"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn download_progress_aggregates_and_runtime_disconnect_fails_truthfully() {
        {
            let mut capabilities = Capabilities::default();
            for id in 1..=4 {
                capabilities.apply(WebViewEvent::DownloadUpdated {
                    id,
                    state: "progress".into(),
                    path: "/tmp/large-download".into(),
                    name: "large".into(),
                    url: "https://downloads.test/large".into(),
                    retryable: true,
                    received: 1_u64 << 61,
                    total: 1_u64 << 62,
                    error: String::new(),
                });
            }
            assert_eq!(
                capabilities.toolbar_status().as_deref(),
                Some("4 downloads at 50%")
            );
        }

        {
            let capture = ui::webview::test_engine::TestEngine::new();
            let engine: SharedEngine = capture.clone();
            let mut capabilities = Capabilities::default();
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 41,
                state: "progress".into(),
                path: "/tmp/archive.zip".into(),
                name: "archive.zip".into(),
                url: "https://downloads.test/archive.zip".into(),
                retryable: true,
                received: 256,
                total: 1024,
                error: String::new(),
            });
            assert_eq!(
                capabilities.toolbar_status().as_deref(),
                Some("1 download at 25%")
            );

            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 41,
                state: "failed".into(),
                path: String::new(),
                name: String::new(),
                url: String::new(),
                retryable: false,
                received: 0,
                total: 0,
                error: "connection reset".into(),
            });
            let mut rows = capabilities.downloads(&engine, &ui::Theme::dark());
            assert!(contains(rows.as_ref(), "browser.download.retry.41"));
            let result = rows
                .start_encoded(
                    ui::tilo_actions::OperationKind::Effect,
                    "tilo.ui.control.invoke",
                    &serde_json::to_vec("browser.download.retry.41").unwrap(),
                    &mut automerge::AutoCommit::new(),
                    &automerge::ROOT,
                )
                .unwrap();
            assert!(result.is_some());
            assert!(
                capture
                    .commands()
                    .contains(&WebViewCommand::RetryDownload { id: 41 })
            );
            assert_eq!(
                capabilities.toolbar_status().as_deref(),
                Some("Download failed")
            );
        }

        {
            let engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
            let mut capabilities = Capabilities::default();
            capabilities.apply(WebViewEvent::DownloadUpdated {
                id: 7,
                state: "progress".into(),
                path: "/tmp/file".into(),
                name: "file".into(),
                url: "https://downloads.test/file".into(),
                retryable: true,
                received: 10,
                total: 20,
                error: String::new(),
            });
            capabilities.apply(WebViewEvent::DownloadRuntime {
                admitted: 1,
                detached: 1,
                accepting: true,
                error: String::new(),
            });
            assert!(capabilities.exit_requires_choice());
            capabilities.apply(WebViewEvent::DownloadRuntime {
                admitted: 0,
                detached: 1,
                accepting: false,
                error: String::new(),
            });
            assert!(!capabilities.exit_cleanup_complete());
            capabilities.apply(WebViewEvent::DownloadRuntime {
                admitted: 0,
                detached: 0,
                accepting: false,
                error: "Chromium engine disconnected".into(),
            });
            assert!(capabilities.exit_cleanup_complete());
            assert!(contains(
                capabilities.downloads(&engine, &ui::Theme::dark()).as_ref(),
                "browser.download.terminal-status"
            ));
        }
    }

    #[test]
    fn an_extension_opens_only_a_complete_download_the_engine_reported() {
        let mut capabilities = Capabilities::default();
        let update = |id, state: &str| WebViewEvent::DownloadUpdated {
            id,
            state: state.into(),
            // Gone since: nothing is launched, the notice says so.
            path: "/nonexistent/tilo-extension-download".into(),
            name: "report.csv".into(),
            url: "https://downloads.test/report.csv".into(),
            retryable: true,
            received: 10,
            total: 20,
            error: String::new(),
        };
        capabilities.apply(update(3, "progress"));
        capabilities.apply(update(4, "complete"));
        assert!(!capabilities.open_for_extension(3, false));
        assert!(!capabilities.open_for_extension(9, true));
        assert!(capabilities.open_for_extension(4, false));
        assert!(capabilities.open_for_extension(4, true));
    }
}
