//! Files of the workspace carried out of the page onto the computer the browser runs on
//! ([`ui::carry_out`]): dragged onto its desktop or file manager, or saved.
//!
//! The page never holds the bytes. It asks the workspace's host, over its session, for a link
//! the host's web origin serves the files at for a few minutes
//! ([`tilo_actions::remote::ContentLink`]), and the browser fetches that:
//!
//! - **Saving.** The browser's Save dialog asks where (File System Access, in Chrome and Edge)
//!   and the page pipes the link's response into the file it opened, a buffer at a time; a
//!   browser without one (Firefox, Safari) downloads the link itself, streamed to disk by its
//!   own download manager. Either way a file of a gigabyte takes the page no more memory than a
//!   small one.
//! - **Dragging out.** Chrome and Edge, on Windows and macOS, save a drag that carries a
//!   `DownloadURL` where it is dropped, fetching the URL with their own download manager. The
//!   drag starts in the page's semantic mirror (`carry-out.mjs`), which asks for the row's link
//!   as the pointer comes near it ([`Carrying::link`]): the drag has to name it the moment it
//!   starts. Firefox and Safari, and Chrome on Linux, take no such drag; the drag then stays the
//!   window's own.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_actions::remote::ContentLinkAsk;
use tilo_desktop_surface::notifications::{Notification, NotificationSeverity};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/web/carry-out.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = chooseCarryTarget, catch)]
    fn choose_carry_target(name: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(js_name = saveFromLink, catch)]
    fn save_from_link(target: &JsValue, url: &str) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(js_name = nowMs)]
    fn now_ms() -> f64;
}

/// How long before a link expires the page stops handing it to a drag: a drop a few seconds
/// after the drag began still finds it good.
const MARGIN_MS: f64 = 60_000.0;

/// Where a row's link stands.
#[derive(Clone)]
enum Link {
    /// Asked of the host, not answered yet.
    Asking,
    /// The address the host serves it at, until when (page milliseconds).
    Ready { url: String, until: f64 },
    /// The host refused or did not answer; asked again after this (page milliseconds).
    Refused { again: f64 },
}

/// What a drag out of the page names: the address, its media type and the name it is saved
/// under (the `DownloadURL` a drag carries is `mime:name:url`).
pub(crate) struct DragLink {
    pub(crate) url: String,
    pub(crate) mime: &'static str,
    pub(crate) name: String,
}

/// The page's way out for the workspace's files: the session's door to the host, and the
/// links it handed out so far.
pub(crate) struct Carrying {
    inner: Rc<Inner>,
    _served: ui::carry_out::CarryGuard,
}

struct Inner {
    shared: crate::desktop::Shared,
    workspace: String,
    /// The origin of the host's web listener, where its links are served: `None` for a page with
    /// no host.
    origin: Option<String>,
    /// Each offered row's link, by its offer's key.
    links: RefCell<HashMap<String, Link>>,
    /// The shell's notices, where a save that fails is told.
    notifications: tilo_desktop_surface::DesktopNotifications,
}

impl Carrying {
    /// Carry the windows' files out of the page through the host of `workspace`, whose web
    /// origin is `origin`.
    pub(crate) fn new(
        shared: crate::desktop::Shared,
        workspace: String,
        origin: Option<String>,
        notifications: tilo_desktop_surface::DesktopNotifications,
    ) -> Self {
        let inner = Rc::new(Inner {
            shared,
            workspace,
            origin: origin.map(|origin| origin.trim_end_matches('/').to_owned()),
            links: RefCell::default(),
            notifications,
        });
        let served = ui::carry_out::serve(Rc::new(Host(Rc::clone(&inner))));
        Self {
            inner,
            _served: served,
        }
    }

