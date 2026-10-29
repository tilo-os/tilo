//! Each setting as the row that changes it, wherever it is shown.
//!
//! A setting the index names ([`settings_store::index`]) is shown by one
//! function: the row its pane shows, with its label, its explanation and its
//! control bound to the section's field edit. The pane builds its row with
//! that function, and so does Settings' search: typing in the window bar
//! lists the settings that match as those same rows, operable where they
//! are found ([`Found`]), each page's group opening its page. A yes-or-no
//! that only the device can switch (a radio, the sound server) is the
//! switch its pane shows once the window has read it can be switched. A
//! setting whose pane draws it some other way (a layout, a schedule, a
//! picker) is listed by name, and showing it takes the person to it in its
//! page; the row says what it is set to, read where its pane reads it.
//!
//! A pane brings its settings' rows in its [`crate::area::Part`] with
//! [`setting_row!`], its device switches with [`setting_bound_switch!`] or
//! [`setting_view_switch!`], and what the others are set to with
//! [`setting_value!`].

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use settings_store::index::{self, Setting};
use ui::storage::SettingsCache;
use ui::widget::{AppModel, Button, ButtonRole, ControlSize, ModelPath, Widget};

use crate::categories::Category;
use crate::effects::{NavigationEffect, SettingsEffect};
use crate::i18n::t;
use crate::resource::Settings;
use provider_settings_ui::widgets::{Item, SettingsSection, rows, section_action};

/// One setting, as search shows it.
pub struct SettingEntry {
    /// The setting's id in the index.
    pub id: &'static str,
    pub shown: Shown,
}

/// How search shows a setting.
pub enum Shown {
    /// The row its page shows, as view code over its section
    /// ([`setting_row!`]); `None` where the section cannot be held.
    Bound(fn(&ui::framework::ViewCx<crate::resource::Settings>) -> Option<Item>),
    /// What it is set to, as its pane reads it in the window: its section,
    /// followed in `follows`, or what its area last read of the device
    /// (never reading it). `None` while that is unknown. Read at every
    /// sync while shown, so it reads only what is already at hand.
    Value(fn(&crate::settings_app::SettingsApp, &Follows) -> Option<String>),
    /// A yes-or-no its pane switches only where the device lets it (a
    /// radio, a display, the sound server), as view code over its section
    /// ([`setting_bound_switch!`]): the switch the second function builds
    /// while what the first reads, as its pane reads it in the window like
    /// [`Shown::Value`], says it can be switched. Listed by name with what
    /// it says while it cannot be switched, or is not known.
    BoundSwitch(
        fn(&crate::settings_app::SettingsApp, &Follows) -> Read,
        fn(&ui::framework::ViewCx<crate::resource::Settings>) -> Option<Item>,
    ),
    /// [`Shown::BoundSwitch`] whose switch is the device's: the second function
    /// builds it in the window's view, showing whether it is on, bound to
    /// the effect that switches the device ([`setting_view_switch!`]).
    ViewSwitch(
        fn(&crate::settings_app::SettingsApp, &Follows) -> Read,
        fn(&ui::framework::ViewCx<crate::resource::Settings>, bool) -> Item,
    ),
}

/// What search reads of a setting it shows by name or as a switch of the
/// device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// What it is set to, shown beside its name; `None` while unknown.
    Value(Option<String>),
    /// A switch, on or off.
    Switch(bool),
}

/// The [`SettingEntry`] of the setting `$id`, whose row `$row(cx)` builds
/// as view code over the section `$section`: the function its page builds
/// the row with.
#[macro_export]
macro_rules! setting_row {
    ($id:literal, $section:ty => $row:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::Bound(|cx| {
                let section = cx.settings::<$section>();
                provider_settings_ui::bound::record_item(cx, &section, $row)
            }),
        }
    };
}
pub use crate::setting_row;

/// The [`SettingEntry`] of the setting `$id`, which search shows by name
/// with what `$value(app, follows)` says it is set to; `known` when that is
/// always at hand.
#[macro_export]
macro_rules! setting_value {
    ($id:literal => known $value:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::Value(|app, follows| Some($value(app, follows))),
        }
    };
    ($id:literal => $value:expr) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::Value($value),
        }
    };
}
pub use crate::setting_value;

/// The [`SettingEntry`] of the setting `$id`, which search shows as the
/// switch `$row(cx)` builds as view code over the section `$section` while
/// `$read(app, follows)` says it can be switched ([`Shown::BoundSwitch`]).
#[macro_export]
macro_rules! setting_bound_switch {
    ($id:literal => $read:path, $section:ty => $row:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::BoundSwitch($read, |cx| {
                let section = cx.settings::<$section>();
                provider_settings_ui::bound::record_item(cx, &section, $row)
            }),
        }
    };
}
pub use crate::setting_bound_switch;

