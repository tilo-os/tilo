//! The page a window shows, one request at a time, with no UI of its own.
//!
//! A window holds one engine and one [`PageSession`]: the session asks the engine to do what the
//! page's actions say (`PageRequest`), keeps the answers until the frame that carries them back
//! to whoever asked, and takes every other event the engine reports into a queue the view
//! reads. The engine's events have one reader — this session — so a page's navigation and its
//! answers never take each other's.

pub mod codec;

mod asks;
mod logins;
pub mod omnibox;
mod prompts;

pub use asks::{Asks, ExtensionButton, ExtensionNotice, Menu};
pub use logins::{Answered, LoginAsk, LoginVault, VaultLogin};
pub use omnibox::Addressed;
pub use prompts::{Prompt, PromptChange, PromptKey, PromptKind, Prompts};

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::mpsc;

use logins::VaultAnswer;

use tilo_webview::{
    CredentialAction, CredentialRequest, ExtensionSidePanelChange, FileChooser, PageActor,
    PageFailure, PageReply, PageRequest, SharedEngine, WebViewCommand, WebViewEvent,
};

/// The most sign-ins loaded out of sight at once: the pages of an extension's sign-in a private
/// window shows nobody.
const QUIET_SIGN_INS: usize = 4;

/// What the page shown beside the window's own is: whose page it is, where it shows, and what
/// for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelInfo {
    /// `side` for an extension's side panel, `sign-in` for the page it asked the person to sign
    /// in on, `popup` for a page a script opened in a window the site sized.
    pub kind: String,
    /// The engine's identifier of the extension whose page it is.
    pub id: String,
    /// The address the page shows.
    pub url: String,
    /// Whether the person is shown it: a sign-in they may not be asked on loads out of sight.
    pub interactive: bool,
}

/// An extension's page beside the window's page (`chrome.sidePanel`), or the sign-in page it
/// asked the person to sign in on: a page of its own, in a second engine of the window's, so
/// the page the person browsed stays where it is.
struct PanelPage {
    engine: SharedEngine,
    info: PanelInfo,
}

