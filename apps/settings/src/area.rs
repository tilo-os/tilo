//! The areas of Settings, as the window holds them without naming them.
//!
//! An area is what one pane reads of the device and keeps between visits:
//! the printers and their queue, the accounts, the audio graph. The window
//! ([`crate::settings_app::SettingsApp`]) holds every area in [`Areas`] and reaches each through
//! [`PaneArea`] alone: it polls them, starts their reads and offers their
//! live rows; each pane's view reads its area. Which areas there are is the
//! panes' business (their [`Part`]s, brought together in [`registry`]), so
//! the window's code names none of them.

use std::any::Any;
use std::rc::Rc;

use crate::categories::Category;

/// One area of Settings: what its pane reads and keeps.
pub trait PaneArea: Any {
    /// Take in what the area's background work produced since the last
    /// frame.
    fn poll(&self) {}

    /// Read what the area's pane opens on, after every area read once.
    fn warm(&self) {}

    /// Whether the area still waits for something its pane shows in part
    /// meanwhile, so the pane is built again once it arrives.
    fn partial(&self) -> bool {
        false
    }

    /// Whether a pane shows the area's first reading of the device as on
    /// its way: a capture waits for it.
    fn first_read(&self) -> bool {
        false
    }

    /// Whether an action of the area is running that the window must keep
    /// drawing for.
    fn busy(&self) -> bool {
        false
    }

    /// Whether a read already under way will replace what the area's page
    /// shows meanwhile (a "Scanning…" note): a still capture waits for it,
    /// and a live window asks for no frames on its account.
    fn pending_visual_work(&self) -> bool {
        false
    }

    /// When the area next wants to look at the device again while nothing
    /// else happens (a page that re-reads it on a timer).
    fn next_wake(&self) -> Option<ui::time::Instant> {
        None
    }

    /// The effects of the area that differ from their declaration on this
    /// device now.
    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        Vec::new()
    }

    /// What the area asks the host to do with the window, once.
    fn take_host_intent(&self) -> Option<ui::widget::AppHostIntent> {
        None
    }

    /// Collaboration requests of the area's host actions.
    fn take_collab_requests(&self) -> Vec<ui::collaboration::CollabRequest> {
        Vec::new()
    }

    /// A collaboration event for the area's host actions.
    fn handle_collab_event(&self, _event: &ui::collaboration::CollabEvent) {}

    /// What the window bar's search queries while `category`'s pane is
    /// shown, when the pane lists the answer to a query (models on Hugging
    /// Face) rather than a page the search narrows. `None`: the search
    /// narrows the sidebar.
    fn query(&self, _category: Category) -> Option<&dyn PaneQuery> {
        None
    }

    /// The area, to find it by its type.
    fn as_any(&self) -> &dyn Any;

    /// The area, to replace it by its type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A pane's query over what lies beyond the window (models on Hugging
/// Face), driven by the window bar's search: what is typed there is the
/// query, and the pane draws no field of its own ([`ui::widget::Queryable`]).
pub trait PaneQuery {
    /// What is typed is now `text` (empty when nothing is yet).
    fn set_query_text(&self, text: &str);

    /// The search closed: the pane shows what it showed before it opened.
    fn query_closed(&self);
}

/// Implement [`PaneArea`]'s conversions for an area type, with any hooks it
/// overrides: `pane_area!(Area)`, or `pane_area! { Area; fn poll(&self) {..} }`.
#[macro_export]
macro_rules! pane_area {
    ($ty:ty $(; $($hook:item)*)?) => {
        impl $crate::area::PaneArea for $ty {
            $($($hook)*)?

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }
    };
}
pub use crate::pane_area;

/// Every area of a window, each found by its type.
#[derive(Default)]
pub struct Areas(Vec<Box<dyn PaneArea>>);

impl Areas {
    /// The areas the registry ([`registry`]) makes in `context`, in order: an
    /// area made later may use one made before it.
    pub fn of(context: &AreaContext) -> Self {
        let mut areas = Self::default();
        for make in &registry().areas {
            let area = make(context, &areas);
            areas.0.push(area);
        }
        areas
    }

    /// The area of type `A`: every window holds one of each type
    /// the registry ([`registry`]) makes, from when it is made.
    #[must_use]
    pub fn find<A: PaneArea>(&self) -> Option<&A> {
        self.0
            .iter()
            .find_map(|area| area.as_any().downcast_ref::<A>())
    }

    /// Put `area` in place of the area of its type.
    #[cfg(test)]
    fn replace(&mut self, area: Box<dyn PaneArea>) {
        let wanted = area.as_any().type_id();
        if let Some(slot) = self
            .0
            .iter_mut()
            .find(|held| held.as_any().type_id() == wanted)
        {
            *slot = area;
        } else {
            self.0.push(area);
        }
    }

    /// Put the areas a test asked for ([`replace_in_next_window`]) in place
    /// of the areas of their types.
    #[cfg(test)]
    pub(crate) fn take_replacements(&mut self, context: &AreaContext) {
        for make in REPLACEMENTS.with(std::cell::RefCell::take) {
            self.replace(make(context));
        }
    }

    /// Every area, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = &dyn PaneArea> {
        self.0.iter().map(|area| &**area)
    }
}

/// What makes an area in place of one a window would make, for a test.
#[cfg(test)]
type Replacement = Box<dyn FnOnce(&AreaContext) -> Box<dyn PaneArea>>;

#[cfg(test)]
thread_local! {
    static REPLACEMENTS: std::cell::RefCell<Vec<Replacement>> =
        std::cell::RefCell::default();
}