/// The [`SettingEntry`] of the setting `$id`, which search shows as the
/// switch `$row(cx, on)` builds as view code while `$read(app, follows)`
/// says it can be switched ([`Shown::ViewSwitch`]): a switch of the device,
/// its pane's own.
#[macro_export]
macro_rules! setting_view_switch {
    ($id:literal => $read:path, $row:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::ViewSwitch($read, $row),
        }
    };
}
pub use crate::setting_view_switch;

/// A chooser of every value the field at `field` offers, by its label:
/// `row` bound to it.
pub fn choose<R, V>(
    row: provider_settings_ui::bound::Setting<'_, R>,
    field: ui::tilo_resource::Path<R, V>,
) -> Item
where
    R: ui::tilo_resource::Resource<State = R> + ui::tilo_resource::Model,
    V: settings_store::Labelled
        + ui::tilo_resource::Model
        + ui::tilo_resource::Settable
        + Send
        + Sync
        + 'static,
{
    row.among(field, V::CHOICES.iter().copied(), |choice: &V| {
        settings_store::Labelled::label(*choice).to_owned()
    })
}

/// What each of `settings` is set to, where search shows it by name or as
/// a switch of the device.
#[must_use]
pub fn reads(
    app: &crate::settings_app::SettingsApp,
    follows: &Follows,
    settings: &[&'static Setting],
) -> Vec<Read> {
    settings
        .iter()
        .map(
            |setting| match entry(setting.id).map(|entry| &entry.shown) {
                Some(Shown::Value(value)) => Read::Value(value(app, follows)),
                Some(Shown::BoundSwitch(read, _) | Shown::ViewSwitch(read, _)) => {
                    read(app, follows)
                }
                Some(Shown::Bound(_)) | None => Read::Value(None),
            },
        )
        .collect()
}

/// "On" or "Off", as a yes-or-no setting reads.
#[must_use]
pub fn on_off(on: bool) -> String {
    t(if on {
        "settings-value-on"
    } else {
        "settings-value-off"
    })
}

/// The sections the settings search lists by name read, each read again
/// only when the settings document changed.
#[derive(Default)]
pub struct Follows {
    /// Each section read, as a `RefCell<SettingsCache<T>>`.
    held: RefCell<Vec<Rc<dyn Any>>>,
}

/// A section of the settings that can be followed.
pub trait Followable: ui::storage::SettingsSchema + Clone + PartialEq + 'static {}

impl<T: ui::storage::SettingsSchema + Clone + PartialEq + 'static> Followable for T {}

impl Follows {
    /// The section `T` as stored in `storage` now, read again only where
    /// the document changed since it was last asked for.
    pub fn get<T: Followable>(&self, storage: &ui::storage::AppStorage) -> T {
        let mut held = self.held.borrow_mut();
        let found = held
            .iter()
            .find_map(|any| Rc::clone(any).downcast::<RefCell<SettingsCache<T>>>().ok());
        let cache = found.unwrap_or_else(|| {
            let cache = Rc::new(RefCell::new(SettingsCache::new(storage.settings::<T>())));
            held.push(Rc::clone(&cache) as Rc<dyn Any>);
            cache
        });
        let mut cache = cache.borrow_mut();
        cache.refresh();
        cache.get().clone()
    }
}

/// Every setting's row the parts of Settings bring, by the setting's id.
#[must_use]
pub fn entry(id: &str) -> Option<&'static SettingEntry> {
    crate::area::registry()
        .settings
        .iter()
        .copied()
        .find(|entry| entry.id == id)
}

/// The settings that match `query`, best first: found by their names and
/// the words people use for them, in every language, as the desktop palette
/// finds them.
#[must_use]
pub fn matching(query: &str) -> Vec<&'static Setting> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = index::SETTINGS
        .iter()
        .map(|setting| t(&setting.name_key()).to_lowercase())
        .collect();
    let words: Vec<Vec<String>> = index::SETTINGS
        .iter()
        .map(|setting| setting.search_words())
        .collect();
    let ranks = ui::search::ListIndex::default().ranks(
        query,
        names.iter().zip(&words).map(|(name, words)| {
            std::iter::once((name.as_str(), ui::search::FieldWeight::TITLE)).chain(
                words
                    .iter()
                    .map(|word| (word.as_str(), ui::search::FieldWeight::KEYWORD)),
            )
        }),
    );
    let mut found: Vec<(i32, usize)> = ranks
        .into_iter()
        .enumerate()
        .filter_map(|(at, rank)| rank.map(|rank| (rank, at)))
        .collect();
    found.sort_by(|(a, a_at), (b, b_at)| b.cmp(a).then(a_at.cmp(b_at)));
    found
        .into_iter()
        .filter_map(|(_, at)| index::SETTINGS.get(at))
        .filter(|setting| Category::from_slug(setting.page).is_some())
        .collect()
}

