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
    /// in on.
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
    /// The number this window keeps a download's request under ([`tilo_engine_loader::request`]),
    /// so what it never handed over goes when it closes.
    keeper: u64,
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
                    url,
                    name,
                    referrer,
                    cookies,
                    user_agent,
                    ..
                } => {
                    let context = serde_json::json!({
                        "referrer": referrer,
                        "cookies": cookies,
                        "user_agent": user_agent,
                    });
                    let request = tilo_engine_loader::kept::keep(self.keeper, context.to_string());
                    self.notes.push(codec::download_note(url, name, &request));
                    continue;
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
        // Pages a script opened in a window of their own: a popup the site sized, or a plain new
        // tab. The app keeps them as pages; the window that holds one is the system's.
        while let Ok(answer) = self.vault_answers.1.try_recv() {
            self.vault_pending = self.vault_pending.saturating_sub(1);
            self.vault_answered(answer);
        }
        for (url, _foreground) in self.engine.take_new_tab_requests() {
            self.notes.push(codec::open_note(&url, false));
        }
        self.engine.accept_popups();
        for popup in self.engine.take_popups() {
            self.notes.push(codec::open_note(&popup.url, true));
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

    /// The engine of the window's second page (an extension's side panel, a sign-in page), when
    /// one is open: the page the window draws beside or over the one the person browsed.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_webview::{MediaElementInfo, test_engine::TestEngine};

    /// A document that finished loading is asked what video it shows: the answer waits in the
    /// asks for the app and its window, and the app is told the page's video changed so it reads
    /// it. A document with no video says so the same way.
    #[test]
    fn a_loaded_page_is_asked_what_video_it_shows_and_tells_the_app_when_it_changes() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        let video = MediaElementInfo {
            element_id: "video:1".to_owned(),
            viewport_rect: [0.0, 0.1, 1.0, 0.5],
            intrinsic_size: [1_280, 720],
            label: "A talk on maps".to_owned(),
        };
        test.set_media_element(Some(video.clone()));
        test.push_event(WebViewEvent::LoadFinished);
        session.poll();
        assert!(
            test.commands()
                .iter()
                .any(|command| matches!(command, WebViewCommand::RequestMediaElement { .. })),
            "the loaded page is asked what video it shows"
        );
        session.poll();
        assert_eq!(
            session.asks().media.as_ref(),
            Some(&video),
            "the video waits in the asks for the app and its window"
        );
        assert!(session.has_notes(), "the app is told the page's video");
        let notes = session.notes();
        assert!(
            notes.iter().any(|note| note.contains(r#""note":"media""#)),
            "a media note: {notes:?}"
        );

        // The page went elsewhere and shows no video: what waited is gone, and the app is told
        // once the new document says it shows none.
        test.set_media_element(None);
        test.push_event(WebViewEvent::UriChanged("https://example.org/".to_owned()));
        session.poll();
        assert_eq!(
            session.asks().media,
            None,
            "another document shows no video"
        );
        test.push_event(WebViewEvent::LoadFinished);
        session.poll();
        session.poll();
        assert_eq!(session.asks().media, None);
        assert!(
            session
                .notes()
                .iter()
                .any(|note| note.contains(r#""note":"media""#)),
            "the app is told the page shows no video"
        );
    }

    /// A page's outline is asked once and answered once, and the page's own events stay theirs.
    #[test]
    fn an_answer_comes_once_and_the_pages_events_reach_its_view() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        let action = session.ask(
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        );
        assert!(session.waiting_for(action));
        session.poll();
        let answers = session.answers();
        assert_eq!(answers.len(), 1, "one answer, for one action");
        assert_eq!(answers[0].0, action);
        assert!(
            answers[0].1.is_ok(),
            "the outline answers: {:?}",
            answers[0].1
        );
        assert!(!session.waiting_for(action), "the action is answered");

        // What the page reports of itself is the view's, not an answer's: it waits in order.
        test.push_event(WebViewEvent::UriChanged("https://example.org/".to_owned()));
        session.poll();
        assert!(matches!(
            session.next_event(),
            Some(WebViewEvent::UriChanged(uri)) if uri == "https://example.org/"
        ));
        assert!(session.next_event().is_none());
        assert!(session.answers().is_empty(), "events are not answers");
    }

    /// A file input's chooser is the window's to ask with the host's picker, never the page's
    /// own event, and the answer goes back with exactly the paths the person chose.
    #[test]
    fn a_file_chooser_is_the_windows_and_its_answer_is_the_persons_paths() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine);
        test.push_event(WebViewEvent::FileChooser(tilo_webview::FileChooser {
            id: 7,
            mode: tilo_webview::FileChooserMode::One,
            title: "Upload".to_owned(),
            accept: vec!["image/png".to_owned()],
        }));
        session.poll();
        assert!(session.has_choosers());
        let choosers = session.choosers();
        assert_eq!(choosers.len(), 1, "one chooser waits: {choosers:?}");
        assert_eq!(choosers[0].id, 7);
        assert!(
            session.next_event().is_none(),
            "the chooser is not an event the page's view draws"
        );

        session.answer_chooser(7, vec!["/home/ada/picture.png".to_owned()]);
        assert!(
            test.commands().iter().any(|command| matches!(
                command,
                WebViewCommand::AnswerFileChooser { id, paths }
                    if *id == 7 && paths == &["/home/ada/picture.png".to_owned()]
            )),
            "the page is answered with the person's path: {:?}",
            test.commands()
        );
    }

    /// A vault that answers at once, keeping what it was asked: the logins it lists, and what
    /// filling answers.
    /// A login the fake vault was handed: its site, name, password and the login it replaces.
    type Kept = (String, String, String, Option<String>);

    #[derive(Default)]
    struct FakeVault {
        asked: std::cell::RefCell<Vec<String>>,
        kept: std::cell::RefCell<Vec<Kept>>,
        refuse_fill: Option<String>,
    }

    const TYPED: &str = "synthetic-typed-password";
    const KEPT: &str = "synthetic-vault-password";

    fn login(item: &str, username: &str) -> VaultLogin {
        VaultLogin {
            item: item.to_owned(),
            title: "example.org".to_owned(),
            username: username.to_owned(),
            own: true,
        }
    }

    impl LoginVault for FakeVault {
        fn logins(&self, site: &str, done: Answered<Vec<VaultLogin>>) {
            self.asked.borrow_mut().push(format!("logins {site}"));
            done(Ok(vec![login("vault.item:1", "ada")]));
        }
        fn keep(
            &self,
            site: &str,
            username: &str,
            password: tilo_webview::ReleasedText,
            replacing: Option<&str>,
            done: Answered<()>,
        ) {
            let secret = password.take().map(|p| p.to_string()).unwrap_or_default();
            self.kept.borrow_mut().push((
                site.to_owned(),
                username.to_owned(),
                secret,
                replacing.map(str::to_owned),
            ));
            done(Ok(()));
        }
        fn forget(&self, site: &str, item: &str, done: Answered<Vec<VaultLogin>>) {
            self.asked
                .borrow_mut()
                .push(format!("forget {item} {site}"));
            done(Ok(Vec::new()));
        }
        fn fill(
            &self,
            site: &str,
            item: &str,
            done: Answered<(String, tilo_webview::ReleasedText)>,
        ) {
            self.asked.borrow_mut().push(format!("fill {item} {site}"));
            done(match &self.refuse_fill {
                Some(why) => Err(why.clone()),
                None => Ok((
                    "ada".to_owned(),
                    tilo_webview::ReleasedText::new(KEPT.to_owned()),
                )),
            });
        }
    }

    /// The engine's answer to a login action.
    fn reply(
        action: &str,
        success: bool,
        origin: &str,
        document: &str,
    ) -> tilo_webview::CredentialReply {
        tilo_webview::CredentialReply {
            request_id: 1,
            action: action.to_owned(),
            origin: origin.to_owned(),
            document: document.to_owned(),
            success,
            message: if success {
                "Ready"
            } else {
                "Page changed or secure form access is unavailable"
            }
            .to_owned(),
            forms: Vec::new(),
            capture: None,
            username: None,
            late: false,
            password: None,
        }
    }

    fn last_credential(test: &std::sync::Arc<TestEngine>) -> CredentialRequest {
        match test.commands().last() {
            Some(WebViewCommand::Credentials(request)) => request.clone(),
            other => panic!("a credential action was sent: {other:?}"),
        }
    }

    fn with_vault(vault: FakeVault) -> (std::sync::Arc<TestEngine>, PageSession, Rc<FakeVault>) {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        let vault = Rc::new(vault);
        session.set_vault(vault.clone());
        (test, session, vault)
    }

    /// A login action asks the engine where the page is first, and acts on the document and the
    /// site the engine names, whatever the app said: the question itself is not the person's
    /// to see.
    #[test]
    fn a_login_action_names_the_pages_own_document_and_site() {
        let (test, mut session, vault) = with_vault(FakeVault::default());
        session.login(
            "https://elsewhere.example",
            LoginAsk::Engine(CredentialAction::Discover),
        );
        assert!(matches!(
            last_credential(&test).action,
            CredentialAction::Status
        ));
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            true,
            "https://example.org",
            "4",
        )));
        session.poll();
        let discover = last_credential(&test);
        assert!(matches!(discover.action, CredentialAction::Discover));
        assert_eq!(
            (discover.origin.as_str(), discover.document.as_str()),
            ("https://example.org", "4")
        );
        assert!(
            session.notes().is_empty(),
            "the status is not the person's to see"
        );

        session.login("https://elsewhere.example", LoginAsk::List);
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            true,
            "https://example.org",
            "4",
        )));
        session.poll();
        assert_eq!(*vault.asked.borrow(), ["logins https://example.org"]);
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).unwrap();
        assert_eq!(note["did"], "list");
        let accounts: serde_json::Value =
            serde_json::from_str(note["accounts"].as_str().unwrap()).unwrap();
        assert_eq!(accounts[0]["account"], "vault.item:1");
        assert_eq!(accounts[0]["username"], "ada");
    }

    /// What a person typed goes from the engine to the vault, for the site the page is at, and
    /// into no note: the app hears it was saved, and the site's logins as they are now.
    #[test]
    fn a_saved_login_goes_from_the_engine_to_the_vault_alone() {
        let (test, mut session, vault) = with_vault(FakeVault::default());
        session.login(
            "https://example.org",
            LoginAsk::Update {
                capture: "3".to_owned(),
                item: "vault.item:1".to_owned(),
            },
        );
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            true,
            "https://example.org",
            "4",
        )));
        session.poll();
        let take = last_credential(&test);
        assert!(matches!(&take.action, CredentialAction::Take { capture } if capture == "3"));
        let mut taken = reply("take", true, "https://example.org", "4");
        taken.request_id = take.request_id;
        taken.username = Some("ada".to_owned());
        taken.password = Some(tilo_webview::ReleasedText::new(TYPED.to_owned()));
        test.push_event(WebViewEvent::CredentialResult(taken));
        session.poll();
        assert_eq!(
            *vault.kept.borrow(),
            [(
                "https://example.org".to_owned(),
                "ada".to_owned(),
                TYPED.to_owned(),
                Some("vault.item:1".to_owned())
            )]
        );
        let notes = session.notes();
        assert!(notes.iter().all(|note| !note.contains(TYPED)), "{notes:?}");
        let said: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| serde_json::from_str(note).unwrap())
            .collect();
        assert_eq!(said[0]["did"], "update");
        assert_eq!(said[0]["success"], true);
        assert_eq!(said[1]["did"], "list");
        while session.next_event().is_some() {}
        assert!(!session.busy(), "nothing waits on the engine or the vault");
    }

    /// Filling asks the vault for the site the engine says the page is at, and hands the engine
    /// the login for that document; a fill the vault refuses fills nothing and says why.
    #[test]
    fn a_fill_hands_the_vaults_login_to_the_engine_or_says_why_not() {
        let (test, mut session, vault) = with_vault(FakeVault::default());
        session.login(
            "https://elsewhere.example",
            LoginAsk::Fill {
                form: "2".to_owned(),
                item: "vault.item:1".to_owned(),
            },
        );
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            true,
            "https://example.org",
            "4",
        )));
        session.poll();
        assert_eq!(
            *vault.asked.borrow(),
            ["fill vault.item:1 https://example.org"]
        );
        let fill = last_credential(&test);
        assert_eq!(
            (fill.origin.as_str(), fill.document.as_str()),
            ("https://example.org", "4")
        );
        match &fill.action {
            CredentialAction::Fill {
                form,
                username,
                password,
            } => {
                assert_eq!((form.as_str(), username.as_str()), ("2", "ada"));
                assert_eq!(password.take().as_deref().map(String::as_str), Some(KEPT));
            }
            other => panic!("a fill: {other:?}"),
        }
        assert!(session.notes().iter().all(|note| !note.contains(KEPT)));

        let (test, mut session, _) = with_vault(FakeVault {
            refuse_fill: Some("Browser may not fill this login yet".to_owned()),
            ..FakeVault::default()
        });
        session.login(
            "https://example.org",
            LoginAsk::Fill {
                form: "2".to_owned(),
                item: "vault.item:9".to_owned(),
            },
        );
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            true,
            "https://example.org",
            "4",
        )));
        session.poll();
        assert!(
            matches!(last_credential(&test).action, CredentialAction::Status),
            "nothing filled"
        );
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).unwrap();
        assert_eq!(note["did"], "fill");
        assert_eq!(note["success"], false);
        assert_eq!(note["message"], "Browser may not fill this login yet");
    }

    /// A page the engine will not reach (not secure, or moved on) says so in the action's own
    /// answer, and nothing is asked of the engine or the vault for it.
    #[test]
    fn a_login_action_the_engine_refuses_is_answered_in_its_own_words() {
        let (test, mut session, vault) = with_vault(FakeVault::default());
        session.login("http://example.org", LoginAsk::List);
        let before = test.commands().len();
        test.push_event(WebViewEvent::CredentialResult(reply(
            "status",
            false,
            "http://example.org",
            "",
        )));
        session.poll();
        assert_eq!(test.commands().len(), before);
        assert!(vault.asked.borrow().is_empty());
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).unwrap();
        assert_eq!(note["did"], "list");
        assert_eq!(note["success"], false);
        assert_eq!(
            note["message"],
            "Page changed or secure form access is unavailable"
        );
    }

    /// A page with no vault to ask (a private one) is offered no logins and keeps none.
    #[test]
    fn a_page_without_a_vault_offers_and_keeps_no_logins() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        session.login(
            "https://example.org",
            LoginAsk::Save {
                capture: "3".to_owned(),
            },
        );
        assert!(test.commands().is_empty(), "nothing is taken from the form");
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).unwrap();
        assert_eq!(note["did"], "save");
        assert_eq!(note["success"], false);
        assert_eq!(note["message"], logins::NO_VAULT);
    }

    /// A private page's profile is its window's alone and goes when the page does: nothing it
    /// read or signed in to is kept for the next window.
    #[test]
    fn a_private_page_owns_the_profile_it_goes_with() {
        let root = tilo_webview::chromium::allocate_private_root().expect("a private root");
        assert!(
            tilo_webview::chromium::is_owned_private_root(&root),
            "a root this browser made is one it may remove"
        );
        let session = PageSession::new_private(TestEngine::new(), root.clone());
        assert!(session.is_private());
        drop(session);
        assert!(!root.exists(), "the private profile goes with the page");
    }

    /// A page that is not private keeps this device's profile.
    #[test]
    fn an_ordinary_page_is_not_private() {
        assert!(!PageSession::new(TestEngine::new()).is_private());
    }

    /// The page written as a PDF is the app's note, naming the file the engine wrote and what
    /// it is called.
    #[test]
    fn a_printed_page_is_a_note_naming_the_pdf() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::PrintResult {
            request_id: 1,
            ok: true,
            path: "/home/ada/.local/share/tilo/chromium/print/Example page.pdf".into(),
            name: "Example page.pdf".into(),
            message: String::new(),
        });
        session.poll();
        let note: serde_json::Value =
            serde_json::from_str(&session.notes()[0]).expect("the note is JSON");
        assert_eq!(note["note"], "print");
        assert_eq!(note["ok"], true);
        assert_eq!(
            note["path"],
            "/home/ada/.local/share/tilo/chromium/print/Example page.pdf"
        );
        assert_eq!(note["name"], "Example page.pdf");
    }

    /// An extension's toolbar button is what the engine last said of it, and a reset takes them
    /// all away; the button a press opens a page from is a popup the app keeps as a page.
    #[test]
    fn an_extensions_toolbar_button_is_what_the_engine_last_said() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionAction {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            global: false,
            change: tilo_webview::ExtensionActionChange {
                badge: Some(Some("3".to_owned())),
                title: Some(Some("Read later".to_owned())),
                popup: None,
                enabled: None,
                icon: None,
            },
        });
        test.push_event(WebViewEvent::ExtensionAction {
            id: "ponmlkjihgfedcbaponmlkjihgfedcba".to_owned(),
            global: false,
            change: tilo_webview::ExtensionActionChange {
                enabled: Some(Some(false)),
                ..Default::default()
            },
        });
        session.poll();
        let buttons = &session.asks().extension_buttons;
        assert_eq!(buttons.len(), 2, "two buttons: {buttons:?}");
        assert!(buttons[0].enabled, "a button nobody dimmed takes a press");
        assert!(
            !buttons[1].enabled,
            "a button its extension dimmed does not"
        );
        assert_eq!(buttons[0].badge, "3");
        assert_eq!(buttons[0].title, "Read later");

        test.push_event(WebViewEvent::ExtensionActionReset);
        session.poll();
        assert!(
            session.asks().extension_buttons.is_empty(),
            "a reset takes them all away"
        );

        test.push_event(WebViewEvent::ExtensionOpenPopup {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            url: "https://example.org/popup".to_owned(),
        });
        session.poll();
        let note: serde_json::Value = serde_json::from_str(&session.notes()[0]).expect("a note");
        assert_eq!(note["note"], "open");
        assert_eq!(note["url"], "https://example.org/popup");
        assert_eq!(note["popup"], true, "a popup is shown as a page of its own");
    }

    /// An extension's notification is kept while it shows and gone when the extension takes it
    /// back, and the person's press on one of its buttons reaches the engine by number.
    #[test]
    fn an_extensions_notification_is_kept_until_the_extension_takes_it_back() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionNotification {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            notification: "n1".to_owned(),
            notice: Some(tilo_webview::ExtensionNotice {
                title: "Saved".to_owned(),
                message: "Three pages kept".to_owned(),
                context: String::new(),
                items: Vec::new(),
                buttons: vec!["Open".to_owned(), "Later".to_owned()],
                progress: None,
                silent: false,
            }),
        });
        session.poll();
        let notices = &session.asks().extension_notices;
        assert_eq!(notices.len(), 1, "one notification: {notices:?}");
        assert_eq!(notices[0].title, "Saved");
        assert_eq!(notices[0].buttons, ["Open", "Later"]);

        test.push_event(WebViewEvent::ExtensionNotification {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            notification: "n1".to_owned(),
            notice: None,
        });
        session.poll();
        assert!(
            session.asks().extension_notices.is_empty(),
            "the extension took it back"
        );
    }

    /// A page a script opened (a popup the site sized, a plain new tab) is a note the window
    /// hands the app, never the page's own event.
    #[test]
    fn pages_a_script_opens_are_notes_for_the_app() {
        let test = TestEngine::new();
        let engine: SharedEngine = test.clone();
        let mut session = PageSession::new(engine.clone());
        test.push_new_tab_request("https://example.org/new");
        // The page holds a popup for a window that asked for them, which is what the session
        // says at each poll.
        engine.accept_popups();
        test.push_popup(9, "https://example.org/sign-in", 420, 640);
        session.poll();
        let notes = session.notes();
        assert_eq!(notes.len(), 2, "a new tab and a popup: {notes:?}");
        let parsed: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| serde_json::from_str(note).expect("a note is JSON"))
            .collect();
        assert_eq!(parsed[0]["note"], "open");
        assert_eq!(parsed[0]["url"], "https://example.org/new");
        assert_eq!(parsed[0]["popup"], false);
        assert_eq!(parsed[1]["url"], "https://example.org/sign-in");
        assert_eq!(parsed[1]["popup"], true);
    }

    /// An action nobody waits for any more is dropped, and the page is told to stop it.
    #[test]
    fn a_cancelled_action_holds_neither_the_page_nor_an_answer() {
        let engine: SharedEngine = TestEngine::new();
        let mut session = PageSession::new(engine.clone());
        let action = session.ask(
            None,
            PageRequest::Outline {
                from: None,
                frame: None,
            },
        );
        session.cancel(action);
        assert!(!session.waiting_for(action));
        session.poll();
        assert!(
            session.answers().is_empty(),
            "a cancelled action has no answer"
        );
    }

    fn note_of(notes: &[String]) -> serde_json::Value {
        serde_json::from_str(notes.last().expect("a note")).expect("a note is JSON")
    }

    /// An extension's side panel opens in a page of its own, in a second engine of the window's
    /// that is told whose panel it is first, and closes when the extension or the person says so.
    #[test]
    fn an_extensions_side_panel_opens_beside_the_page_and_closes() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        let id = "abcdefghijklmnopabcdefghijklmnop".to_owned();
        let url = format!("chrome-extension://{id}/panel.html");
        test.push_event(WebViewEvent::ExtensionSidePanel {
            id: id.clone(),
            change: ExtensionSidePanelChange::Open(url.clone()),
        });
        session.poll();
        let tabs = test.created_tabs();
        assert_eq!(tabs.len(), 1, "the panel is a page of its own");
        let commands = tabs[0].commands();
        assert!(
            commands.contains(&WebViewCommand::ExtensionSidePanelPage { id: id.clone() }),
            "the panel is told whose it is: {commands:?}"
        );
        assert!(
            commands.contains(&WebViewCommand::Navigate(url.clone())),
            "the panel opens its page: {commands:?}"
        );
        let panel = session.panel().expect("the window shows it");
        assert_eq!(panel.kind, "side");
        assert_eq!(panel.id, id);
        assert_eq!(panel.url, url);
        assert!(session.panel_engine().is_some());
        let note = note_of(&session.notes());
        assert_eq!(note["note"], "panel");
        assert_eq!(note["open"], true);
        assert_eq!(note["url"], url);

        // A toggle of the panel that shows closes it, and the app hears of that.
        test.push_event(WebViewEvent::ExtensionSidePanel {
            id: id.clone(),
            change: ExtensionSidePanelChange::Toggle(url.clone()),
        });
        session.poll();
        assert!(session.panel_engine().is_none(), "the panel closed");
        let note = note_of(&session.notes());
        assert_eq!(note["note"], "panel");
        assert_eq!(note["open"], false);

        // And one the person closes goes the same way.
        test.push_event(WebViewEvent::ExtensionSidePanel {
            id,
            change: ExtensionSidePanelChange::Open(url),
        });
        session.poll();
        assert!(session.panel_engine().is_some());
        session.close_panel();
        assert!(session.panel_engine().is_none());
        assert_eq!(note_of(&session.notes())["open"], false);
    }

    /// A panel whose engine closes it (the page went, the sign-in ended) leaves the window, and
    /// the app hears of it.
    #[test]
    fn a_panel_its_engine_closed_leaves_the_window() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionSignIn {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            url: "https://accounts.example.org/sign-in".to_owned(),
            interactive: true,
            flow: 3,
        });
        session.poll();
        let tabs = test.created_tabs();
        assert_eq!(tabs.len(), 1, "the sign-in page is a page of its own");
        assert!(
            tabs[0]
                .commands()
                .contains(&WebViewCommand::ExtensionSignInPanel {
                    id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
                    flow: 3,
                }),
            "the host watches the page for where the sign-in ends: {:?}",
            tabs[0].commands()
        );
        assert_eq!(session.panel().expect("shown").kind, "sign-in");
        drop(session.notes());

        tabs[0].push_event(WebViewEvent::TabClosed);
        session.poll();
        assert!(
            session.panel_engine().is_none(),
            "the window shows it no more"
        );
        assert_eq!(note_of(&session.notes())["open"], false);
    }

    /// A sign-in the extension may not ask the person about loads out of sight: a page of its
    /// own, which the window does not show.
    #[test]
    fn a_quiet_sign_in_loads_without_a_window_showing_it() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        test.push_event(WebViewEvent::ExtensionSignIn {
            id: "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            url: "https://accounts.example.org/sign-in".to_owned(),
            interactive: false,
            flow: 4,
        });
        session.poll();
        let tabs = test.created_tabs();
        assert_eq!(tabs.len(), 1, "it is still loaded");
        assert!(
            tabs[0].commands().iter().any(|command| matches!(
                command,
                WebViewCommand::ExtensionSignInPanel { flow: 4, .. }
            )),
            "the host watches it too: {:?}",
            tabs[0].commands()
        );
        assert!(
            session.panel_engine().is_none(),
            "nobody is shown it, and the notes say nothing of a panel"
        );
        assert!(
            session
                .notes()
                .iter()
                .all(|note| !note.contains("\"panel\""))
        );
    }

    /// The address hears an extension's keyword: the extension hears the person start and each
    /// change, offers its rows, and hears Enter and their leaving.
    #[test]
    fn the_address_hears_an_extensions_keyword_and_offers_its_rows() {
        let test = TestEngine::new();
        let mut session = PageSession::new(test.clone());
        let id = "abcdefghijklmnopabcdefghijklmnop".to_owned();
        session.address_typed(
            "kw ca",
            Some(&Addressed {
                id: id.clone(),
                name: "Kw finder".to_owned(),
                text: "ca".to_owned(),
            }),
        );
        let told: Vec<_> = test
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::ExtensionOmnibox { input, .. } => Some(input),
                _ => None,
            })
            .collect();
        assert!(
            matches!(
                told.as_slice(),
                [
                    tilo_webview::OmniboxInput::Started,
                    tilo_webview::OmniboxInput::Changed { text, request: 1 }
                ] if text == "ca"
            ),
            "the extension hears it started and what is typed: {told:?}"
        );
        let note = note_of(&session.notes());
        assert_eq!(note["note"], "omnibox");
        assert_eq!(note["name"], "Kw finder");

        test.push_event(WebViewEvent::ExtensionOmniboxDefault {
            id: id.clone(),
            description: Some("Look up <match>%s</match>".to_owned()),
        });
        test.push_event(WebViewEvent::ExtensionOmniboxSuggestions {
            id: id.clone(),
            request: 1,
            suggestions: vec![tilo_webview::OmniboxSuggestion {
                content: "cat".to_owned(),
                description: "<match>cat</match> &amp; more".to_owned(),
            }],
        });
        session.poll();
        let note = note_of(&session.notes());
        let rows: serde_json::Value =
            serde_json::from_str(note["rows"].as_str().expect("rows as text"))
                .expect("rows are JSON");
        assert_eq!(
            rows[0]["title"], "Look up ca",
            "Enter does the default suggestion"
        );
        assert_eq!(rows[0]["content"], "ca");
        assert_eq!(rows[1]["title"], "cat & more");
        assert_eq!(rows[1]["content"], "cat");

        session.address_entered("cat");
        assert!(
            test.commands().iter().any(|command| matches!(
                command,
                WebViewCommand::ExtensionOmnibox {
                    input: tilo_webview::OmniboxInput::Entered { text },
                    ..
                } if text == "cat"
            )),
            "the extension hears what was chosen: {:?}",
            test.commands()
        );
        assert_eq!(
            note_of(&session.notes())["id"],
            "",
            "the address addresses no extension once Enter took it"
        );

        session.address_typed(
            "kw c",
            Some(&Addressed {
                id,
                name: "Kw finder".to_owned(),
                text: "c".to_owned(),
            }),
        );
        session.address_left();
        assert!(
            test.commands().iter().any(|command| matches!(
                command,
                WebViewCommand::ExtensionOmnibox {
                    input: tilo_webview::OmniboxInput::Cancelled,
                    ..
                }
            )),
            "the extension hears of the person leaving: {:?}",
            test.commands()
        );
    }
}
