//! The areas of Settings, as the window holds them without naming them.

use std::any::Any;
use std::rc::Rc;

use crate::categories::Category;

pub trait PaneArea: Any {
    fn poll(&self) {}

    fn warm(&self) {}

    /// Whether the area still waits for something its pane shows in part
    /// meanwhile, so the pane is built again once it arrives.
    fn partial(&self) -> bool {
        false
    }

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

    fn next_wake(&self) -> Option<ui::time::Instant> {
        None
    }

    fn statuses(&self) -> Vec<ui::framework::EffectStatus> {
        Vec::new()
    }

    fn take_host_intent(&self) -> Option<ui::widget::AppHostIntent> {
        None
    }

    fn take_collab_requests(&self) -> Vec<ui::collaboration::CollabRequest> {
        Vec::new()
    }

    fn handle_collab_event(&self, _event: &ui::collaboration::CollabEvent) {}

    fn query(&self, _category: Category) -> Option<&dyn PaneQuery> {
        None
    }

    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub trait PaneQuery {
    fn set_query_text(&self, text: &str);

    fn query_closed(&self);
}

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

#[derive(Default)]
pub struct Areas(Vec<Box<dyn PaneArea>>);

impl Areas {
    pub fn of(context: &AreaContext) -> Self {
        let mut areas = Self::default();
        for make in &registry().areas {
            let area = make(context, &areas);
            areas.0.push(area);
        }
        areas
    }

    #[must_use]
    pub fn find<A: PaneArea>(&self) -> Option<&A> {
        self.0
            .iter()
            .find_map(|area| area.as_any().downcast_ref::<A>())
    }

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

    #[cfg(test)]
    pub(crate) fn take_replacements(&mut self, context: &AreaContext) {
        for make in REPLACEMENTS.with(std::cell::RefCell::take) {
            self.replace(make(context));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn PaneArea> {
        self.0.iter().map(|area| &**area)
    }
}

#[cfg(test)]
type Replacement = Box<dyn FnOnce(&AreaContext) -> Box<dyn PaneArea>>;

#[cfg(test)]
thread_local! {
    static REPLACEMENTS: std::cell::RefCell<Vec<Replacement>> =
        std::cell::RefCell::default();
}

#[cfg(test)]
pub fn replace_in_next_window<A: PaneArea>(make: impl FnOnce(&AreaContext) -> A + 'static) {
    REPLACEMENTS.with(|replacements| {
        replacements
            .borrow_mut()
            .push(Box::new(move |context| Box::new(make(context))));
    });
}

pub struct AreaContext {
    pub storage: ui::storage::AppStorage,
    pub scene: Option<Rc<crate::scenarios::CaptureScene>>,
    /// Whether the areas probe and drive this device's hardware and
    /// services, rather than the stand-ins a screenshot shows. The crate's
    /// own tests drive the stand-ins too, so they never touch the machine
    /// running them.
    pub devices: bool,
    /// The radios this device has; never read when it reaches no device.
    pub radios: crate::radios::Radios,
    pub pages: crate::pages::PanePages,
    pub secrets: crate::secrets::SecretAsks,
    pub provider_admin: ui::models::ProviderAdministration,
    pub linked: ui::linked_accounts::LinkedAccounts,
    pub sections: crate::sections::Sections,
    pub account_root: crate::accounts::AccountRoot,
}

impl AreaContext {
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
    pub areas: &'static [(u16, MakeArea)],
    pub families: &'static [(u16, &'static crate::effects::EffectFamily)],
    pub live_areas: &'static [(u16, crate::effects::LiveArea)],
    pub settings: &'static [crate::setting::SettingEntry],
}

#[derive(Default)]
pub struct Registry {
    pub areas: Vec<MakeArea>,
    pub families: Vec<&'static crate::effects::EffectFamily>,
    pub live_areas: Vec<&'static crate::effects::LiveArea>,
    pub settings: Vec<&'static crate::setting::SettingEntry>,
}

impl Registry {
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

#[must_use]
pub fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Registry::of(crate::panes::PARTS))
}