/// The rows of `settings` whose pages are view code, built in `cx`, by
/// their place, `reads` saying which switches can be switched: what
/// [`Found`] shows for them.
#[must_use]
pub fn bound_rows(
    cx: &ui::framework::ViewCx<crate::resource::Settings>,
    settings: &[&'static Setting],
    reads: &[Read],
) -> Vec<Option<Item>> {
    settings
        .iter()
        .enumerate()
        .map(
            |(at, setting)| match (entry(setting.id).map(|entry| &entry.shown), reads.get(at)) {
                (Some(Shown::Bound(row)), _) => row(cx),
                (Some(Shown::BoundSwitch(_, row)), Some(Read::Switch(_))) => row(cx),
                (Some(Shown::ViewSwitch(_, row)), Some(&Read::Switch(on))) => Some(row(cx, on)),
                _ => None,
            },
        )
        .collect()
}

/// What Settings' search found: the matching settings, grouped by the page
/// each is on, as the rows that change them.
pub struct Found {
    /// What its navigation buttons are bound through; `None` outside a
    /// view, where they are unavailable.
    actions: Option<ui::framework::Actions<Settings>>,
    /// The rows of the settings found whose pages are view code, built by
    /// the view that shows this list, by the setting's place in `settings`.
    bound: RefCell<Vec<Option<Item>>>,
    /// The settings found, best first, and what each shown by name or as a
    /// switch of the device is set to ([`reads`]).
    settings: Vec<&'static Setting>,
    reads: Vec<Read>,
    composed: ui::widget::Composed,
}

impl Found {
    /// The settings found, `bound` holding the rows built as view code
    /// for those whose pages are ([`bound_rows`]).
    #[must_use]
    pub fn new(
        actions: Option<ui::framework::Actions<Settings>>,
        settings: Vec<&'static Setting>,
        reads: Vec<Read>,
        bound: Vec<Option<Item>>,
    ) -> Self {
        Self {
            actions,
            bound: RefCell::new(bound),
            settings,
            reads,
            composed: ui::widget::Composed::default(),
        }
    }

    /// A button `id` reading `label`, running `effect`: Settings' own
    /// navigation, as an agent's `open` runs it.
    fn navigating(&self, id: &str, label: &str, effect: NavigationEffect) -> Button {
        use ui::framework::ActionControl as _;
        let button = Button::new(id, label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small);
        match &self.actions {
            Some(actions) => actions.bind_effect_invocation(button, &SettingsEffect::from(effect)),
            None => button.unavailable(),
        }
    }

    /// One page's group: its name, a button opening it, and each setting
    /// found there, `found` being where they are in its settings.
    fn group(&self, page: &'static str, found: &[usize]) -> Option<SettingsSection> {
        let category = Category::from_slug(page)?;
        let items: Vec<Item> = found
            .iter()
            .filter_map(|&at| Some((at, *self.settings.get(at)?, self.reads.get(at))))
            .map(|(at, setting, read)| {
                if let Some(row) = self.bound.borrow_mut().get_mut(at).and_then(Option::take) {
                    return row;
                }
                let value = match read {
                    Some(Read::Value(value)) => value.as_deref(),
                    _ => None,
                };
                let name = t(&setting.name_key());
                let show = self
                    .navigating(
                        &format!("settings.found.show:{}", setting.id),
                        &t("settings-found-show"),
                        NavigationEffect::ShowSetting {
                            setting: setting.id.to_owned(),
                        },
                    )
                    .with_accessible_name(&format!("{}: {name}", t("settings-found-show")));
                let row = ui::widget::ListRow::new(format!("settings.found:{}", setting.id), &name)
                    .trailing(Box::new(show));
                provider_settings_ui::widgets::row(Box::new(match value {
                    Some(value) => row.value(value),
                    None => row,
                }))
            })
            .collect();
        let open = self
            .navigating(
                &format!("settings.found.open:{page}"),
                &t("settings-found-open"),
                NavigationEffect::OpenPane {
                    pane: page.to_owned(),
                },
            )
            .with_accessible_name(&format!(
                "{}: {}",
                t("settings-found-open"),
                category.label()
            ));
        Some(section_action(&category.label(), open, rows(items)))
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // Each page with where its settings are in `settings`.
        let mut pages: Vec<(&'static str, Vec<usize>)> = Vec::new();
        for (at, setting) in self.settings.iter().enumerate() {
            match pages.iter_mut().find(|(page, _)| *page == setting.page) {
                Some((_, found)) => found.push(at),
                None => pages.push((setting.page, vec![at])),
            }
        }
        provider_settings_ui::widgets::page(
            pages
                .iter()
                .filter_map(|(page, settings)| self.group(page, settings))
                .collect(),
        )
    }
}

impl Widget for Found {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let mut composed = std::mem::take(&mut self.composed);
        let child = composed.build_layout_kept(|| self.build_composition(), taffy, theme, node_map);
        self.composed = composed;
        let style = ui::taffy::Style {
            display: ui::taffy::Display::Flex,
            flex_direction: ui::taffy::FlexDirection::Column,
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::percent(1.0),
                height: ui::taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        ui::widget::TaffyTreeExt::widget_node(taffy, style, &[child], node_map, self)
    }

    ui::forward_to_composed!(composed);

    fn name(&self) -> &'static str {
        "SettingsFound"
    }
}

#[cfg(test)]
mod tests;