/// One app window's page: its engine, the actions waiting for an answer, and the events its
/// view shows.
pub struct PageSession {
    engine: SharedEngine,
    /// The id of the last page action asked about.
    next: u32,
    /// The actions whose answers have not come.
    waiting: BTreeSet<u32>,
    /// The answers that came and wait to be carried to whoever asked.
    answers: Vec<(u32, Result<PageReply, PageFailure>)>,
    /// The engine's other events, in the order they came, for the view to show.
    events: VecDeque<WebViewEvent>,
    /// What the page waits on and last reported of itself, as its overlays and the app's records
    /// read it: a dialog, a sign-in, a permission, a held request, its context menu.
    asks: Asks,
    /// What the page waits on a person's yes for, as the window raises and withdraws it.
    prompts: Prompts,
    /// The page's file choosers a person's click opened, waiting for the host's picker: the
    /// window answers each and the page is never left waiting on one.
    choosers: Vec<FileChooser>,
    /// What the page says of itself that the app's own record carries (a download saved, the
    /// pages it can go to), for the window to hand the app as its calls: the record moves while
    /// the person looks.
    notes: Vec<String>,
    /// The number this window keeps a download's request under ([`tilo_engine_loader::kept`]),
    /// so what it never handed over goes when it closes.
    keeper: u64,
    /// The downloads the person retries, waiting on what a request for their address carries
    /// from this page now: the job each ended as, by the number the engine was asked under.
    again: HashMap<u32, String>,
    /// The engine's number for the next download asked again.
    again_next: u32,
    /// What the window last told the app of each download an extension started, which the
    /// engine saves: its state, and how far it came in tenths.
    extension_downloads: HashMap<u32, (String, u64)>,
    /// The engine's number for the next credential action.
    credential_next: u32,
    /// The engine's number for the next ask about the video the page shows.
    media_next: u64,
    /// A login action waiting for the page's current document and site: the engine acts only on
    /// the document the person is looking at, and the vault is asked only for the site the
    /// engine says the page is at, so the session asks for both first.
    login_waiting: Option<LoginAsk>,
    /// The takes the engine was asked for, by the session's number for each: what keeping the
    /// password it hands over is for, and the login it replaces.
    taking: HashMap<u32, (&'static str, Option<String>)>,
    /// The workspace's vault, which keeps the page's logins: none in a private window.
    vault: Option<Rc<dyn LoginVault>>,
    /// What the vault answered, from whichever thread it answered on.
    vault_answers: (mpsc::Sender<VaultAnswer>, mpsc::Receiver<VaultAnswer>),
    /// How many vault calls have not answered yet.
    vault_pending: usize,
    /// A private page's disposable root, which this session's engine started on and which goes
    /// when the session does: nothing a private window read or signed in to is kept.
    private_root: Option<std::path::PathBuf>,
    /// The page an extension showed beside this one, or the one it asked the person to sign in
    /// on: the window's second engine, and what it shows.
    panel: Option<PanelPage>,
    /// Sign-ins an extension may not ask the person about, loading out of sight: their pages go
    /// once the engine closes them, and nothing of them is shown.
    quiet_sign_ins: Vec<SharedEngine>,
    /// What the person types after an extension's keyword in the address, and what the extension
    /// says of it.
    omnibox: omnibox::Omnibox,
}

impl PageSession {
    #[must_use]
    pub fn new(engine: SharedEngine) -> Self {
        Self {
            engine,
            next: 0,
            waiting: BTreeSet::new(),
            answers: Vec::new(),
            events: VecDeque::new(),
            asks: Asks::default(),
            prompts: Prompts::default(),
            choosers: Vec::new(),
            notes: Vec::new(),
            keeper: tilo_engine_loader::kept::owner(),
            again: HashMap::new(),
            again_next: 0,
            extension_downloads: HashMap::new(),
            credential_next: 0,
            media_next: 0,
            login_waiting: None,
            taking: HashMap::new(),
            vault: None,
            vault_answers: mpsc::channel(),
            vault_pending: 0,
            private_root: None,
            panel: None,
            quiet_sign_ins: Vec::new(),
            omnibox: omnibox::Omnibox::default(),
        }
    }

    /// The page of a window opened private: `engine` is already started on `root`, a fresh
    /// profile that holds nothing the person signed in to, and this session removes it when the
    /// window closes.
    #[must_use]
    pub fn new_private(engine: SharedEngine, root: std::path::PathBuf) -> Self {
        let mut session = Self::new(engine);
        session.private_root = Some(root);
        session
    }

    /// Whether this page is private: nothing it reads or signs in to is kept, and its history is
    /// kept nowhere.
    #[must_use]
    pub fn is_private(&self) -> bool {
        self.private_root.is_some()
    }

    /// Keep this page's logins in `vault`. A private page keeps none and is offered none.
    pub fn set_vault(&mut self, vault: Rc<dyn LoginVault>) {
        if !self.is_private() {
            self.vault = Some(vault);
        }
    }

    /// The engine this window shows its page with.
    #[must_use]
    pub fn engine(&self) -> &SharedEngine {
        &self.engine
    }

    /// Ask the page to do something as `actor` (`None` for the one unnamed actor a window has
    /// always had), and answer its id: the answer comes in [`Self::poll`] and [`Self::answers`].
    pub fn ask(&mut self, actor: Option<PageActor>, request: PageRequest) -> u32 {
        let request_id = self.next.wrapping_add(1);
        self.next = request_id;
        self.waiting.insert(request_id);
        self.engine.send(WebViewCommand::Page {
            request_id,
            actor,
            request,
        });
        request_id
    }

    /// Stop waiting for `action`: whoever asked gave up, and the page must not hold the next
    /// action behind this one.
    pub fn cancel(&mut self, action: u32) {
        if self.waiting.remove(&action) {
            self.engine
                .send(WebViewCommand::CancelPage { request_id: action });
        }
    }

