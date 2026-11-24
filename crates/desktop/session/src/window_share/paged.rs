//! A window shared as its page, on the device that runs it.
//!
//! A window whose app draws its page is shared as that page and never as pictures of it: the
//! pictures show whoever watches what the page masks, a secret field among it. Which of the two a
//! window is the session decides every pass, never once when its share starts, from what it
//! knows of the window's app at that pass ([`Drawn`]):
//!
//! - its app offered the window's page ([`PageTracks::offers`]): the share is the page's;
//! - its app said, on any of its connections, that a window of its runs a page engine
//!   ([`PageTracks::draws_pages`]), or its installed package says a window of it is its page
//!   ([`WindowShares::pages_declared_by`]), and this window's page is not offered yet: the share
//!   waits for the page and says so (`starting`, `awaits_page`), and nothing of the window is
//!   captured;
//! - otherwise the window is shared as pictures.
//!
//! An app says it draws pages on a connection that names no window, so its offer is taken up
//! only once the session knows which window speaks ([`crate::app_tabs`]), and an app whose
//! package this device holds none of says nothing before that. A share that started as pictures
//! in that time is taken over the pass the session learns its app draws pages
//! ([`WindowShares::settle_pages`]): the pictures end, their viewers are told, and the window is
//! its page's from then on, or waits for it.
//!
//! The page goes out in the workspace the window is shared with, whatever name its app's launch
//! filed the offer under: the share says who is shown the window
//! ([`PageTracks::step`]).

use std::collections::{BTreeMap, BTreeSet};

use iroh::EndpointId;
use neighbour_wire::PeerId;
use tilo_compositor::Compositor;
use tilo_desktop_model::window_share::mapped_window;
use tilo_desktop_model::{Layout, TabId};
use tilo_live::share::{Control, Said};
use tilo_session_core::desktop::i18n::t;

use super::{Starting, WindowShares};
use crate::page_track::PageTracks;
use crate::sync::Client;

/// A window of this device whose page this session publishes for the members that show it: the
/// app draws it, so no picture of it is captured or encoded here.
pub(super) struct Paged {
    /// The tab the window stands in on every device of the workspace, and the surface its
    /// announcement names.
    pub(super) tab: TabId,
    /// The compositor's identifier of the window, as the share named it.
    pub(super) window: String,
    pub(super) app_id: String,
    pub(super) workspace: String,
    /// The window's size, which the announcement carries so a viewer reserves its place before the
    /// page's first frame.
    pub(super) width: u32,
    pub(super) height: u32,
    /// The viewers the announcement went to, and their devices: the ones a page's track is
    /// published for.
    pub(super) announced: BTreeMap<PeerId, EndpointId>,
}

impl Paged {
    /// The announcement of this window's page, to `device`: the window travels as its page.
    fn announcement(&self, device: EndpointId) -> Said {
        Said {
            to: *device.as_bytes(),
            control: Control::Page {
                tab: self.tab,
                app_id: self.app_id.clone(),
                width: self.width,
                height: self.height,
            },
        }
    }

    /// The end of it, for a viewer that stops showing the window or a share that ends.
    fn end_to(&self, device: EndpointId) -> Said {
        Said {
            to: *device.as_bytes(),
            control: Control::Ended { tab: self.tab },
        }
    }
}

/// How a window this device shares, or is asked to, stands with its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawn {
    /// Its app offered its page: the window is shared as that page.
    Offered,
    /// Its app draws its windows' pages and has not offered this one's: the share waits for it.
    Awaited,
    /// Its app draws no page: the window is shared as pictures.
    Pictures,
}

/// Which apps' installed packages say a window of theirs is its page, as far as each was asked.
/// A package is read once a session: a share asks every pass while it stands.
#[derive(Default)]
pub(super) struct Declared {
    says: Option<fn(&str) -> bool>,
    known: BTreeMap<String, bool>,
}

impl Declared {
    /// Whether the package of `app` says a window of it is its page; nothing is said where
    /// nobody reads packages.
    fn of(&mut self, app: &str) -> bool {
        let Some(says) = self.says else {
            return false;
        };
        if let Some(known) = self.known.get(app) {
            return *known;
        }
        let known = says(app);
        self.known.insert(app.to_owned(), known);
        known
    }
}

impl WindowShares {
    /// Read from `says` whether an app's installed package declares that a window of it is its
    /// page ([`crate::apps::window_is_its_page`]). A window of such an app is known to draw its
    /// page before its app said anything, so its share waits for the page from the first pass
    /// and no picture of it is ever captured.
    pub(crate) fn pages_declared_by(&mut self, says: fn(&str) -> bool) {
        self.declared = Declared {
            says: Some(says),
            known: BTreeMap::new(),
        };
    }

    /// The windows this session shares as their page, each with the workspace it is shared
    /// with: the only ones whose page leaves the device, and only into that workspace
    /// ([`PageTracks::step`]).
    pub(crate) fn paged(&self) -> BTreeMap<TabId, String> {
        self.pages
            .iter()
            .map(|(tab, page)| (*tab, page.workspace.clone()))
            .collect()
    }

