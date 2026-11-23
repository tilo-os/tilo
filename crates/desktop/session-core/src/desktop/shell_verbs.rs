//! The shell's own published side, `desktop:shell` and `desktop:launchpad`: its windows and
//! displays, the open search and the empty shell's launchpad, and the verbs on them, carried out on
//! the desktop the session owns. Whatever draws the screen reads the same resource and asks through
//! the same verbs.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_model::Layout;
use tilo_desktop_model::layout::TileContent;
use ui::search::Scope;

use crate::desktop::host::contract::DesktopAsk;
use crate::desktop::launchpad::LaunchpadAction;
use crate::desktop::navigation::{LayoutEffect, Verb};
use crate::desktop::state::DesktopHandle;
use crate::desktop::verbs::{self, OpenSearch, launchpad_resource, shell_resource};

/// The launchpad's cards, as the host offers them now.
pub type StartCards = Rc<dyn Fn() -> Vec<LaunchpadAction>>;

/// Whether the shell in front shows no window: the strip shown in the workspace in front is empty,
/// so the launchpad offers what to start with.
#[must_use]
pub fn shell_is_empty(tree: &Layout) -> bool {
    tree.active_workspace().is_some_and(|workspace| {
        tree.active_strip_for(workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
            .is_none_or(|strip| strip.window_groups.is_empty())
    })
}

/// Ask the session `asked`, and wake the desktop for it.
pub fn ask(handle: &DesktopHandle, asks: &Rc<RefCell<Vec<DesktopAsk>>>, asked: DesktopAsk) {
    asks.borrow_mut().push(asked);
    handle.wake();
}

/// The shell's published side over the desktop `handle` holds: what the session asks for lands in
/// `asks`.
pub struct ShellVerbs {
    handle: DesktopHandle,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    start_cards: StartCards,
}

impl ShellVerbs {
    #[must_use]
    pub fn new(
        handle: DesktopHandle,
        asks: Rc<RefCell<Vec<DesktopAsk>>>,
        start_cards: StartCards,
    ) -> Self {
        Self {
            handle,
            asks,
            start_cards,
        }
    }

    /// The shell's resource, and the launchpad's while the shell in front shows no window.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        let windows: Vec<verbs::Shown> = self.handle.read(|tree| {
            tree.tabs()
                .map(|tab| {
                    let title = crate::desktop::state::window_name(tab);
                    // A window the session does not run is read of through the shell, unless its
                    // app keeps what it shows to itself: the address is not offered, so no row
                    // names a picture nobody may make.
                    let foreign = match &tab.tile {
                        TileContent::Foreign { app_id, .. } if !self.handle.withholds(app_id) => {
                            Some(app_id.clone())
                        }
                        _ => None,
                    };
                    verbs::Shown {
                        tab: tab.id,
                        title,
                        foreign,
                    }
                })
                .collect()
        });
        let (query, rows) = self.handle.search_rows();
        let search = self.handle.search_open().then_some(OpenSearch {
            query: query.as_str(),
            rows: rows.as_slice(),
            searching: self.handle.search_searching(),
        });
        let hung = self.handle.unresponsive_tabs();
        let mut resources = vec![shell_resource(
            &windows,
            &hung,
            &self.handle.displays(),
            search,
        )];
        if self.handle.read(shell_is_empty) {
            resources.push(launchpad_resource(&(self.start_cards)()));
        }
        resources
    }

    /// Carry out the shell's verb at `address`: `Ok(None)` for an address the shell does not
    /// publish.
    ///
    /// # Errors
    ///
    /// The arguments do not fit the verb, or the verb names what the desktop does not hold.
    pub fn invoke(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<tilo_actions::ActionOutput>, ApiError> {
        let published = self.resources();
        let encoded = match tilo_actions::api::encode_json(&published, address, arguments) {
            Ok(encoded) => encoded,
            Err(ApiError::Unknown(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let verb = match Verb::from_payload(&encoded.payload) {
            Ok(Verb::Layout(verb)) => verb,
            Ok(Verb::Chrome(_)) => return Ok(None),
            Err(error) => return Err(ApiError::InvalidArguments(error)),
        };
        if self.perform(&verb) {
            Ok(Some(tilo_actions::ActionOutput::default()))
        } else {
            Err(ApiError::Failed(crate::desktop::i18n::t(
                "desktop-verb-names-nothing",
            )))
        }
    }

    /// Carry out `verb` on the desktop. Whether it named what the desktop holds.
    pub fn perform(&self, verb: &LayoutEffect) -> bool {
        match verb {
            LayoutEffect::Focus { tab } => {
                return tab.parse().is_ok_and(|tab| self.handle.focus_tab(tab));
            }
            LayoutEffect::CloseTab { .. } => return self.handle.dispatch(verb.clone()).is_ok(),
            // With nothing typed the effect is the search key itself: it opens search, and a press
            // over the open search closes it. A query is typed into the search already open.
            LayoutEffect::Search { query } => match query {
                None => self.handle.toggle_search(),
                Some(query) => self
                    .handle
                    .open_search(Scope::Workspace, Some(query.clone())),
            },
            LayoutEffect::ChooseSearchRow { row, argument } => self.handle.choose_search_row(
                usize::try_from(*row).unwrap_or(usize::MAX),
                argument.clone(),
            ),
            LayoutEffect::Launchpad { card } => {
                if card == "apps" {
                    self.handle.open_search(Scope::WindowGroup(None), None);
                } else if let Some(card) = LaunchpadAction::ALL
                    .into_iter()
                    .find(|action| action.slug() == card)
                {
                    ask(
                        &self.handle,
                        &self.asks,
                        DesktopAsk::HostAction(card.host_id()),
                    );
                } else {
                    return false;
                }
            }
            LayoutEffect::StepFocus { steps } => {
                self.handle
                    .step_focus(i32::try_from(*steps).unwrap_or_default());
            }
            LayoutEffect::FocusDisplay { display } => {
                let Ok(display) = display.parse::<u32>() else {
                    return false;
                };
                self.handle.focus_display(display);
            }
            LayoutEffect::MoveToDisplay { steps } => {
                self.handle
                    .move_focused_to_display(i32::try_from(*steps).unwrap_or_default());
            }
            LayoutEffect::WaitWindow { tab } => {
                let Ok(tab) = tab.parse() else {
                    return false;
                };
                ask(&self.handle, &self.asks, DesktopAsk::WaitWindow(tab));
            }
            LayoutEffect::ForceQuitApp { tab } => {
                let Ok(tab) = tab.parse() else {
                    return false;
                };
                ask(&self.handle, &self.asks, DesktopAsk::ForceQuitApp(tab));
            }
        }
        true
    }
}