    /// Take what the engine reported: the answers to the actions asked, and the page's other
    /// events for the view. A window calls this once a frame, before it draws.
    #[expect(clippy::too_many_lines, reason = "one arm per kind of page event")]
    pub fn poll(&mut self) {
        let mut loaded = false;
        for event in self.engine.drain_events() {
            match &event {
                // A document finished loading: what video it shows is asked for once it is
                // there, so the app can offer to float it and the window has it to hand.
                WebViewEvent::LoadFinished | WebViewEvent::LoadFinishedAt { .. } => loaded = true,
                WebViewEvent::PageResult { request_id, result } => {
                    // An answer to an action cancelled meanwhile is dropped: nobody waits for
                    // it, and the page settled it as asked.
                    if self.waiting.remove(request_id) {
                        self.answers.push((*request_id, result.clone()));
                    }
                    continue;
                }
                // A page's file chooser is answered by the host's picker, never shown as the
                // page's own event: the window takes it from here and asks the person.
                WebViewEvent::FileChooser(chooser) => {
                    self.choosers.push(chooser.clone());
                    continue;
                }
                WebViewEvent::FileChooserClosed { id } => {
                    self.choosers.retain(|chooser| chooser.id != *id);
                    continue;
                }
                // A download the person saved is the app's own to fetch: the window keeps what
                // its request carried in this process, and the app's call names only the handle
                // the download redeems once as it starts. Cookies never enter a record.
                WebViewEvent::DownloadRequest {
                    id,
                    url,
                    name,
                    referrer,
                    cookies,
                    user_agent,
                    again,
                } => {
                    // One asked again names the job of the download the person retries; one
                    // nobody here asked for is dropped with its cookies.
                    let retried = if *again {
                        match self.again.remove(id) {
                            Some(job) => Some(job),
                            None => continue,
                        }
                    } else {
                        None
                    };
                    let context = serde_json::json!({
                        "referrer": referrer,
                        "cookies": cookies,
                        "user_agent": user_agent,
                    });
                    let request = tilo_engine_loader::kept::keep(self.keeper, context.to_string());
                    self.notes.push(codec::download_note(
                        url,
                        name,
                        &request,
                        retried.as_deref(),
                    ));
                    continue;
                }
                // A download an extension started is the engine's to save and the extension's to
                // follow, and the Browser's list shows it too: the window tells the app as its
                // state changes and each tenth of the way.
                WebViewEvent::DownloadUpdated {
                    id,
                    state,
                    name,
                    url,
                    path,
                    received,
                    total,
                    error,
                    extension: true,
                    ..
                } => {
                    let step = received.saturating_mul(10).checked_div(*total).unwrap_or(0);
                    let said = (state.clone(), step);
                    if self.extension_downloads.insert(*id, said.clone()) != Some(said) {
                        self.notes
                            .push(codec::extension_download_note(&codec::SeenDownload {
                                id: *id,
                                state,
                                name,
                                url,
                                path,
                                received: *received,
                                total: *total,
                                error,
                            }));
                    }
                }
                // The video the page shows appeared or went with its page: the app's record
                // carries what it offers to float, and it moves while the person looks.
                WebViewEvent::MediaElement { .. }
                | WebViewEvent::MediaElementUnavailable { .. } => {
                    self.notes.push(codec::media_note());
                }
                WebViewEvent::NavigationHistory {
                    direction, entries, ..
                } => {
                    self.notes.push(codec::history_note(*direction, entries));
                }
                // What is installed and what installing, turning on or removing came to: the
                // app's record's, never the page's own event.
                WebViewEvent::ExtensionList {
                    installed, message, ..
                } => {
                    self.notes.push(codec::extensions_note(installed, message));
                    continue;
                }
                // A press on an extension's toolbar button that opens a page of the extension's
                // own is a popup: the app keeps it as a page of the browser.
                WebViewEvent::ExtensionOpenPopup { url, .. } => {
                    self.notes.push(codec::open_note(url, true));
                    continue;
                }
                // An extension asks for its side panel beside this page: the window shows it in
                // its second engine, which the extension's page is told of first.
                WebViewEvent::ExtensionSidePanel { id, change } => {
                    if self.side_panel_changed(id, change) {
                        let info = self.panel_info();
                        self.notes.push(codec::panel_note(info.as_ref()));
                    }
                    continue;
                }
                // It asks the person to sign in: the window shows the page in its second engine,
                // which the host watches for where the sign-in ends, as it watches a tab it
                // bound. One that may not ask them loads out of sight, as in Chrome.
                WebViewEvent::ExtensionSignIn {
                    id,
                    url,
                    interactive,
                    flow,
                } => {
                    self.sign_in_asked(id, url, *interactive, *flow);
                    continue;
                }
                // What the address offers the extension it addresses: the app's record's, never
                // the page's own event.
                WebViewEvent::ExtensionOmniboxSuggestions { .. }
                | WebViewEvent::ExtensionOmniboxDefault { .. } => {
                    if self.omnibox.apply(&event) {
                        self.note_omnibox();
                    }
                    continue;
                }
                WebViewEvent::ExtensionOutcome {
                    ok,
                    kind,
                    message,
                    id,
                    ..
                } => {
                    self.notes
                        .push(codec::extension_outcome_note(*ok, kind, message, id));
                    continue;
                }
                // What asking the store for newer versions came to: the app's record's too.
                WebViewEvent::ExtensionUpdates {
                    checked,
                    updated,
                    failed,
                    ..
                } => {
                    self.notes
                        .push(codec::extension_updates_note(*checked, updated, failed));
                }
                // The page written as a PDF: where it went is the app's to hand on, never the
                // page's own event.
                WebViewEvent::PrintResult {
                    ok,
                    path,
                    name,
                    message,
                    ..
                } => {
                    self.notes.push(codec::print_note(*ok, path, name, message));
                    continue;
                }
                // How the engine answered a login action: the forms it found, the capture it
                // took, a fill, or the status the session asked to learn the page's document and
                // site, which is not the person's to see: their own action follows with them.
                WebViewEvent::CredentialResult(reply) => self.engine_answered(reply),
                // The page came into or out of fullscreen: its window fills the screen while it
                // is in it and shows the way out, which the app's record keeps.
                WebViewEvent::FullscreenChanged(on) if self.asks.fullscreen != *on => {
                    self.notes.push(codec::fullscreen_note(*on));
                }
                _ => {}
            }
            self.asks.apply(&event);
            self.prompts.apply(&event);
            self.events.push_back(event);
        }
        if loaded {
            self.media_next = self.media_next.wrapping_add(1);
            self.engine.send(WebViewCommand::RequestMediaElement {
                request_id: self.media_next,
            });
        }
        // Pages a script opened: a plain new tab is the app's page, which the system shows in a
        // window of its own, and a popup the site sized is adopted into the window's second
        // engine and shown beside the page, as the deleted app showed one.
        while let Ok(answer) = self.vault_answers.1.try_recv() {
            self.vault_pending = self.vault_pending.saturating_sub(1);
            self.vault_answered(answer);
        }
        for (url, _foreground) in self.engine.take_new_tab_requests() {
            self.notes.push(codec::open_note(&url, false));
        }
        self.engine.accept_popups();
        for popup in self.engine.take_popups() {
            match self.engine.adopt_popup(popup.id) {
                Ok(engine) => {
                    self.panel = Some(PanelPage {
                        engine,
                        info: PanelInfo {
                            kind: "popup".to_owned(),
                            id: popup.id.to_string(),
                            url: popup.url,
                            interactive: true,
                        },
                    });
                    let info = self.panel_info();
                    self.notes.push(codec::panel_note(info.as_ref()));
                }
                // A window that cannot hold the page beside it still keeps it as one.
                Err(_) => self.notes.push(codec::open_note(&popup.url, true)),
            }
        }
        // The window's second page, and the sign-in pages nobody is shown: what they said since
        // the last frame, and whether they are still there at all.
        self.poll_panel();
        self.poll_quiet_sign_ins();
    }

