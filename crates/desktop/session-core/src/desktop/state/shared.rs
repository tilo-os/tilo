//! What a pass on the shell reports: the revision a host saves and shares against, and the count of
//! what the person sees changing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{IdMinter, Layout};

use super::shown::ShownTabs;
use super::{Presentation, Shared};

impl Shared {
    /// A change a host saves and shares: the revision, and a wake for the host holding this shell.
    pub fn changed(&mut self) {
        self.note_change();
        self.wake.wake();
    }

    /// A change of what a peer reads of this shell that nothing draws again for: a window's name,
    /// which its app's bar names (the surface publishes them).
    pub fn note_change(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Say `notice` over the windows for a while ([`super::DesktopHandle::notice`]): a notice with
    /// a button stays long enough to reach it.
    pub fn say(&self, notice: ui::window_toolbar::ToolbarNotice) {
        let lifetime = if notice.actions.is_empty() {
            ui::widget::toast::NOTICE_LIFETIME
        } else {
            ui::widget::toast::ACTION_NOTICE_LIFETIME
        };
        self.notice.borrow_mut().show_for(notice, lifetime);
        self.wake.wake();
    }

    /// Close the window `tab` as the person does, saying what closed with the Undo that reopens
    /// it (the reopen Shift+Super+T makes).
    pub fn close(
        &mut self,
        tab: tilo_desktop_model::TabId,
    ) -> Option<tilo_desktop_model::RemovedTab> {
        let removed = self.tree.close_tab(tab)?;
        let said = crate::desktop::i18n::t_args(
            "shell-window-closed",
            &[(
                "window",
                super::tile_content::window_name(&removed.tab).into(),
            )],
        );
        self.say(
            ui::window_toolbar::ToolbarNotice::new(said, ui::widget::toast::ToastKind::Info)
                .action(ui::window_toolbar::ToolbarAction::new(
                    super::REOPEN_CLOSED,
                    "undo-2",
                    crate::desktop::i18n::t("shell-window-closed-undo"),
                )),
        );
        Some(removed)
    }

    /// What the person sees changed, or did not.
    pub fn present(&mut self, presentation: Presentation) {
        if self.presentation != presentation {
            self.presentation = presentation;
            self.presented = self.presented.wrapping_add(1);
        }
    }

    /// The shell's shared state at the start: the tree it presents, the minter for this device's new
    /// ids, and the search it answers in.
    pub fn new(
        tree: Layout,
        minter: IdMinter,
        search: &Rc<RefCell<tilo_desktop_search::DesktopSearch>>,
    ) -> Self {
        Self {
            tree,
            cards_born: std::collections::BTreeSet::new(),
            asked_floats: Vec::new(),
            holders: HashMap::new(),
            opened_by: std::collections::BTreeMap::new(),
            authors: std::collections::BTreeMap::new(),
            pointer_faces: HashMap::new(),
            ended: std::collections::BTreeMap::new(),
            opened_for: std::collections::BTreeMap::new(),
            resources: std::collections::BTreeMap::new(),
            items: std::collections::BTreeMap::new(),
            window_offers: Vec::new(),
            workspace_shares: None,
            workspace_share_asks: Vec::new(),
            other_screens: Vec::new(),
            device_moves: Vec::new(),
            rules: None,
            asks: None,
            asking: std::collections::BTreeSet::new(),
            opens: true,
            revision: 1,
            queue: Vec::new(),
            activity_focus: None,
            minter,
            viewport_width: ui::widget::DEFAULT_STRIP_USABLE_PX,
            displays: Vec::new(),
            pans: HashMap::new(),
            reveal_asked: Vec::new(),
            top_overlay: 0.0,
            replaced: false,
            shown_tabs: ShownTabs::default(),
            placed: None,
            carried: None,
            float_covers: Vec::new(),
            shown_strip: None,
            focused_presence: Vec::new(),
            rebuild: Vec::new(),
            search_request: None,
            search_open: false,
            typed_ahead: super::search::TypedAhead::default(),
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
            drawn: std::collections::BTreeSet::new(),
            typed_field: Err(super::search::TypedRefusal::NoField),
            typed_ask: None,
            personal_ask: None,
            personal_ask_revision: 0,
            arrivals: Vec::new(),
            revealing: super::reveals::Revealing::default(),
            followed: None,
            presentation: Presentation::default(),
            presented: 0,
            artwork: std::collections::BTreeMap::new(),
            withheld: std::collections::BTreeSet::new(),
            unresponsive: std::collections::BTreeSet::new(),
            unresponsive_revision: 0,
            window_sizes: std::collections::BTreeMap::new(),
            window_sizes_revision: 0,
            liveness: Vec::new(),
            notice: Rc::default(),
            notice_actions: RefCell::default(),
            notifications: None,
            peers: None,
            bars: super::window_bars::Bars::default(),
            sideways: Vec::new(),
            wake: crate::desktop::wake::Waker::default(),
            publish: None,
            published: 1,
        }
    }
}