/// Make the next window this thread attaches hold the area `make` makes in
/// its context, in place of the area of its type: a stand-in a test drives
/// (a daemon that stalls).
#[cfg(test)]
pub fn replace_in_next_window<A: PaneArea>(make: impl FnOnce(&AreaContext) -> A + 'static) {
    REPLACEMENTS.with(|replacements| {
        replacements
            .borrow_mut()
            .push(Box::new(move |context| Box::new(make(context))));
    });
}

/// What an area is made in: the window's settings document and what it
/// reaches of the device and the person. An area keeps the parts it uses.
pub struct AreaContext {
    /// The app's storage: its settings document, files and secrets.
    pub storage: ui::storage::AppStorage,
    /// The stand-ins a screenshot shows in place of this device's hardware
    /// and accounts; `None` in a live window.
    pub scene: Option<Rc<crate::scenarios::CaptureScene>>,
    /// Whether the areas probe and drive this device's hardware and
    /// services, rather than the stand-ins a screenshot shows. The crate's
    /// own tests drive the stand-ins too, so they never touch the machine
    /// running them.
    pub devices: bool,
    /// The radios this device has; never read when it reaches no device.
    pub radios: crate::radios::Radios,
    /// The pages the panes drilled into, shared with the window.
    pub pages: crate::pages::PanePages,
    /// The password prompt the window shows for the panes.
    pub secrets: crate::secrets::SecretAsks,
    /// The administration of the agent providers the launcher granted.
    pub provider_admin: ui::models::ProviderAdministration,
    /// The person's accounts with the services the workspace is connected
    /// to, when the host lent them.
    pub linked: ui::linked_accounts::LinkedAccounts,
    /// The settings sections' actions, for an area invoking them.
    pub sections: crate::sections::Sections,
    /// Where the person's accounts are kept.
    pub account_root: crate::accounts::AccountRoot,
}

impl AreaContext {
    /// The stand-ins a screenshot shows; `None` in a live window.
    #[must_use]
    pub fn scene(&self) -> Option<&crate::scenarios::CaptureScene> {
        self.scene.as_deref()
    }

    /// What an area is made in over the settings and accounts of the
    /// profile at `root`, outside any view, for a test: it reaches no
    /// device and its sections are not kept.
    #[cfg(test)]
    #[must_use]
    pub fn in_profile(root: std::path::PathBuf) -> Self {
        let id = <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID;
        Self {
            storage: ui::storage::AppStorage::new(
                id,
                ui::storage::secrets(id),
                ui::storage::SettingsDocument::Profile(root.clone()),
            ),
            scene: None,
            devices: false,
            radios: crate::radios::Radios::unread(),
            pages: crate::pages::PanePages::new(),
            secrets: crate::secrets::SecretAsks::new(),
            provider_admin: ui::models::ProviderAdministration::default(),
            linked: ui::linked_accounts::LinkedAccounts::default(),
            sections: crate::sections::Sections::default(),
            account_root: crate::accounts::AccountRoot::at(root),
        }
    }
}

/// What makes one area in a window's context, given the areas made before.
pub type MakeArea = fn(&AreaContext, &Areas) -> Box<dyn PaneArea>;

/// Why an effect could not reach its area: the window holds every area from
/// when it is made, so this says only that the window was not.
#[must_use]
pub fn missing() -> String {
    "this part of Settings is not open".to_owned()
}

/// One crate's part of Settings: the areas, panes, effect families and live
/// areas it brings, each with the place it takes in the whole (areas are
/// made, effects declared and rows published in that order, whichever crate
/// brings them). The settings sections are the session's, kept whether or
/// not Settings is open ([`settings_sections`]).
pub struct Part {
    /// What makes each area, by place.
    pub areas: &'static [(u16, MakeArea)],
    pub families: &'static [(u16, &'static crate::effects::EffectFamily)],
    pub live_areas: &'static [(u16, crate::effects::LiveArea)],
    /// The rows of the settings its panes show, as search shows them too.
    pub settings: &'static [crate::setting::SettingEntry],
}

/// Everything the parts of Settings bring, in place order.
#[derive(Default)]
pub struct Registry {
    /// What makes each area.
    pub areas: Vec<MakeArea>,
    pub families: Vec<&'static crate::effects::EffectFamily>,
    pub live_areas: Vec<&'static crate::effects::LiveArea>,
    /// Every setting's row.
    pub settings: Vec<&'static crate::setting::SettingEntry>,
}

impl Registry {
    /// What `parts` bring together, each kind in place order.
    #[must_use]
    pub fn of(parts: &[&'static Part]) -> Self {
        fn ordered<T: ?Sized + 'static>(mut items: Vec<(u16, &'static T)>) -> Vec<&'static T> {
            items.sort_by_key(|(place, _)| *place);
            items.into_iter().map(|(_, item)| item).collect()
        }
        let mut areas: Vec<(u16, MakeArea)> = parts
            .iter()
            .flat_map(|part| part.areas.iter().copied())
            .collect();
        areas.sort_by_key(|(place, _)| *place);
        Self {
            areas: areas.into_iter().map(|(_, make)| make).collect(),
            families: ordered(
                parts
                    .iter()
                    .flat_map(|part| {
                        part.families
                            .iter()
                            .map(|(place, family)| (*place, *family))
                    })
                    .collect(),
            ),
            live_areas: ordered(
                parts
                    .iter()
                    .flat_map(|part| part.live_areas.iter().map(|(place, area)| (*place, area)))
                    .collect(),
            ),
            settings: parts.iter().flat_map(|part| part.settings.iter()).collect(),
        }
    }
}

/// What Settings is made of: every part ([`crate::panes::PARTS`]).
#[must_use]
pub fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Registry::of(crate::panes::PARTS))
}