    /// What the window's second page became: where it went, or that it went (its sign-in ended,
    /// its engine closed it). A page that is gone is taken out of the window's, and the app is
    /// told through [`Self::notes`].
    fn poll_panel(&mut self) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let mut url = None;
        let mut closed = false;
        for event in panel.engine.drain_events() {
            match event {
                WebViewEvent::TabClosed | WebViewEvent::TabDetached => closed = true,
                WebViewEvent::UriChanged(uri) => url = Some(uri),
                _ => {}
            }
        }
        if closed {
            self.panel = None;
            self.notes.push(codec::panel_note(None));
            return;
        }
        let changed = match self.panel.as_mut() {
            Some(panel) if url.as_deref().is_some_and(|url| panel.info.url != *url) => {
                panel.info.url = url.unwrap_or_default();
                Some(panel.info.clone())
            }
            _ => None,
        };
        if let Some(info) = changed {
            self.notes.push(codec::panel_note(Some(&info)));
        }
    }

    /// The sign-ins nobody is shown: one whose page the engine closed goes with it.
    fn poll_quiet_sign_ins(&mut self) {
        let mut kept = Vec::with_capacity(self.quiet_sign_ins.len());
        for engine in self.quiet_sign_ins.drain(..) {
            let closed = engine
                .drain_events()
                .iter()
                .any(|event| matches!(event, WebViewEvent::TabClosed | WebViewEvent::TabDetached));
            if !closed {
                kept.push(engine);
            }
        }
        self.quiet_sign_ins = kept;
    }

