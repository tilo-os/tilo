//! What a pass on the desk reports: the revision a host saves and shares against, and the count of
//! what the person sees changing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{DesktopTree, IdMinter};

use super::shown::ShownTabs;
use super::{Presentation, Shared};

impl Shared {
    /// A change a host saves and shares: the revision, and a wake for the host holding this desk.
    pub(super) fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.wake.wake();
    }

    /// What the person sees changed, or did not.
    pub(super) fn present(&mut self, presentation: Presentation) {
        if self.presentation != presentation {
            self.presentation = presentation;
            self.presented = self.presented.wrapping_add(1);
        }
    }

    /// The desk's shared state at the start: the tree it presents, the minter for this device's new
    /// ids, and the search it answers in.
    pub(super) fn new(
        tree: DesktopTree,
        minter: IdMinter,
        search: &Rc<RefCell<tilo_desktop_search::LazyDesktopSearch>>,
    ) -> Self {
        Self {
            tree,
            holders: HashMap::new(),
            window_offers: Vec::new(),
            asking: std::collections::BTreeSet::new(),
            opens: true,
            revision: 1,
            queue: Vec::new(),
            activity_focus: None,
            minter,
            viewport_width: ui::widget::DEFAULT_STRIP_USABLE_PX,
            replaced: false,
            shown_tabs: ShownTabs::default(),
            shown_strip: None,
            hint_places: ui::widget::HintPlaces::default(),
            focused_presence: Vec::new(),
            rebuild: Vec::new(),
            search_request: None,
            search_open: false,
            search: Rc::clone(search),
            search_revision: 0,
            search_area: None,
            host_actions: Vec::new(),
            host_choices: Vec::new(),
            menus: Vec::new(),
            search_host: None,
            search_choices: Vec::new(),
            search_choice_context: None,
            search_refusal: None,
            search_asks: Vec::new(),
            shown_search: super::search::ShownSearch::default(),
            typed_field: Err(super::search::TypedRefusal::NoField),
            typed_ask: None,
            personal_ask: None,
            personal_ask_revision: 0,
            arrivals: Vec::new(),
            presentation: Presentation::default(),
            presented: 0,
            artwork: std::collections::BTreeMap::new(),
            withheld: std::collections::BTreeSet::new(),
            unresponsive: std::collections::BTreeSet::new(),
            unresponsive_revision: 0,
            liveness: Vec::new(),
            acknowledger: ui::widget::acknowledgement::Acknowledger::default(),
            #[cfg(all(not(target_arch = "wasm32"), feature = "peer-view"))]
            peers: Rc::new(RefCell::new(super::peer_view::PeerSurfaces::default())),
            bars: super::window_bars::Bars::default(),
            sideways: Vec::new(),
            wake: crate::wake::Waker::default(),
        }
    }
}
