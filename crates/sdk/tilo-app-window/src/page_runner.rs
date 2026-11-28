//! The page this window shows: one engine, one reader of its events, and the answers to the
//! app's page calls.
//!
//! An app's actions run in another process from the window that holds the engine. A page call
//! arrives as `ToUi::Page` carrying the request as JSON, the window asks its page
//! ([`tilo_page_engine`]), and the answer goes back as `ToApp::PageAnswer`. The same session
//! feeds the view its page's events, so the engine's one event stream is read exactly once.

use std::cell::RefCell;
use std::rc::Rc;

pub use tilo_page_engine::Arrived;
use tilo_page_engine::{PageSession, codec};
use tilo_ui_webview as webview;
use tilo_webview::{SharedEngine, WebViewCommand};

/// The page of one window, made when the window first needs it.
pub type Page = Rc<RefCell<Option<PageSession>>>;

/// Have the window of `page` browse as a visitor: it was opened for whoever works its record (an
/// agent), not for the person. Its page starts now, on a profile of its own that goes when the
/// window closes; it is offered none of the person's saved logins and opens none of the sign-ins
/// they brought, whatever the app asks of it. A page the window already started for the person
/// goes.
pub fn browse_as_visitor(page: &Page) {
    page.borrow_mut().replace(visitor_session());
}

/// The engine of `page`, made now when the window has none: the window's one page.
#[must_use]
pub fn engine_of(page: &Page) -> SharedEngine {
    let mut held = page.borrow_mut();
    if let Some(session) = held.as_ref() {
        session.engine().clone()
    } else {
        let engine = webview::new_page_engine();
        #[cfg_attr(
            target_arch = "wasm32",
            expect(unused_mut, reason = "a web page has no vault")
        )]
        let mut session = PageSession::new(engine.clone());
        #[cfg(not(target_arch = "wasm32"))]
        session.set_vault(vault());
        held.replace(session);
        engine
    }
}

/// The engine of the page the window shows beside or over its own — an extension's side panel,
/// the page it asked the person to sign in on — when one is open. The window draws it in its
/// second `ui.WebView`, and its one session reads that engine's events.
#[must_use]
pub fn panel_engine(page: &Page) -> Option<SharedEngine> {
    page.borrow().as_ref().and_then(PageSession::panel_engine)
}

/// The workspace's vault, which keeps the page's logins, asked on this window's own connection
/// and waking the window that runs now as it answers. A page in a web browser has none: its
/// window has no daemon to ask.
#[cfg(not(target_arch = "wasm32"))]
fn vault() -> std::rc::Rc<dyn tilo_page_engine::LoginVault> {
    std::rc::Rc::new(crate::page_vault::NativeVault::new(ui::WakeHandle::new()))
}

/// The file beside the Browser's profile that names the sign-ins it holds.
#[cfg(not(target_arch = "wasm32"))]
const SIGN_INS_HELD: &str = "tilo-sign-ins";

/// What opens the person's website accounts (Slack, Discord) on profiles of their own, each
/// signed in as its account: the window's host's, which keeps those profiles.
pub trait AccountProfiles: Send + Sync {
    /// Open the account `session` is the sign-in of on its own profile, signed in as it: the
    /// engine, where the account opens, and what keeps the profile until the window closes. It
    /// is called off the window's thread, and may take as long as the engine takes to start.
    ///
    /// # Errors
    /// Why the account did not open, in the person's words.
    fn open(
        &self,
        session: tilo_webview::sessions::SessionCookies,
    ) -> Result<tilo_page_engine::Arrived, String>;
}

/// What asking the page left to do.
pub enum Asked {
    /// The operation answered at once (a navigation, a zoom): what the app reads.
    Answered(String),
    /// The page was asked; its answer comes with the action `action`.
    Waiting(u32),
}

