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
//! [`setting_entry!`], its device switches with [`setting_switch!`], and
//! what the others are set to with [`setting_value!`].

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use settings_store::index::{self, Setting};
use ui::widget::{AppModel, Button, ButtonRole, ControlSize, ModelPath, Widget};

use crate::categories::Category;
use crate::effects::{NavigationEffect, SettingsEffect};
use crate::host::{Followed, PaneCx};
use crate::i18n::t;
use crate::widgets::{Item, SettingsSection, rows, section_action};

/// One setting, as search shows it.
pub struct SettingEntry {
    /// The setting's id in the index.
    pub id: &'static str,
    pub shown: Shown,
}

/// How search shows a setting.
pub enum Shown {
    /// The row its pane shows, over the sections `follows` keeps current.
    Row(fn(&PaneCx, &Follows) -> Item),
    /// What it is set to, as its pane reads it in the window: its section,
    /// followed in `follows`, or what its area last read of the device
    /// (never reading it). `None` while that is unknown. Read at every
    /// sync while shown, so it reads only what is already at hand.
    Value(fn(&crate::settings_app::SettingsApp, &Follows) -> Option<String>),
    /// A yes-or-no its pane switches only where the device lets it (a
    /// radio, a display, the sound server): the switch the second function
    /// builds over what the first reads, as its pane reads it in the
    /// window, like [`Shown::Value`]. Listed by name with what it says
    /// while it cannot be switched, or is not known.
    Switch(
        fn(&crate::settings_app::SettingsApp, &Follows) -> Read,
        fn(&PaneCx, bool) -> Item,
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

/// The [`SettingEntry`] of the setting `$id`, whose row `$row(cx, followed)`
/// builds over the followed section `$section`: the function the pane
/// builds the row with.
#[macro_export]
macro_rules! setting_entry {
    ($id:literal, $section:ty => $row:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::Row(|cx, follows| {
                let followed = follows.of::<$section>(cx);
                let followed = followed.borrow();
                $row(cx, &followed)
            }),
        }
    };
}
pub use crate::setting_entry;

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
/// switch `$row(cx, on)` builds while `$read(app, follows)` says
/// it can be switched ([`Shown::Switch`]).
#[macro_export]
macro_rules! setting_switch {
    ($id:literal => $read:path, $row:path) => {
        $crate::setting::SettingEntry {
            id: $id,
            shown: $crate::setting::Shown::Switch($read, $row),
        }
    };
}
pub use crate::setting_switch;

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
                Some(Shown::Switch(read, _)) => read(app, follows),
                Some(Shown::Row(_)) | None => Read::Value(None),
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

/// The sections the rows shown outside their panes read, each followed
/// once and kept current as it changes ([`Self::refresh`]).
#[derive(Default)]
pub struct Follows {
    /// Each section followed, as itself and as what refreshes it.
    held: RefCell<Vec<Held>>,
}

/// A followed section, as itself and as what refreshes it.
type Held = (Rc<dyn Any>, Rc<dyn Refresh>);

/// A section of the settings that can be followed.
pub trait Followable: ui::storage::SettingsSchema + Clone + PartialEq + 'static {}

impl<T: ui::storage::SettingsSchema + Clone + PartialEq + 'static> Followable for T {}

/// A followed section, whatever its type.
trait Refresh {
    fn refresh(&self) -> bool;
}

impl<T: Followable> Refresh for RefCell<Followed<T>> {
    fn refresh(&self) -> bool {
        self.borrow_mut().refresh()
    }
}

impl Follows {
    /// The section `T`, followed through `cx` from when it was first asked
    /// for.
    pub fn of<T: Followable>(&self, cx: &PaneCx) -> Rc<RefCell<Followed<T>>> {
        let mut held = self.held.borrow_mut();
        if let Some(followed) = held
            .iter()
            .find_map(|(any, _)| Rc::clone(any).downcast::<RefCell<Followed<T>>>().ok())
        {
            return followed;
        }
        let followed = Rc::new(RefCell::new(cx.follow::<T>()));
        held.push((
            Rc::clone(&followed) as Rc<dyn Any>,
            Rc::clone(&followed) as Rc<dyn Refresh>,
        ));
        followed
    }

    /// The section `T` as stored now, followed from when it was first
    /// asked for.
    pub fn get<T: Followable>(&self, cx: &PaneCx) -> T {
        self.of::<T>(cx).borrow().get().clone()
    }

    /// Read every followed section again where the settings changed, moving
    /// the controls that show them; whether any value changed.
    pub fn refresh(&self) -> bool {
        let held: Vec<Rc<dyn Refresh>> = self
            .held
            .borrow()
            .iter()
            .map(|(_, followed)| Rc::clone(followed))
            .collect();
        held.iter()
            .fold(false, |moved, followed| followed.refresh() | moved)
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

/// What Settings' search found: the matching settings, grouped by the page
/// each is on, as the rows that change them.
pub struct Found {
    cx: PaneCx,
    follows: Follows,
    /// The settings found, best first, and what each shown by name or as a
    /// switch of the device is set to ([`reads`]).
    settings: Vec<&'static Setting>,
    reads: Vec<Read>,
    composed: ui::widget::Composed,
}

impl Found {
    #[must_use]
    pub fn new(cx: PaneCx, settings: Vec<&'static Setting>, reads: Vec<Read>) -> Self {
        Self {
            cx,
            follows: Follows::default(),
            settings,
            reads,
            composed: ui::widget::Composed::default(),
        }
    }

    /// A button `id` reading `label`, running `effect`: Settings' own
    /// navigation, as an agent's `open` runs it.
    fn navigating(&self, id: &str, label: &str, effect: NavigationEffect) -> Button {
        self.cx.bind_invocation(
            Button::new(id, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small),
            SettingsEffect::from(effect),
        )
    }

    /// One page's group: its name, a button opening it, and each setting
    /// found there, `found` being where they are in its settings.
    fn group(&self, page: &'static str, found: &[usize]) -> Option<SettingsSection> {
        let category = Category::from_slug(page)?;
        let items: Vec<Item> = found
            .iter()
            .filter_map(|&at| Some((*self.settings.get(at)?, self.reads.get(at))))
            .map(|(setting, read)| {
                let value = match (entry(setting.id).map(|entry| &entry.shown), read) {
                    (Some(Shown::Row(row)), _) => return row(&self.cx, &self.follows),
                    (Some(Shown::Switch(_, row)), Some(&Read::Switch(on))) => {
                        return row(&self.cx, on);
                    }
                    (_, Some(Read::Value(value))) => value.as_deref(),
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
                crate::widgets::row(Box::new(match value {
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
        crate::widgets::page(
            pages
                .iter()
                .filter_map(|(page, settings)| self.group(page, settings))
                .collect(),
        )
    }
}

impl Widget for Found {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The rows' controls follow the stored values in place.
        self.follows.refresh();
        self.composed.sync(state, root);
    }

    crate::pane_model::composed_pane!(kept);

    fn name(&self) -> &'static str {
        "SettingsFound"
    }
}

#[cfg(test)]
mod tests;