    /// How the window `tab`, shared as a window of the app `named`, stands with its page now.
    fn drawn(&mut self, tree: &Layout, pages: &PageTracks, tab: TabId, named: &str) -> Drawn {
        if pages.offers(tab) {
            return Drawn::Offered;
        }
        // The app the layout says the tab's window belongs to speaks for the window as much as
        // the one its share named: whoever asks for a share may name the app itself.
        let mapped = mapped_window(&tree.workspaces, tab).map(|window| window.app_id.to_owned());
        let awaited = [Some(named), mapped.as_deref()]
            .into_iter()
            .flatten()
            .any(|app| pages.draws_pages(app) || self.declared.of(app));
        if awaited {
            Drawn::Awaited
        } else {
            Drawn::Pictures
        }
    }

    /// Settle which shares are their window's page, before any share starts this pass.
    ///
    /// A window shared as pictures whose app is now known to draw its page stops going out as
    /// pictures at once: its stream ends, its viewers are told, nobody controls it any more (a
    /// page takes no input), and its share starts over as the page's. A share that waits to
    /// start becomes the page's as soon as the page is offered, and until then is marked as one
    /// that waits for its page ([`Self::wait_for_page`]), which nothing starts as pictures.
    pub(super) fn settle_pages(
        &mut self,
        compositor: &mut Compositor,
        tree: &Layout,
        sync: &Client,
        pages: &PageTracks,
    ) {
        let pictured: Vec<(TabId, String)> = self
            .hosted
            .iter()
            .map(|(tab, hosted)| (*tab, hosted.app_id.clone()))
            .collect();
        for (tab, app_id) in pictured {
            if self.drawn(tree, pages, tab, &app_id) == Drawn::Pictures {
                continue;
            }
            let Some(hosted) = self.hosted.remove(&tab) else {
                continue;
            };
            tracing::info!(
                tab,
                app = %hosted.app_id,
                "a window shared as pictures is shared as its page from here on"
            );
            self.end(&hosted, tab, sync);
            self.renegotiate.remove(&tab);
            self.close_seats(compositor, tab, None);
            self.starting.push(Starting {
                window: hosted.window,
                tab,
                app_id: hosted.app_id,
                workspace: hosted.workspace,
                waiting_for: String::new(),
                controlled: BTreeSet::new(),
                page: true,
            });
            self.changed();
        }
        for mut start in std::mem::take(&mut self.starting) {
            let drawn = self.drawn(tree, pages, start.tab, &start.app_id);
            start.page = drawn != Drawn::Pictures;
            match sync.workspace_of(&start.workspace) {
                Some(_) if drawn == Drawn::Offered => {
                    self.pages.insert(start.tab, page_of(compositor, &start));
                    self.changed();
                }
                _ => self.starting.push(start),
            }
        }
    }

    /// Keep `start` waiting for its window's page, and say so: its app draws its windows' pages
    /// and has not offered this one's, and a picture of the window is not what is shared.
    pub(super) fn wait_for_page(&mut self, mut start: Starting) {
        let waiting_for = t("session-window-share-page-unoffered");
        if start.waiting_for != waiting_for {
            start.waiting_for = waiting_for;
            self.changed();
        }
        self.starting.push(start);
    }

    /// Announce the windows whose page this session publishes to the members showing them, and tell
    /// the ones that stopped showing it. Nothing of a page is captured or encoded here: its app
    /// draws it, and the live session carries it.
    pub(super) fn announce_pages(&mut self, sync: &Client) {
        let tabs: Vec<TabId> = self.pages.keys().copied().collect();
        for tab in tabs {
            let viewers: BTreeMap<PeerId, EndpointId> = self
                .asked
                .iter()
                .filter(|((asking, _), _)| *asking == tab)
                .map(|((_, peer), asked)| (*peer, asked.device))
                .collect();
            let Some(page) = self.pages.get(&tab) else {
                continue;
            };
            let mut said = Vec::new();
            let mut announced = page.announced.clone();
            let dropped: Vec<(PeerId, EndpointId)> = announced
                .iter()
                .map(|(peer, device)| (*peer, *device))
                .filter(|(peer, _)| !viewers.contains_key(peer))
                .collect();
            for (peer, device) in dropped {
                announced.remove(&peer);
                said.push(page.end_to(device));
            }
            for (peer, device) in &viewers {
                if announced.insert(*peer, *device).is_none() {
                    said.push(page.announcement(*device));
                }
            }
            sync.say(&page.workspace, &said);
            if let Some(page) = self.pages.get_mut(&tab) {
                page.announced = announced;
            }
        }
    }

    /// Send the end of `page` 's announcement to the viewers it went to, so each drops the window
    /// rather than reading a track nothing publishes any more.
    pub(super) fn end_page(page: &Paged, sync: &Client) {
        let ended: Vec<Said> = page
            .announced
            .values()
            .map(|device| page.end_to(*device))
            .collect();
        sync.say(&page.workspace, &ended);
    }
}

/// The page of a window whose app draws it: the members showing it read the workspace's live
/// live session, so no picture of it is captured or encoded here.
fn page_of(compositor: &Compositor, start: &Starting) -> Paged {
    let (_, _, width, height) = compositor
        .shareable_window_rect(&start.window)
        .unwrap_or_default();
    Paged {
        tab: start.tab,
        window: start.window.clone(),
        app_id: start.app_id.clone(),
        workspace: start.workspace.clone(),
        width: width.unsigned_abs(),
        height: height.unsigned_abs(),
        announced: BTreeMap::new(),
    }
}
