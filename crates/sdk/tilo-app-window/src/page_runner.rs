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
        held.replace(PageSession::new(engine.clone()));
        engine
    }
}

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
            let mut held = page.borrow_mut();
            if held.is_some() {
                return Ok(Asked::Answered(codec::accepted()));
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
        // A credential action names the page's document, which is the session's to know and the
        // app's never to see: the session asks the page for it and sends the action after.
        codec::Ask::Command(WebViewCommand::Credentials(credential)) => {
            let mut held = page.borrow_mut();
            let session = held
                .as_mut()
                .ok_or_else(|| "this window has no page".to_owned())?;
            session.credential(credential);
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
/// host path names no content a sandboxed app may hold.
fn read_package(read: impl Fn(&str) -> Option<Vec<u8>>, file: &str) -> Result<Vec<u8>, String> {
    let hash = file
        .strip_prefix("tilo-blob://")
        .ok_or_else(|| format!("`{file}` is no file's address"))?;
    read(hash).ok_or_else(|| "the package is not on this device".to_owned())
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