    /// The link a drag of the row offered under `key` names, when the host already answered for
    /// it; asks for it otherwise, so a drag a moment later finds it.
    pub(crate) fn link(&self, key: &str) -> Option<DragLink> {
        let ask = ui::carry_out::offered(key)?;
        let now = now_ms();
        let known = self.inner.links.borrow().get(key).cloned();
        match known {
            Some(Link::Ready { url, until }) if until - MARGIN_MS > now => {
                return Some(DragLink {
                    url,
                    mime: if ask.zip {
                        "application/zip"
                    } else {
                        "application/octet-stream"
                    },
                    name: ask.name,
                });
            }
            Some(Link::Asking) => return None,
            Some(Link::Refused { again }) if again > now => return None,
            _ => {}
        }
        self.inner
            .links
            .borrow_mut()
            .insert(key.to_owned(), Link::Asking);
        let inner = Rc::clone(&self.inner);
        let key = key.to_owned();
        wasm_bindgen_futures::spawn_local(async move {
            let answered = inner.mint(ask).await;
            let now = now_ms();
            let link = match answered {
                Ok((url, expires_in_secs)) => {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a link lives minutes, far below what a float holds exactly"
                    )]
                    let until = now + expires_in_secs as f64 * 1000.0;
                    Link::Ready { url, until }
                }
                Err(why) => {
                    tracing::info!("no link to drag {key} out with: {why}");
                    Link::Refused {
                        again: now + 10_000.0,
                    }
                }
            };
            inner.links.borrow_mut().insert(key, link);
        });
        None
    }
}

impl Inner {
    /// Ask the host for a link to `ask`'s files: its address and how long it stays good.
    async fn mint(&self, ask: ContentLinkAsk) -> Result<(String, u64), String> {
        let origin = self
            .origin
            .clone()
            .ok_or_else(|| "This page isn't connected to the workspace.".to_owned())?;
        let door = self
            .shared
            .peer
            .zip(crate::peer::workspace_id(&self.workspace).ok())
            .and_then(|(peer, id)| crate::peer_sync::actions_of(peer, id))
            .ok_or_else(|| "This page isn't connected to the workspace.".to_owned())?;
        let link = door
            .content_link(ask)
            .await
            .ok_or_else(|| "The workspace stopped answering.".to_owned())?
            .map_err(|error| error.to_string())?;
        Ok((format!("{origin}{}", link.path), link.expires_in_secs))
    }

    /// Show `name` did not reach the person's computer, and why. A save that works says nothing:
    /// the Save dialog or the browser's download manager is where the person watched it.
    fn not_saved(&self, name: &str, why: &str) {
        let mut notice = Notification::new(format!("{name} was not saved"), why);
        notice.severity = NotificationSeverity::Error;
        self.notifications.post(notice);
        self.shared.wake();
    }
}

/// The host a window's [`ui::carry_out::save`] reaches.
struct Host(Rc<Inner>);

impl ui::carry_out::CarryHost for Host {
    fn save(&self, ask: ContentLinkAsk) {
        // The Save dialog first, inside the click's gesture, which the browser asks for it.
        let target = match choose_carry_target(&ask.name) {
            Ok(target) => target,
            Err(error) => {
                self.0.not_saved(&ask.name, &js_reason(&error));
                return;
            }
        };
        let inner = Rc::clone(&self.0);
        let name = ask.name.clone();
        wasm_bindgen_futures::spawn_local(async move {
            inner.shared.wake();
            if let Err(why) = save(&inner, ask, target).await {
                inner.not_saved(&name, &why);
            }
            inner.shared.wake();
        });
    }
}

/// What a browser's error says, in the words it gave.
fn js_reason(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(error, &"message".into())
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "The browser refused.".to_owned())
}

/// Mint `ask`'s link and save it to `target` (the dialog's answer): why it did not, when it did
/// not; the person cancelling the dialog is not a failure.
async fn save(inner: &Inner, ask: ContentLinkAsk, target: js_sys::Promise) -> Result<(), String> {
    let (url, _) = inner.mint(ask).await?;
    let target = wasm_bindgen_futures::JsFuture::from(target)
        .await
        .map_err(|error| js_reason(&error))?;
    if target.is_null() || target.is_undefined() {
        return Ok(());
    }
    let saving = save_from_link(&target, &url).map_err(|error| js_reason(&error))?;
    wasm_bindgen_futures::JsFuture::from(saving)
        .await
        .map_err(|error| js_reason(&error))?;
    Ok(())
}
