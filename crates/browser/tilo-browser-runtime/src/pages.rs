//! Another device's window page, drawn in this page's desktop.
//!
//! A window whose app runs a page engine hands its page over as a display
//! list, and the workspace's live plane carries it as one track per window
//! (`tilo-page/1`). This page draws the windows other devices host: it asks
//! its host — the session this page's peer is with — for exactly the tracks of
//! the windows on its desk ([`follow`]), and what those tracks bring rebuilds
//! the frame the hosting device holds ([`heard`]). The tile itself is the
//! toolkit's [`PageView`](tilo_desktop_surface::page_view::PageStream), the
//! same one the native desk draws a page with.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::page_view::PageStream;
use ui::widget::{TileContent, TileHost};

use crate::peer_sync;

/// The pages this page draws, by the tab each window stands in.
#[derive(Default)]
struct Pages {
    tiles: HashMap<u64, Tile>,
    /// What this page last asked its host for, by the device hosting the windows: only a change
    /// reaches the host, since an ask says everything this page shows of that device.
    asked: BTreeMap<[u8; 32], Vec<u64>>,
}

struct Tile {
    stream: Rc<RefCell<PageStream>>,
    /// What the tile is called, for a reader who cannot see the page.
    title: String,
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
        let shown: Vec<tilo_session_host::PageShown> = shown
            .into_iter()
            .map(|(device, tabs)| tilo_session_host::PageShown { device, tabs })
            .collect();
        // A host that took the ask answers by publishing the pages: nothing here waits on it.
        let _ = handle.show_pages(workspace, shown);
    }
    changed
}

/// Take one object of `from`'s page for window `tab`, in `workspace_id`'s plane: it rebuilds the
/// frame the tile draws, and the mounts drawing that workspace lay out again.
pub(crate) fn heard(id: u32, workspace_id: u64, tab: u64, body: &[u8]) {
    let changed = PAGES.with_borrow_mut(|pages| {
        let stream = Rc::clone(
            &pages
                .tiles
                .entry(tab)
                .or_insert_with(|| Tile {
                    stream: Rc::new(RefCell::new(PageStream::new())),
                    title: String::new(),
                })
                .stream,
        );
        stream.borrow_mut().apply(body)
    });
    if changed {
        crate::browser::presence::page_arrived(id, workspace_id, tab);
    }
}

/// The tile of window `tab` while this page draws its page: the page itself, or the arriving card
/// until its publisher's first whole frame comes. `None` while nothing of it is here, so the host's
/// own tile answers that tab.
pub(crate) fn tile(tab: u64, title: &str) -> Option<TileHost> {
    let stream = PAGES.with_borrow(|pages| {
        let tile = pages.tiles.get(&tab)?;
        Some(Rc::clone(&tile.stream))
    })?;
    let alt = if title.is_empty() {
        PAGES
            .with_borrow(|pages| pages.tiles.get(&tab).map(|tile| tile.title.clone()))
            .unwrap_or_default()
    } else {
        title.to_owned()
    };
    Some(TileHost::new(TileContent::App(Box::new(
        tilo_desktop_surface::page_view::PageView::new(stream, alt),
    ))))
}
