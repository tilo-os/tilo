//! The areas of Settings, as the window holds them without naming them.
//!
//! An area is what one pane reads of the device and keeps between visits:
//! the printers and their queue, the accounts, the audio graph. The window
//! ([`SettingsApp`]) holds every area in [`Areas`] and reaches each through
//! [`PaneArea`] alone: it polls them, starts their reads, offers their live
//! rows and builds the pane that shows one. Which areas and panes there are
//! is the panes' business (their [`Part`]s, brought together by [`install`]),
//! so the window's code names none of them, and a pane that lives in a
//! library of its own is just one more entry.

use std::any::{Any, TypeId};

use ui::widget::Widget;

use crate::categories::Category;
use crate::host::PaneCx;
use crate::settings_app::SettingsApp;

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

    /// Whether an action of the area is running that the window must keep
    /// drawing for.
    fn busy(&self) -> bool {
        false
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
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        Vec::new()
    }

    /// A collaboration event for the area's host actions.
    fn handle_collab_event(&mut self, _event: &ui::collaboration::CollabEvent) {}

    /// The area, to find it by its type.
    fn as_any(&self) -> &dyn Any;

    /// The area, to replace it by its type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
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
    pub fn of(context: &AreaContext<'_>) -> Self {
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
    pub fn replace<A: PaneArea>(&mut self, area: A) {
        let wanted = TypeId::of::<A>();
        if let Some(slot) = self
            .0
            .iter_mut()
            .find(|held| held.as_any().type_id() == wanted)
        {
            *slot = Box::new(area);
        } else {
            self.0.push(Box::new(area));
        }
    }

    /// Every area, in the order they were made.
    pub fn iter(&self) -> impl Iterator<Item = &dyn PaneArea> {
        self.0.iter().map(|area| &**area)
    }

    /// Every area, to hand collaboration traffic to.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn PaneArea>> {
        self.0.iter_mut()
    }
}

/// What an area is made in: the window's host and the person's accounts.
pub struct AreaContext<'a> {
    /// What the panes reach of the host.
    pub cx: &'a PaneCx,
    /// Where the person's accounts are kept.
    pub account_root: &'a crate::accounts::AccountRoot,
}

/// What makes one area in a window's context, given the areas made before.
pub type MakeArea = fn(&AreaContext<'_>, &Areas) -> Box<dyn PaneArea>;

/// A section of the person's settings a pane owns: published as a record of
/// their own and held for the pane's controls.
pub struct SectionEntry {
    /// Hold the section's published actions for the panes.
    pub held: HoldSection,
    /// Publish the section as a record.
    pub record: fn(&mut ui::framework::Records<'_>),
}

/// [`SectionEntry::held`].
pub type HoldSection = fn(
    &ui::framework::SurfaceCx<crate::resource::Settings>,
    &mut std::collections::HashMap<TypeId, Box<dyn Any>>,
);

/// The [`SectionEntry`] of section `$ty`.
#[macro_export]
macro_rules! section_entry {
    ($ty:ty) => {
        $crate::area::SectionEntry {
            held: $crate::host::held::<$ty>,
            record: |records| {
                records.settings::<$ty>();
            },
        }
    };
}
pub use crate::section_entry;

/// One pane: the categories it shows and how it is built for the window.
pub struct PaneEntry {
    /// The categories the pane shows.
    pub categories: &'static [Category],
    /// Build the pane's body for `category`.
    pub build: fn(&SettingsApp, Category) -> Box<dyn Widget>,
}

/// Why an effect could not reach its area: the window holds every area from
/// when it is made, so this says only that the window was not.
#[must_use]
pub fn missing() -> String {
    "this part of Settings is not open".to_owned()
}

/// One crate's part of Settings: the areas, panes, effect families, live
/// areas and settings sections it brings, each with the place it takes in
/// the whole (areas are made, effects declared, rows published and sections
/// recorded in that order, whichever crate brings them).
pub struct Part {
    /// What makes each area, by place.
    pub areas: &'static [(u16, MakeArea)],
    /// Every pane.
    pub panes: &'static [PaneEntry],
    /// Every effect family, by place.
    pub families: &'static [(u16, &'static crate::effects::EffectFamily)],
    /// Every area with live rows, by place.
    pub live_areas: &'static [(u16, crate::effects::LiveArea)],
    /// Every settings section, by place.
    pub sections: &'static [(u16, SectionEntry)],
}

/// Everything the parts of Settings bring, in place order.
#[derive(Default)]
pub struct Registry {
    /// What makes each area.
    pub areas: Vec<MakeArea>,
    /// Every pane.
    pub panes: Vec<&'static PaneEntry>,
    /// Every effect family.
    pub families: Vec<&'static crate::effects::EffectFamily>,
    /// Every area with live rows.
    pub live_areas: Vec<&'static crate::effects::LiveArea>,
    /// Every settings section.
    pub sections: Vec<&'static SectionEntry>,
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
            panes: parts.iter().flat_map(|part| part.panes.iter()).collect(),
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
            sections: ordered(
                parts
                    .iter()
                    .flat_map(|part| {
                        part.sections
                            .iter()
                            .map(|(place, section)| (*place, section))
                    })
                    .collect(),
            ),
        }
    }
}

static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();

/// Bring `parts` together as what Settings is made of, once per process: a
/// second call keeps the first.
pub fn install(parts: &[&'static Part]) {
    let _ = REGISTRY.get_or_init(|| Registry::of(parts));
}

/// What Settings is made of: the parts [`install`]ed. Before any were, only
/// Settings' own part (in this crate's tests, every part); asking before
/// installing never keeps an [`install`] from taking effect.
#[must_use]
pub fn registry() -> &'static Registry {
    static UNINSTALLED: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    #[cfg(test)]
    let parts = crate::panes::PARTS;
    #[cfg(not(test))]
    let parts: &[&'static Part] = &[&crate::effects::PART];
    REGISTRY
        .get()
        .unwrap_or_else(|| UNINSTALLED.get_or_init(|| Registry::of(parts)))
}