    /// What the window's second page shows, when one is open.
    #[must_use]
    pub fn panel(&self) -> Option<&PanelInfo> {
        self.panel.as_ref().map(|panel| &panel.info)
    }

    /// The engine of the window's second page (an extension's side panel, a sign-in page, a
    /// popup a script opened), when one is open: the page the window draws beside or over the
    /// one the person browsed.
    #[must_use]
    pub fn panel_engine(&self) -> Option<SharedEngine> {
        self.panel.as_ref().map(|panel| panel.engine.clone())
    }

    fn panel_info(&self) -> Option<PanelInfo> {
        self.panel.as_ref().map(|panel| panel.info.clone())
    }

    /// The extension asked for its side panel beside this page: show it in the window's second
    /// engine, or put it where it went or take it away. Whether the window's page changed.
    fn side_panel_changed(&mut self, id: &str, change: &ExtensionSidePanelChange) -> bool {
        let shows = self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.info.kind == "side" && panel.info.id == id);
        let url = match change {
            ExtensionSidePanelChange::Open(url) => url.clone(),
            ExtensionSidePanelChange::Toggle(url) if !shows => url.clone(),
            ExtensionSidePanelChange::Options(Some(url)) if shows => url.clone(),
            ExtensionSidePanelChange::Toggle(_)
            | ExtensionSidePanelChange::Options(None)
            | ExtensionSidePanelChange::Close
                if shows =>
            {
                self.panel = None;
                return true;
            }
            _ => return false,
        };
        if let Some(panel) = self
            .panel
            .as_mut()
            .filter(|panel| panel.info.kind == "side" && panel.info.id == id)
        {
            if panel.info.url != url {
                panel.engine.send(WebViewCommand::Navigate(url.clone()));
                panel.info.url = url;
                return true;
            }
            return false;
        }
        let Ok(engine) = self.engine.create_tab() else {
            return false;
        };
        // Its "current tab" is the page beside it, which the extension may script as its own
        // page when it asked for the panel.
        engine.send(WebViewCommand::ExtensionSidePanelPage { id: id.to_owned() });
        engine.send(WebViewCommand::Navigate(url.clone()));
        self.panel = Some(PanelPage {
            engine,
            info: PanelInfo {
                kind: "side".to_owned(),
                id: id.to_owned(),
                url,
                interactive: true,
            },
        });
        true
    }

    /// The extension asked the person to sign in: show the page in the window's second engine,
    /// which the host watches for the address that ends the sign-in, or load it out of sight
    /// when it may not ask them. Whether the window showed one.
    fn sign_in_asked(&mut self, id: &str, url: &str, interactive: bool, flow: u32) {
        let Ok(engine) = self.engine.create_tab() else {
            return;
        };
        engine.send(WebViewCommand::ExtensionSignInPanel {
            id: id.to_owned(),
            flow,
        });
        engine.send(WebViewCommand::Navigate(url.to_owned()));
        if !interactive {
            self.quiet_sign_ins.push(engine);
            if self.quiet_sign_ins.len() > QUIET_SIGN_INS {
                self.quiet_sign_ins.remove(0);
            }
            return;
        }
        self.panel = Some(PanelPage {
            engine,
            info: PanelInfo {
                kind: "sign-in".to_owned(),
                id: id.to_owned(),
                url: url.to_owned(),
                interactive,
            },
        });
        let info = self.panel_info();
        self.notes.push(codec::panel_note(info.as_ref()));
    }

    /// What the person typed in the address: `addressed` is the extension whose keyword what is
    /// typed starts with, when it starts with one, and what follows it.
    pub fn address_typed(&mut self, text: &str, addressed: Option<&Addressed>) {
        if self.omnibox.typed(text, addressed, &self.engine) {
            self.note_omnibox();
        }
    }

    /// The person left the address without Enter: the extension hears of it.
    pub fn address_left(&mut self) {
        if self.omnibox.leave(&self.engine) {
            self.note_omnibox();
        }
    }

    /// The person pressed Enter, or chose a row: the extension hears what it was given.
    pub fn address_entered(&mut self, text: &str) {
        if let Some(id) = self.omnibox.session_id() {
            self.engine.send(WebViewCommand::ExtensionOmnibox {
                id,
                input: tilo_webview::OmniboxInput::Entered {
                    text: text.to_owned(),
                },
            });
        }
        self.omnibox.entered(text);
        self.note_omnibox();
    }

    /// Ask the page what a request for `url` carries from it now, for the download the person
    /// retries, whose job ended as `job`: the app's call that follows names that job and the
    /// handle the context is kept under, as a saved download's does.
    pub fn download_again(&mut self, url: &str, job: &str) {
        self.again_next = self.again_next.wrapping_add(1).max(1);
        self.again.insert(self.again_next, job.to_owned());
        self.engine.send(WebViewCommand::RequestDownload {
            id: self.again_next,
            uri: url.to_owned(),
        });
    }

    /// Close the page the extension showed beside this one: its side panel, or the sign-in page
    /// it asked the person to sign in on. A sign-in closed this way ends as refused, which the
    /// engine tells the extension.
    pub fn close_panel(&mut self) {
        if self.panel.take().is_some() {
            self.notes.push(codec::panel_note(None));
        }
    }

    fn note_omnibox(&mut self) {
        let state = self.omnibox.state();
        self.notes.push(codec::omnibox_note(state.as_ref()));
    }

    /// The answers that came since the last call, oldest first.
    pub fn answers(&mut self) -> Vec<(u32, Result<PageReply, PageFailure>)> {
        std::mem::take(&mut self.answers)
    }

    /// The next event of the page for its view, in the order it came.
    pub fn next_event(&mut self) -> Option<WebViewEvent> {
        self.events.pop_front()
    }

    /// Whether `action` still waits for its answer.
    #[must_use]
    pub fn waiting_for(&self, action: u32) -> bool {
        self.waiting.contains(&action)
    }

    /// What the page waits on and last reported of itself.
    #[must_use]
    pub fn asks(&self) -> &Asks {
        &self.asks
    }

    /// What the page started and stopped waiting on a person's yes for since the window last
    /// took it, in the order it changed.
    pub fn prompts(&mut self) -> Vec<PromptChange> {
        self.prompts.take()
    }

    /// Whether a prompt started or stopped waiting since the window last took them.
    #[must_use]
    pub fn has_prompts(&self) -> bool {
        self.prompts.changed()
    }

    /// The page's file choosers that started waiting since the window last took them, oldest
    /// first: the window asks the person with the host's picker and answers each.
    pub fn choosers(&mut self) -> Vec<FileChooser> {
        std::mem::take(&mut self.choosers)
    }

    /// Whether a file chooser started waiting since the window last took them.
    #[must_use]
    pub fn has_choosers(&self) -> bool {
        !self.choosers.is_empty()
    }

    /// What the page said of itself since the window last took it, oldest first, each encoded
    /// for the app (`ToApp::PagePrompt`): a download saved, the pages it can go to.
    pub fn notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Whether the page said anything of itself since the window last took it.
    #[must_use]
    pub fn has_notes(&self) -> bool {
        !self.notes.is_empty()
    }

    /// Do `ask` with the page's logins. The engine acts on the document the person is looking at
    /// and refuses one from a page they left, and the vault is asked only for the site the engine
    /// says the page is at, so the session asks the engine for both first (`origin` is the app's
    /// word for the site, which only starts that question). A password never crosses the action
    /// or its answer.
    pub fn login(&mut self, origin: &str, ask: LoginAsk) {
        if let LoginAsk::Engine(action @ (CredentialAction::Status | CredentialAction::Cancel)) =
            &ask
        {
            if matches!(action, CredentialAction::Cancel) {
                self.login_waiting = None;
                self.taking.clear();
            }
            self.send_credential(CredentialRequest {
                request_id: 0,
                origin: origin.to_owned(),
                document: String::new(),
                action: action.clone(),
            });
            return;
        }
        if !matches!(ask, LoginAsk::Engine(_)) && self.vault.is_none() {
            let why = if self.is_private() {
                logins::PRIVATE
            } else {
                logins::NO_VAULT
            };
            self.notes.push(codec::credential_note(
                ask.did(),
                false,
                why,
                &[],
                &[],
                None,
                None,
                false,
            ));
            return;
        }
        self.login_waiting = Some(ask);
        self.send_credential(CredentialRequest {
            request_id: 0,
            origin: origin.to_owned(),
            document: String::new(),
            action: CredentialAction::Status,
        });
    }

    /// The engine answered a login action.
    fn engine_answered(&mut self, reply: &tilo_webview::CredentialReply) {
        if reply.action == "status"
            && let Some(ask) = self.login_waiting.take()
        {
            if reply.document.is_empty() || !reply.success {
                self.notes.push(codec::credential_note(
                    ask.did(),
                    false,
                    &reply.message,
                    &[],
                    &[],
                    None,
                    None,
                    reply.late,
                ));
            } else {
                self.go_on(ask, &reply.origin, &reply.document);
            }
            return;
        }
        if reply.action == "take"
            && let Some((did, replacing)) = self.taking.remove(&reply.request_id)
        {
            match (&reply.password, &reply.username, &self.vault) {
                (Some(password), Some(username), Some(vault)) if reply.success => {
                    let site = reply.origin.clone();
                    let sender = self.vault_answers.0.clone();
                    self.vault_pending = self.vault_pending.saturating_add(1);
                    vault.keep(
                        &reply.origin,
                        username,
                        password.clone(),
                        replacing.as_deref(),
                        Box::new(move |result| {
                            let _ = sender.send(VaultAnswer::Kept { did, site, result });
                        }),
                    );
                }
                _ => self.notes.push(codec::credential_note(
                    did,
                    false,
                    &reply.message,
                    &[],
                    &[],
                    None,
                    None,
                    reply.late,
                )),
            }
            // Whatever the take handed over goes now: the vault took its own copy, or nothing did.
            if let Some(password) = &reply.password {
                drop(password.take());
            }
            return;
        }
        self.notes.push(codec::credential_note(
            &reply.action,
            reply.success,
            &reply.message,
            &reply.forms,
            &[],
            reply.capture.as_deref(),
            reply.username.as_deref(),
            reply.late,
        ));
    }

    /// Do `ask` now that the engine said the page is at `site`, on its document `document`.
    fn go_on(&mut self, ask: LoginAsk, site: &str, document: &str) {
        let engine = |action| CredentialRequest {
            request_id: 0,
            origin: site.to_owned(),
            document: document.to_owned(),
            action,
        };
        let Some(vault) = self.vault.clone() else {
            if let LoginAsk::Engine(action) = ask {
                self.send_credential(engine(action));
            }
            return;
        };
        let sender = self.vault_answers.0.clone();
        let listed = |did, message| -> Answered<Vec<VaultLogin>> {
            let sender = sender.clone();
            Box::new(move |result| {
                let _ = sender.send(VaultAnswer::Listed {
                    did,
                    message,
                    result,
                });
            })
        };
        match ask {
            LoginAsk::Engine(action) => {
                self.send_credential(engine(action));
            }
            LoginAsk::List => {
                self.vault_pending = self.vault_pending.saturating_add(1);
                vault.logins(site, listed("list", ""));
            }
            LoginAsk::Delete { item } => {
                self.vault_pending = self.vault_pending.saturating_add(1);
                vault.forget(site, &item, listed("delete", "Forgotten"));
            }
            LoginAsk::Save { capture } => {
                let id = self.send_credential(engine(CredentialAction::Take { capture }));
                self.taking.insert(id, ("save", None));
            }
            LoginAsk::Update { capture, item } => {
                let id = self.send_credential(engine(CredentialAction::Take { capture }));
                self.taking.insert(id, ("update", Some(item)));
            }
            LoginAsk::Fill { form, item } => {
                let (site, document) = (site.to_owned(), document.to_owned());
                self.vault_pending = self.vault_pending.saturating_add(1);
                vault.fill(
                    &site.clone(),
                    &item,
                    Box::new(move |result| {
                        let _ = sender.send(VaultAnswer::Filled {
                            form,
                            site,
                            document,
                            result,
                        });
                    }),
                );
            }
        }
    }

    /// The vault answered a call this session made.
    fn vault_answered(&mut self, answer: VaultAnswer) {
        let note = |did: &str, result: Result<&[VaultLogin], &str>, message: &str| match result {
            Ok(logins) => {
                codec::credential_note(did, true, message, &[], logins, None, None, false)
            }
            Err(why) => codec::credential_note(did, false, why, &[], &[], None, None, false),
        };
        match answer {
            VaultAnswer::Listed {
                did,
                message,
                result,
            } => {
                let note = note(did, result.as_deref().map_err(String::as_str), message);
                self.notes.push(note);
            }
            VaultAnswer::Kept { did, site, result } => {
                let kept = result.is_ok();
                let said = note(
                    did,
                    result.as_ref().map(|()| &[][..]).map_err(String::as_str),
                    "Saved in your vault",
                );
                self.notes.push(said);
                // The site's logins as they are now, the one just kept among them.
                if kept && let Some(vault) = self.vault.clone() {
                    let sender = self.vault_answers.0.clone();
                    self.vault_pending = self.vault_pending.saturating_add(1);
                    vault.logins(
                        &site,
                        Box::new(move |result| {
                            let _ = sender.send(VaultAnswer::Listed {
                                did: "list",
                                message: "",
                                result,
                            });
                        }),
                    );
                }
            }
            VaultAnswer::Filled {
                form,
                site,
                document,
                result,
            } => match result {
                Ok((username, password)) => {
                    self.send_credential(CredentialRequest {
                        request_id: 0,
                        origin: site,
                        document,
                        action: CredentialAction::Fill {
                            form,
                            username,
                            password,
                        },
                    });
                }
                Err(why) => self.notes.push(note("fill", Err(&why), "")),
            },
        }
    }

    /// Send one credential action, named by the session's own number for it, which it answers.
    fn send_credential(&mut self, mut request: CredentialRequest) -> u32 {
        self.credential_next = self.credential_next.wrapping_add(1).max(1);
        request.request_id = self.credential_next;
        self.engine.send(WebViewCommand::Credentials(request));
        self.credential_next
    }

    /// Answer the page's file chooser `id` with exactly the paths the person picked, or with
    /// none to cancel it.
    pub fn answer_chooser(&self, id: u32, paths: Vec<String>) {
        self.engine
            .send(WebViewCommand::AnswerFileChooser { id, paths });
    }

    /// Whether the page has something for the window: an answer to carry, or an event to show.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.waiting.is_empty()
            || !self.answers.is_empty()
            || !self.events.is_empty()
            || self.prompts.changed()
            || !self.choosers.is_empty()
            || self.login_waiting.is_some()
            || !self.taking.is_empty()
            || self.vault_pending > 0
            || !self.notes.is_empty()
    }
}

impl Drop for PageSession {
    fn drop(&mut self) {
        // A download the window handed over and the app never started can no longer start.
        tilo_engine_loader::kept::forget(self.keeper);
        // The engine's own supervisor removes a private root once its child has exited; a
        // window that closed before one started leaves the root to this.
        if let Some(root) = self.private_root.take() {
            tilo_webview::chromium::release_private_root(&root);
        }
    }
}
