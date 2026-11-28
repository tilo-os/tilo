//! The page this window shows: one engine, one reader of its events, and the answers to the
//! app's page calls.
//!
//! An app's actions run in another process from the window that holds the engine. A page call
//! arrives as `ToUi::Page` carrying the request as JSON, the window asks its page
//! ([`tilo_page_engine`]), and the answer goes back as `ToApp::PageAnswer`. The same session
//! feeds the view its page's events, so the engine's one event stream is read exactly once.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_page_engine::{PageSession, codec};
use tilo_webview::{SharedEngine, WebViewCommand};
use ui::webview;

/// The page of one window, made when the window first needs it.
pub type Page = Rc<RefCell<Option<PageSession>>>;

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

/// What asking the page left to do.
pub enum Asked {
    /// The operation answered at once (a navigation, a zoom): what the app reads.
    Answered(String),
    /// The page was asked; its answer comes with the action `action`.
    Waiting(u32),
}

/// Ask the page `request` (JSON an app's action wrote) to do what it says.
///
/// # Errors
/// The request is no JSON the page reads, or the window keeps no page.
pub fn ask(page: &Page, request: &str) -> Result<Asked, String> {
    match codec::decode(request)? {
        // A page opened private starts its window's engine on a fresh, disposable profile. It is
        // the window's to make before anything else is asked, since it settles which engine the
        // page has; a page that already started keeps the engine it has.
        codec::Ask::Private => {
            start_private(page)?;
            Ok(Asked::Answered(codec::accepted()))
        }
        // The sites the person brought their sign-ins of open signed in in the page's profile,
        // which names beside itself the sign-ins it holds.
        codec::Ask::SignIns => {
            let _ = engine_of(page);
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(profile) = tilo_webview::chromium::browsing_profile() {
                session_of(page, |session| {
                    session.open_sign_ins(profile.join(SIGN_INS_HELD));
                })?;
            }
            Ok(Asked::Answered(codec::accepted()))
        }
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

/// Start the window's page on a fresh, disposable profile, unless it already has one.
fn start_private(page: &Page) -> Result<(), String> {
    let mut held = page.borrow_mut();
    if held.is_some() {
        return Ok(());
    }
    let root = tilo_webview::chromium::allocate_private_root()?;
    let engine = match ui::webview::new_private_page_engine(&root) {
        Ok(engine) => engine,
        Err(why) => {
            tilo_webview::chromium::release_private_root(&root);
            return Err(why);
        }
    };
    held.replace(PageSession::new_private(engine, root));
    Ok(())
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