/// Ask the page `request` (JSON an app's action wrote) to do what it says. `accounts` opens the
/// person's website accounts, where the window's host gave it one.
///
/// # Errors
/// The request is no JSON the page reads, or the window keeps no page.
pub fn ask(
    page: &Page,
    request: &str,
    accounts: Option<&std::sync::Arc<dyn AccountProfiles>>,
) -> Result<Asked, String> {
    match codec::decode(request)? {
        // A page opened private starts its window's engine on a fresh, disposable profile. It is
        // the window's to make before anything else is asked, since it settles which engine the
        // page has; a page that already started keeps the engine it has.
        codec::Ask::Private => {
            start_private(page)?;
            Ok(Asked::Answered(codec::accepted()))
        }
        // The sites the person brought their sign-ins of open signed in in the page's profile,
        // which names beside itself the sign-ins it holds. A page on a profile of its own (a
        // visitor's window: the app asks no private page for them) opens none, and says it
        // browses as a visitor.
        codec::Ask::SignIns => {
            let _ = engine_of(page);
            if page.borrow().as_ref().is_some_and(PageSession::is_private) {
                return Ok(Asked::Answered(codec::visitor()));
            }
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(profile) = tilo_webview::chromium::browsing_profile() {
                session_of(page, |session| {
                    session.open_sign_ins(profile.join(SIGN_INS_HELD));
                })?;
            }
            Ok(Asked::Answered(codec::accepted()))
        }
        // The person's website accounts that open in a window of their own: the vault's answer
        // reaches the app as a note. A private page (a visitor's) asks for none.
        codec::Ask::Accounts => {
            let _ = engine_of(page);
            if_session(page, PageSession::list_accounts);
            Ok(Asked::Answered(codec::accepted()))
        }
        // A page that opens one of the person's website accounts settles its engine before
        // anything else is asked: the account's own, signed in as it, which the window's host
        // opens off this thread. A visitor's page opens none.
        codec::Ask::Account { account } => open_account(page, &account, accounts),
        // An extension the person chose arrives as the address of its bytes in this device's
        // content store: an app names no host path, so the window reads the package here and
        // hands the engine the bytes.
        codec::Ask::ExtensionInstall { file } => {
            let bytes = package_bytes(&file)?;
            engine_of(page).send(WebViewCommand::ExtensionInstall {
                request_id: 1,
                bytes,
            });
            Ok(Asked::Answered(codec::accepted()))
        }
        // A login action names the page's document and site, which are the session's to know
        // and the app's never to see: the session asks the page for them and acts after, with
        // the engine and the workspace's vault.
        codec::Ask::Login { origin, ask } => {
            session_of(page, |session| session.login(&origin, ask))?;
            Ok(Asked::Answered(codec::accepted()))
        }
        // What the person types in the address, presses Enter on or leaves, and the page an
        // extension showed beside this one going: the window's session keeps the bookkeeping
        // and tells the app through its notes.
        codec::Ask::AddressTyped {
            text,
            extension,
            name,
            rest,
        } => {
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            let addressed = extension.map(|id| tilo_page_engine::Addressed {
                id,
                name,
                text: rest,
            });
            session.address_typed(&text, addressed.as_ref());
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::AddressEntered { text } => {
            if_session(page, |session| session.address_entered(&text));
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::AddressLeft => {
            if_session(page, PageSession::address_left);
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::DownloadAgain { url, job } => {
            let _ = engine_of(page);
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            session.download_again(&url, &job);
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::PanelClose => {
            if_session(page, PageSession::close_panel);
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::Command(command) => {
            engine_of(page).send(command);
            Ok(Asked::Answered(codec::accepted()))
        }
        codec::Ask::Asks => {
            let _ = engine_of(page);
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            session.poll();
            Ok(Asked::Answered(codec::asks_json(session.asks())))
        }
        codec::Ask::Page(request) => {
            let _ = engine_of(page);
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            Ok(Asked::Waiting(session.ask(None, request)))
        }
    }
}

/// What a page that asks for a website account is told where its window opens none.
const NO_ACCOUNTS: &str = "This window cannot open your accounts";

/// Open the person's website account `account` in the window of `page`: the page shows that it
/// is on its way, the vault is asked for the account's sign-in on the window's own connection,
/// and `accounts` opens its profile off this thread. A page the window already started for the
/// person goes; a visitor's page opens none, and says so.
#[cfg(not(target_arch = "wasm32"))]
fn open_account(
    page: &Page,
    account: &str,
    accounts: Option<&std::sync::Arc<dyn AccountProfiles>>,
) -> Result<Asked, String> {
    if page.borrow().as_ref().is_some_and(PageSession::is_private) {
        return Ok(Asked::Answered(codec::visitor()));
    }
    let accounts = std::sync::Arc::clone(accounts.ok_or_else(|| NO_ACCOUNTS.to_owned())?);
    let (send, arriving) = std::sync::mpsc::channel();
    let wake = ui::WakeHandle::new();
    let vault = crate::page_vault::NativeVault::new(wake.clone());
    vault.website_account(account, move |session| {
        let opened = std::thread::Builder::new()
            .name("tilo-page-account".to_owned())
            .spawn(move || {
                // A window that closed meanwhile has nobody to hand the engine to; what keeps the
                // profile goes with the answer.
                drop(send.send(session.and_then(|session| accounts.open(session))));
                wake.wake();
            });
        if let Err(error) = opened {
            tracing::error!(%error, "an account's profile could not be opened off the window");
        }
    });
    let placeholder: SharedEngine =
        tilo_webview::chromium::UnavailableEngine::new("Opening your account…".to_owned());
    page.borrow_mut()
        .replace(PageSession::new_arriving(placeholder, arriving));
    Ok(Asked::Answered(codec::accepted()))
}

/// A page in a web browser opens no account: its window has no daemon to ask.
#[cfg(target_arch = "wasm32")]
fn open_account(
    _page: &Page,
    _account: &str,
    _accounts: Option<&std::sync::Arc<dyn AccountProfiles>>,
) -> Result<Asked, String> {
    Err(NO_ACCOUNTS.to_owned())
}

/// Start the window's page on a fresh, disposable profile, unless it already has one.
fn start_private(page: &Page) -> Result<(), String> {
    let mut held = page.borrow_mut();
    if held.is_some() {
        return Ok(());
    }
    let root = tilo_webview::chromium::allocate_private_root()?;
    let engine = match tilo_ui_webview::new_private_page_engine(&root) {
        Ok(engine) => engine,
        Err(why) => {
            tilo_webview::chromium::release_private_root(&root);
            return Err(why);
        }
    };
    held.replace(PageSession::new_private(engine, root));
    Ok(())
}

/// The page of a window opened for a visitor: a profile of its own, which goes with the window,
/// and no vault; what the page is about to send (a form, a download, a payment) waits for the
/// person's answer. A page that cannot start one shows why, never the person's profile instead,
/// and is given no vault either.
fn visitor_session() -> PageSession {
    let unavailable = |why: String| -> SharedEngine {
        tracing::error!(%why, "a visitor's page could not start a profile of its own");
        tilo_webview::chromium::UnavailableEngine::new(why)
    };
    let session = match tilo_webview::chromium::allocate_private_root() {
        Ok(root) => {
            let engine =
                tilo_ui_webview::new_private_page_engine(&root).unwrap_or_else(unavailable);
            PageSession::new_private(engine, root)
        }
        Err(why) => PageSession::new(unavailable(why)),
    };
    session.engine().hold_departures();
    session
}

/// Do `act` with the window's page session, refusing when the window keeps no page.
fn session_of<T>(page: &Page, act: impl FnOnce(&mut PageSession) -> T) -> Result<T, String> {
    let mut held = page.borrow_mut();
    let session = held
        .as_mut()
        .ok_or_else(|| "this window has no page".to_owned())?;
    Ok(act(session))
}

/// Do `act` with the window's page session, when it has one: with none, there is nothing to do.
fn if_session(page: &Page, act: impl FnOnce(&mut PageSession)) {
    if let Some(session) = page.borrow_mut().as_mut() {
        act(session);
    }
}

/// The bytes the content address `file` (`tilo-blob://<hash>`) names, from this device's content
/// store: what the person chose, read where a sandboxed app may not.
fn package_bytes(file: &str) -> Result<Vec<u8>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        read_package(
            |hash| tilo_content_device::device().and_then(|store| store.read(hash)),
            file,
        )
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = file;
        Err("this host keeps no content store".to_owned())
    }
}

/// The bytes the content address `file` names, as `read` answers the content at a reference: a
/// host path names no content a sandboxed app may hold. A page's host keeps no store to read.
#[cfg(not(target_arch = "wasm32"))]
fn read_package(read: impl Fn(&str) -> Option<Vec<u8>>, file: &str) -> Result<Vec<u8>, String> {
    let hash = file
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{file}` is no file's address"))?;
    read(hash).ok_or_else(|| "the package is not on this device".to_owned())
}

/// Take the answers the page has given since the window last looked, oldest first.
#[must_use]
pub fn answers(page: &Page) -> Vec<(u32, String)> {
    let mut held = page.borrow_mut();
    let Some(session) = held.as_mut() else {
        return Vec::new();
    };
    session.poll();
    session
        .answers()
        .into_iter()
        .map(|(action, answer)| (action, codec::encode(&answer)))
        .collect()
}

/// What the page started and stopped waiting on the person's yes for since the window last
/// looked, each encoded for the app (`ToApp::PagePrompt`), oldest first.
#[must_use]
pub fn prompts(page: &Page) -> Vec<String> {
    let mut page = page.borrow_mut();
    let Some(session) = page.as_mut() else {
        return Vec::new();
    };
    session.poll();
    session.prompts().iter().map(codec::prompt_json).collect()
}

/// The page's file choosers a person's click opened since the window last took them, oldest
/// first: the window asks the host's picker and answers each with [`answer_chooser`].
#[must_use]
pub fn choosers(page: &Page) -> Vec<tilo_webview::FileChooser> {
    let mut held = page.borrow_mut();
    let Some(session) = held.as_mut() else {
        return Vec::new();
    };
    session.poll();
    session.choosers()
}

/// What the page said of itself that the app's record carries (a download moving, the pages it
/// can go to) since the window last took it, each encoded for the app, oldest first.
#[must_use]
pub fn notes(page: &Page) -> Vec<String> {
    let mut held = page.borrow_mut();
    let Some(session) = held.as_mut() else {
        return Vec::new();
    };
    session.poll();
    session.notes()
}

/// Whether the page said anything of itself since the window last took it.
#[must_use]
pub fn notes_waiting(page: &Page) -> bool {
    page.borrow_mut().as_mut().is_some_and(|session| {
        session.poll();
        session.has_notes()
    })
}

/// Whether a file chooser started waiting since the window last took them.
#[must_use]
pub fn choosers_waiting(page: &Page) -> bool {
    page.borrow_mut().as_mut().is_some_and(|session| {
        session.poll();
        session.has_choosers()
    })
}

/// Answer the page's file chooser `id` with exactly the paths the person picked, or with none
/// to cancel it.
pub fn answer_chooser(page: &Page, id: u32, paths: Vec<String>) {
    let mut held = page.borrow_mut();
    let Some(session) = held.as_mut() else {
        return;
    };
    session.answer_chooser(id, paths);
}

/// Whether the page started or stopped waiting on the person's yes since the window last took
/// it ([`prompts`]): the window then owes the app a message.
#[must_use]
pub fn prompts_waiting(page: &Page) -> bool {
    page.borrow_mut().as_mut().is_some_and(|session| {
        session.poll();
        session.has_prompts()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window opened for an agent starts its page on a profile of its own, which holds none of
    /// the person's cookies, and opens none of the sign-ins they brought when the app asks: it
    /// says it browses as a visitor instead.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_window_opened_for_an_agent_holds_none_of_the_persons_sign_ins() {
        let page = Page::default();
        browse_as_visitor(&page);
        assert!(
            page.borrow().is_some(),
            "the page starts before the window shows anything"
        );
        let Ok(Asked::Answered(answer)) = ask(&page, r#"{"op":"sign_ins"}"#, None) else {
            panic!("a visitor's window answers at once");
        };
        let answer: serde_json::Value = serde_json::from_str(&answer).expect("JSON");
        assert_eq!(answer["code"], "visitor", "{answer}");
        assert_eq!(answer["ok"], false, "{answer}");
        // A private page keeps no vault, so it is offered no saved login and asks for no sign-in.
        assert!(
            page.borrow()
                .as_ref()
                .expect("the page started")
                .is_private(),
            "the page browses on a profile of its own, not the person's"
        );
        // Asked to browse privately or to open the sign-ins again, it stays as it is.
        let _ = ask(&page, r#"{"op":"private"}"#, None);
        let Ok(Asked::Answered(again)) = ask(&page, r#"{"op":"sign_ins"}"#, None) else {
            panic!("a visitor's window answers at once");
        };
        assert_eq!(again, codec::visitor());
        assert!(page.borrow().as_ref().expect("kept").is_private());

        // Nor does it open a website account of the person's, whatever the app asks.
        let accounts: std::sync::Arc<dyn AccountProfiles> = std::sync::Arc::new(Refused);
        let Ok(Asked::Answered(account)) = ask(
            &page,
            r#"{"op":"account","account":"slack-1"}"#,
            Some(&accounts),
        ) else {
            panic!("a visitor's window answers at once");
        };
        assert_eq!(account, codec::visitor());
        assert!(page.borrow().as_ref().expect("kept").is_private());
    }

    /// Opens no account, and says so if it is asked.
    struct Refused;
    impl AccountProfiles for Refused {
        fn open(
            &self,
            _session: tilo_webview::sessions::SessionCookies,
        ) -> Result<tilo_page_engine::Arrived, String> {
            Err("asked to open an account".to_owned())
        }
    }

    /// A page that opens one of the person's website accounts starts on a placeholder while the
    /// account is on its way, never on the person's own profile, and says why when the account
    /// does not come: a window with no connection to the daemon has no vault to ask for it, and
    /// its host is never asked to open one. A window whose host opens none refuses at once.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_account_page_waits_for_its_account_and_says_why_it_did_not_come() {
        let accounts: std::sync::Arc<dyn AccountProfiles> = std::sync::Arc::new(Refused);
        let page = Page::default();
        let Ok(Asked::Answered(answer)) = ask(
            &page,
            r#"{"op":"account","account":"slack-1"}"#,
            Some(&accounts),
        ) else {
            panic!("the window answers at once");
        };
        assert_eq!(answer, codec::accepted());
        assert!(
            !page
                .borrow()
                .as_ref()
                .expect("the page started")
                .is_private(),
            "an account's page is no visitor's"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let said = loop {
            if let Some(note) = notes(&page).into_iter().next() {
                break serde_json::from_str::<serde_json::Value>(&note).expect("JSON");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the page said nothing of its account"
            );
            std::thread::yield_now();
        };
        assert_eq!(said["note"], "account");
        assert_eq!(said["open"], false);
        assert_eq!(
            said["message"],
            crate::page_vault::NO_DAEMON,
            "the vault was asked, not the host"
        );

        let page = Page::default();
        assert_eq!(
            ask(&page, r#"{"op":"account","account":"slack-1"}"#, None).err(),
            Some(NO_ACCOUNTS.to_owned())
        );
    }

    #[test]
    fn a_chosen_package_is_read_from_the_content_store_by_its_address() {
        let store = tilo_content::ContentStore::memory();
        let hash = store.put(b"a package").expect("kept");
        let file = format!("tilo-blob://{hash}");
        assert_eq!(
            read_package(|hash| store.read(hash), &file).expect("the package"),
            b"a package"
        );
        assert!(
            read_package(|hash| store.read(hash), "/home/ada/reader.crx").is_err(),
            "a host path names no content"
        );
        assert!(
            read_package(|_| None, &file).is_err(),
            "a package this device does not hold is missing"
        );
    }
}
