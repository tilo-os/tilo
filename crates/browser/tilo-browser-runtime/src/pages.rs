//! Another device's window page, drawn in this page's desktop.
//!
//! A window whose app runs a page engine hands its page over as a display
//! list, and the workspace's live plane carries it as one track per window
//! (`tilo-page/1`). This page draws the windows other devices host: it asks
//! its host — the session this page's peer is with — for exactly the tracks of
//! the windows on its desk ([`follow`]), and what those tracks bring rebuilds
//! the frame the hosting device holds ([`heard`]). The tile itself is the
//! toolkit's page view (`tilo_desktop_surface::page_view`), the same one the
//! native desk draws a page with; its code is in a module of its own, so a
//! desk that never shows a hosted window never loads it ([`PAGE`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use tilo_session::sync::{Cmd, Outbox};

use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::page_code::{PAGE, PageHandle};
use ui::widget::{TileContent, TileHost};

use crate::peer_sync;

/// Payloads one page keeps while the page view's code is on its way: the
/// first whole frame and what follows it, not a page's history.
const MAX_WAITING: usize = 64;

/// The pages this page draws, by the tab each window stands in.
#[derive(Default)]
struct Pages {
    tiles: HashMap<u64, Tile>,
    /// What this page last asked its host for, by the device hosting the windows: only a change
    /// reaches the host, since an ask says everything this page shows of that device.
    asked: BTreeMap<[u8; 32], Vec<u64>>,
}

struct Tile {
    /// The page's viewer, once the page view's code is in.
    page: Option<Rc<dyn PageHandle>>,
    /// What came before it: the payloads, so the publisher's first whole frame
    /// is not lost to the fetch.
    waiting: Vec<Vec<u8>>,
    /// What the tile is called, for a reader who cannot see the page.
    title: String,
}

impl Tile {
    /// The viewer, opened the first time a tile asks for one and handed
    /// everything that waited. The first ask loads the page view's code.
    fn open(&mut self) -> Option<Rc<dyn PageHandle>> {
        if self.page.is_none() {
            let page = PAGE.get()?.open();
            for object in self.waiting.drain(..) {
                page.apply(&object);
            }
            self.page = Some(page);
        }
        self.page.clone()
    }

    /// Take one object of the page. Whether the frame the tile shows changed.
    fn apply(&mut self, payload: &[u8]) -> bool {
        if let Some(page) = &self.page {
            return page.apply(payload);
        }
        if self.waiting.len() >= MAX_WAITING {
            self.waiting.remove(0);
        }
        self.waiting.push(payload.to_vec());
        false
    }
}

thread_local! {
    static PAGES: RefCell<Pages> = RefCell::new(Pages::default());
}

/// Ask the host — the session this page's peer `id` is with — for the pages of the windows this
/// desk holds, `workspace`'s, and stop reading the devices left out. Whether what it asks changed.
pub(crate) fn follow(id: u32, workspace: &str, desk: &DesktopHandle) -> bool {
    let Ok(workspace) = crate::peer::workspace_number(workspace) else {
        return false;
    };
    let Some(handle) = peer_sync::handle_of(id) else {
        return false;
    };
    let mut shown: BTreeMap<[u8; 32], Vec<u64>> = BTreeMap::new();
    for (tab, host) in desk.hosted_windows() {
        let Some(host) = hex::decode(&host)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        else {
            continue;
        };
        shown.entry(host).or_default().push(tab);
    }
    let changed = PAGES.with_borrow_mut(|pages| {
        if pages.asked == shown {
            return false;
        }
        pages.asked.clone_from(&shown);
        true
    });
    if changed {
        let shown: Vec<tilo_session::sync::PageShown> = shown
            .into_iter()
            .map(|(device, tabs)| tilo_session::sync::PageShown { device, tabs })
            .collect();
        // A host that took the ask answers by publishing the pages: nothing here waits on it.
        let _ = handle.send(Cmd::ShowPages(workspace, shown));
    }
    changed
}

/// Take one object of `from`'s page for window `tab`, in `workspace_id`'s plane: it rebuilds the
/// frame the tile draws, and the mounts drawing that workspace lay out again.
pub(crate) fn heard(id: u32, workspace_id: u64, tab: u64, body: &[u8]) {
    let changed = PAGES.with_borrow_mut(|pages| {
        pages
            .tiles
            .entry(tab)
            .or_insert_with(|| Tile {
                page: None,
                waiting: Vec::new(),
                title: String::new(),
            })
            .apply(body)
    });
    if changed {
        crate::browser::presence::page_arrived(id, workspace_id, tab);
    }
}

/// The tile of window `tab` while this page draws its page: the page itself, or the arriving card
/// until its publisher's first whole frame comes. `None` while nothing of it is here, or while the
/// page view's code is still on its way, so the host's own tile answers that tab.
pub(crate) fn tile(tab: u64, title: &str) -> Option<TileHost> {
    let (page, alt) = PAGES.with_borrow_mut(|pages| {
        let entry = pages.tiles.get_mut(&tab)?;
        let alt = if title.is_empty() {
            entry.title.clone()
        } else {
            title.to_owned()
        };
        Some((entry.open()?, alt))
    })?;
    Some(TileHost::new(TileContent::App(page.tile(alt))))
}
