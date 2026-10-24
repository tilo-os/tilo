//! Personal Settings' pages and the settings on them, found from the
//! palette without opening Settings first.
//!
//! Each page is one row, found by its name, by what it holds and by the
//! words people use for it in either language, so a German desktop that
//! types "wifi" and an English one that types "wlan" both land on Network.
//! Each setting people look for by name ("keyboard layout", "dark mode",
//! "tap to click") is a row of its own that opens its page at the setting.
//! Both come from the one index Settings' own sidebar reads
//! ([`settings_store::index`]), and Enter opens them through Settings'
//! `settings://` links, the way every other app reaches it.
//!
//! A yes-or-no setting the session keeps ([`index::Switch`]) is the switch
//! itself: its row says whether it is on, and Enter flips it through its
//! section's field edit, the edit its switch in Settings and in Settings'
//! own search makes. A setting the palette changes to a typed value (a
//! timer, the display scale, the keyboard layout) says its value and takes
//! the new one (`values`). This is the one settings search: every row
//! comes from the index.

mod values;

use settings_store::index;
use ui::search::FieldWeight;

use crate::shell::i18n::t;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "settings-pages";

/// The picture of each page: the one Settings' sidebar shows beside it,
/// so a row from search and the page it opens look alike.
fn page_icon(slug: &str) -> Option<ui::icons::Icon> {
    use ui::icons::Icon;
    Some(match slug {
        "network" => Icon::Wifi,
        "bluetooth" => Icon::Bluetooth,
        "devices_sync" => Icon::Cloud,
        "sound" => Icon::Volume2,
        "speakers" => Icon::Speaker,
        "displays" => Icon::Monitor,
        "keyboard" | "keyboard_shortcuts" => Icon::Keyboard,
        "pointing" => Icon::Mouse,
        "spelling" => Icon::Type,
        "time_date" => Icon::Calendar,
        "users" => Icon::Users,
        "printers" => Icon::Printer,
        "power" => Icon::Battery,
        "appearance" => Icon::Palette,
        "accessibility" => Icon::Eye,
        "notifications" => Icon::Bell,
        "default-apps" => Icon::LayoutGrid,
        "storage" => Icon::HardDrive,
        "external_accounts" => Icon::Mail,
        "agent_models" => Icon::Sparkles,
        "region_formats" => Icon::Globe,
        "permissions" => Icon::Shield,
        "updates" => Icon::Refresh,
        "about" => Icon::Info,
        _ => return None,
    })
}

pub struct SettingsPagesProvider {
    kinds: Vec<ResourceKind>,
    /// The rows' text, indexed while it stays the same.
    ranker: ui::search::ListIndex,
    /// The rows and their searchable text, built once per language: a
    /// keystroke must only search them, not rebuild every page's copy.
    built: std::cell::RefCell<Option<Built>>,
    /// The typed settings as they stand, read again when they move.
    readings: values::Readings,
    /// How many times the rows were built.
    #[cfg(test)]
    builds: std::cell::Cell<usize>,
}

struct Built {
    locale: String,
    /// One row per page, then one per setting.
    rows: Vec<SearchResult>,
    /// Each row's searchable text, in row order.
    fields: Vec<Vec<(String, FieldWeight)>>,
    /// How many of the rows are pages; the rest are settings.
    pages: usize,
    /// For each setting row, in order, the row of the page it is on.
    setting_pages: Vec<usize>,
    /// The switch each row flips, in row order: `None` for a row that
    /// opens Settings.
    switches: Vec<Option<index::Switch>>,
    /// The typed setting each row changes or reads, in row order.
    typed: Vec<Option<Typed>>,
}

impl Built {
    /// Adds a setting's row, found by `field`, on the page at `page`.
    fn push_setting(
        &mut self,
        field: Vec<(String, FieldWeight)>,
        page: usize,
        switch: Option<index::Switch>,
        typed: Option<Typed>,
        row: SearchResult,
    ) {
        self.fields.push(field);
        self.setting_pages.push(page);
        self.switches.push(switch);
        self.typed.push(typed);
        self.rows.push(row);
    }
}

/// A row of a setting the palette changes to a typed value.
#[derive(Clone)]
struct Typed {
    kind: values::Kind,
    /// Whether the row reads the value rather than changing it.
    reads: bool,
    /// The setting's name, which the reading row says beside its value.
    name: String,
}

/// Settings the palette already offers as quick actions under the same
/// name ("Wi-Fi", "Volume", "Do not disturb"): typing the name does the
/// thing there, so a second row opening Settings would only outrank it.
const QUICK_ACTION_SETTINGS: &[&str] = &[
    "network.wifi",
    "network.airplane",
    "bluetooth.enabled",
    "sound.volume",
    "sound.mute",
    "display.brightness",
    "notifications.dnd",
];

impl SettingsPagesProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-settings-pages"))],
            ranker: ui::search::ListIndex::default(),
            built: std::cell::RefCell::default(),
            readings: values::Readings::default(),
            #[cfg(test)]
            builds: std::cell::Cell::new(0),
        }
    }

    /// How many times the rows were built.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.get()
    }
}

/// A row flipping the yes-or-no setting `switch`; listed, it reads on or
/// off ([`live`]).
fn switching(
    kind: &ResourceKind,
    id: String,
    title: String,
    facts: Vec<String>,
    switch: index::Switch,
) -> SearchResult {
    let invoked_by = settings_store::InvokingAction::action(format!("{PROVIDER_ID}:{id}"));
    let flip = move |chrome: &crate::shell::chrome_state::Chrome,
                     args: &crate::shell::search::action_api::ActionArgs| {
        let confirmed = args.boolean(CONFIRM) == Some(true);
        let on = crate::shell::settings_records::flip(switch, confirmed, &invoked_by)?;
        // The desktop's own look follows at once rather than at the next
        // reading of the settings file.
        if switch.section
            == <settings_sections::prefs::AppearancePrefs as tilo_resource::Resource>::KIND
        {
            chrome.borrow_mut().note_appearance_change();
        }
        Ok::<bool, ActionError>(on)
    };
    let invoke = flip.clone();
    SearchResult {
        operation_kind: tilo_actions::OperationKind::Effect,
        on_open: std::rc::Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  chrome: &crate::shell::chrome_state::Chrome,
                  args: &crate::shell::search::action_api::ActionArgs| {
                flip(chrome, args).map(drop)
            },
        ),
        on_invoke: Some(std::rc::Rc::new(
            move |_doc: &mut ui::widget::Edits<'_>,
                  _root: &automerge::ObjId,
                  chrome: &crate::shell::chrome_state::Chrome,
                  args: &crate::shell::search::action_api::ActionArgs| {
                invoke(chrome, args).map(|on| {
                    crate::shell::search::ActionReceipt::completed_with(
                        serde_json::json!({ "on": on }),
                    )
                })
            },
        )),
        // A flip that makes the device less safe (notifications on the
        // lock screen) asks the person first, as the palette's other
        // settings do.
        params: vec![
            crate::shell::search::action_api::ActionParam::optional_owned(
                CONFIRM,
                crate::shell::search::action_api::ParamType::Boolean,
                "Confirm a change that makes the device less safe.",
            )
            .with_labelled_choices([("true", t("setting-confirm-weaker"))]),
        ],
        ..SearchResult::nullary(kind.clone(), PROVIDER_ID, id, title, None, 0, |_, _, _| {})
    }
    .with_facts(facts)
}

/// The argument carrying the person's confirmation of a flip.
const CONFIRM: &str = "confirm";

/// A row opening `uri` in Settings.
fn opening(
    kind: &ResourceKind,
    id: String,
    title: String,
    facts: Vec<String>,
    uri: String,
) -> SearchResult {
    SearchResult::nullary_fallible(
        kind.clone(),
        PROVIDER_ID,
        id,
        title,
        Some(facts.join(", ")),
        0,
        move |_doc, _root, chrome| {
            crate::shell::uri_open::open_in_workspace(&uri, chrome)
                .map(|_| ())
                .map_err(|error| {
                    tracing::info!(?error, %uri, "settings page did not open");
                    ActionError::Explained(error.user_message())
                })
        },
    )
    .with_facts(facts)
}

impl SettingsPagesProvider {
    fn build(kind: &ResourceKind, locale: String) -> Built {
        // A switch is offered where the session keeps its section.
        let kept = crate::shell::settings_records::switches_on(
            index::SETTINGS.iter().map(|setting| setting.switch),
        );
        let mut built = Built {
            locale,
            rows: Vec::new(),
            fields: Vec::new(),
            pages: index::PAGES.len(),
            setting_pages: Vec::new(),
            switches: Vec::new(),
            typed: Vec::new(),
        };
        for page in index::PAGES {
            let title = t(&page.title_key());
            let subtitle = t(&page.subtitle_key());
            let mut row = vec![
                (title.to_lowercase(), FieldWeight::TITLE),
                (subtitle.to_lowercase(), FieldWeight::SUBTITLE),
            ];
            row.extend(
                index::names_in_every_locale(&page.title_key())
                    .into_iter()
                    .chain(index::words_in_every_locale(&page.words_key()))
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            built.fields.push(row);
            built.switches.push(None);
            built.typed.push(None);
            built.rows.push(opening(
                kind,
                format!("page:{}", page.slug),
                title,
                vec![subtitle],
                page.link(),
            ));
        }
        for (setting, kept) in index::SETTINGS.iter().zip(&kept) {
            if QUICK_ACTION_SETTINGS.contains(&setting.id) {
                continue;
            }
            let Some((page_row, page)) = index::PAGES
                .iter()
                .enumerate()
                .find(|(_, page)| page.slug == setting.page)
            else {
                continue;
            };
            let name = t(&setting.name_key());
            let page_title = t(&page.title_key());
            // A setting named as its page is ("Bluetooth") is found as the
            // page.
            if name.to_lowercase() == page_title.to_lowercase() {
                continue;
            }
            // A setting's name ranks as a word for its page does: "touchpad"
            // finds the Mouse & touchpad page before the touchpad's speed,
            // and "touchpad speed" finds the speed.
            let mut row = vec![(name.to_lowercase(), FieldWeight::KEYWORD)];
            row.extend(
                index::names_in_every_locale(&setting.name_key())
                    .into_iter()
                    .chain(index::words_in_every_locale(&setting.words_key()))
                    .map(|word| (word, FieldWeight::KEYWORD)),
            );
            let id = format!("setting:{}", setting.id);
            // The page is what tells two settings apart; the app is
            // already the section the row stands in.
            let facts = vec![page_title.clone()];
            if let Some(value) = values::Kind::of(setting.id) {
                // The row changing it, and the one reading it.
                let typed = |reads| Typed {
                    kind: value,
                    reads,
                    name: name.clone(),
                };
                let changing =
                    values::changing(kind, value, setting.id, id.clone(), name.clone(), facts);
                let reading = values::reading(
                    kind,
                    value,
                    setting.id,
                    format!("{id}{READS}"),
                    name.clone(),
                );
                built.push_setting(row.clone(), page_row, None, Some(typed(false)), changing);
                built.push_setting(row, page_row, None, Some(typed(true)), reading);
                continue;
            }
            let switch = setting.switch.filter(|_| kept.is_some());
            let shown = match switch {
                Some(switch) => switching(kind, id, name, facts, switch),
                None => opening(kind, id, name, facts, setting.link()),
            };
            built.push_setting(row, page_row, switch, None, shown);
        }
        built
    }

    /// The pages and settings `needle` finds, best first. A setting is
    /// offered only where it matches better than its page: "battery" is the
    /// Power page, "charge cap" the setting on it.
    fn ranked(&self, built: &Built, needle: &str) -> Vec<SearchResult> {
        let ranks = self.ranker.ranks(
            needle,
            built
                .fields
                .iter()
                .map(|row| row.iter().map(|(text, weight)| (text.as_str(), *weight))),
        );
        let browsing = needle.trim().is_empty();
        let kept = std::cell::OnceCell::new();
        let page_rank = |at: usize| {
            at.checked_sub(built.pages)
                .and_then(|setting| built.setting_pages.get(setting))
                .and_then(|page| ranks.get(*page).copied().flatten())
        };
        built
            .rows
            .iter()
            .zip(&ranks)
            .enumerate()
            .filter_map(|(at, (row, rank))| {
                let rank = (*rank)?;
                // Browsing lists the pages, not every setting on them.
                if browsing && at >= built.pages {
                    return None;
                }
                if page_rank(at).is_some_and(|page| page >= rank) {
                    return None;
                }
                let score = crate::shell::search::provider_score(rank);
                Some(SearchResult {
                    score: if at >= built.pages {
                        score.saturating_sub(1)
                    } else {
                        score
                    },
                    ..self.live(built, at, row, &kept)?
                })
            })
            .collect()
    }
}

impl SettingsPagesProvider {
    /// Row `at` of `built` as it reads now: a switch says whether it is on,
    /// as stored at this moment wherever it last changed, and a typed
    /// setting its value. Every switch is read once into `kept`, and only
    /// when a switch is listed. `None` for a typed setting this session has
    /// nothing to read of (no display has been published).
    fn live(
        &self,
        built: &Built,
        at: usize,
        row: &SearchResult,
        kept: &std::cell::OnceCell<Vec<Option<bool>>>,
    ) -> Option<SearchResult> {
        if let Some(typed) = built.typed.get(at).cloned().flatten() {
            let reading = self.readings.of(typed.kind)?;
            return Some(values::live(
                row,
                typed.kind,
                &reading,
                typed.reads,
                &typed.name,
            ));
        }
        Some(switch_live(built, at, row, kept))
    }

    /// The typed setting `result` changes, where it is stepped: not a
    /// display's, which is chosen and kept.
    fn stepped(&self, result: &SearchResult) -> Option<(values::Kind, &'static str)> {
        let built = self.built.borrow();
        let built = built.as_ref()?;
        let at = built.rows.iter().position(|row| row.id == result.id)?;
        let typed = built.typed.get(at)?.as_ref()?;
        let setting = result.id.strip_prefix("setting:")?;
        let setting = index::setting(setting)?.id;
        (!typed.reads && !typed.kind.is_display()).then_some((typed.kind, setting))
    }
}

/// The suffix of the address of a typed setting's reading row.
const READS: &str = ".get";

/// Row `at` of `built`, a page or a setting that is no typed one, as it
/// reads now.
fn switch_live(
    built: &Built,
    at: usize,
    row: &SearchResult,
    kept: &std::cell::OnceCell<Vec<Option<bool>>>,
) -> SearchResult {
    let badge = built.switches.get(at).copied().flatten().and_then(|_| {
        kept.get_or_init(|| {
            crate::shell::settings_records::switches_on(built.switches.iter().copied())
        })
        .get(at)
        .copied()
        .flatten()
    });
    SearchResult {
        badge: badge.map(super::actions::on_off_badge),
        ..row.clone()
    }
}

impl Default for SettingsPagesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for SettingsPagesProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Settings pages"
    }

    fn display_name(&self) -> String {
        t("search-provider-settings-pages")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        // A setting wears the picture of the page it is on.
        let slug = result.id.strip_prefix("page:").or_else(|| {
            let id = result.id.strip_prefix("setting:")?;
            let id = id.strip_suffix(READS).unwrap_or(id);
            index::SETTINGS
                .iter()
                .find(|setting| setting.id == id)
                .map(|setting| setting.page)
        })?;
        page_icon(slug)
    }

    /// A switch row flips in place: the palette stays open and its row reads
    /// the value it was just switched to.
    fn toggles_in_place(&self, result: &SearchResult) -> bool {
        let built = self.built.borrow();
        built.as_ref().is_some_and(|built| {
            built
                .rows
                .iter()
                .zip(&built.switches)
                .any(|(row, switch)| row.id == result.id && switch.is_some())
        })
    }

    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let built = self.built.borrow();
        let built = built.as_ref()?;
        let at = built.rows.iter().position(|row| row.id == result.id)?;
        crate::shell::search::match_reason_in(needle, result, built.fields.get(at)?)
    }

    /// A person sees one row per typed setting, its value on the row:
    /// reading the value is what the row already does. Agents and the
    /// control socket still reach the reading row.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        _needle: &str,
        _chrome: &crate::shell::chrome_state::ChromeState,
    ) -> bool {
        result.id.starts_with("setting:") && result.id.ends_with(READS)
    }

    /// A typed setting's row steps through the values Settings offers for
    /// it; a display change goes through its own confirmation and is
    /// chosen, not stepped.
    fn adjustable(&self, result: &SearchResult) -> bool {
        self.stepped(result).is_some()
    }

    fn adjust(
        &self,
        result: &SearchResult,
        delta: i8,
        _doc: &mut ui::widget::Edits<'_>,
        _root: &automerge::ObjId,
        chrome: &crate::shell::chrome_state::Chrome,
    ) -> Result<(), ActionError> {
        let (kind, setting) = self
            .stepped(result)
            .ok_or_else(|| ActionError::Refused("this setting is not stepped".into()))?;
        let args = values::stepped_args(kind, delta)?;
        values::apply(chrome, kind, setting, &args).map(drop)
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.with_built(&ctx, |built| {
            crate::shell::search::usage::order_ties_after(self.ranked(built, ctx.needle.trim()))
        })
        .unwrap_or_default()
    }

    /// A page or a setting by its id, whatever was typed: a setting row is
    /// listed only for the words that find it, but its address always
    /// opens it.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        self.with_built(&ctx, |built| {
            let at = built.rows.iter().position(|row| row.id == id)?;
            self.live(built, at, built.rows.get(at)?, &std::cell::OnceCell::new())
        })
        .flatten()
    }
}

impl SettingsPagesProvider {
    /// `read` over the rows built for the desktop's language, building them
    /// first if the language changed; `None` when the kind is not wanted.
    fn with_built<R>(&self, ctx: &SearchContext<'_>, read: impl FnOnce(&Built) -> R) -> Option<R> {
        let kind = self
            .kinds
            .first()
            .filter(|kind| ctx.wanted.contains(kind))?;
        let locale = crate::shell::i18n::current_locale();
        let mut built = self.built.borrow_mut();
        if built.as_ref().is_none_or(|built| built.locale != locale) {
            *built = Some(Self::build(kind, locale));
            #[cfg(test)]
            self.builds.set(self.builds.get().saturating_add(1));
        }
        built.as_ref().map(read)
    }
}

#[cfg(test)]
mod icon_tests {
    #[test]
    fn every_page_wears_its_own_picture_not_the_magnifier() {
        for page in settings_store::index::PAGES {
            let slug = page.slug;
            let icon = super::page_icon(slug).unwrap_or_else(|| panic!("{slug} has no icon"));
            assert_ne!(icon, ui::icons::Icon::Search, "{slug}");
        }
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::ActionArgs;
    use automerge::AutoCommit;

    /// Every yes-or-no setting the index names is a section's field the
    /// session keeps: the palette offers each as the switch it is.
    #[test]
    fn every_indexed_switch_is_a_field_the_session_keeps() {
        for setting in index::SETTINGS {
            let Some(switch) = setting.switch else {
                continue;
            };
            assert!(
                crate::shell::settings_records::switch_is_on(switch).is_some(),
                "{}: {} has no yes-or-no field {}",
                setting.id,
                switch.section,
                switch.field
            );
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn every_setting_switch_flips_both_ways_and_its_row_shows_the_live_value() {
        // The person's settings records, kept apart from every other test's.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let config = tempfile::tempdir().expect("isolated settings");
        let paths = settings_store::SessionPaths::beside(config.path().join("settings.json"));
        let records = settings_store::SessionSettings::open(paths.clone()).expect("records");
        let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(records));

        let provider = SettingsPagesProvider::new();
        let chrome = ChromeState::new(crate::shell::state::WorkspaceTree::bootstrap_default());
        let doc = AutoCommit::new();
        let kind = provider.kinds[0].clone();
        let wanted = [kind];
        // The row as the palette lists it for the setting's name, and as an
        // exact address finds it.
        let listed = |setting: &index::Setting| {
            let state = chrome.borrow();
            let name = t(&setting.name_key());
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: &name,
                    wanted: &wanted,
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == format!("setting:{}", setting.id))
        };
        let resolved = |setting: &index::Setting| {
            let state = chrome.borrow();
            provider.resolve(
                &format!("setting:{}", setting.id),
                SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "",
                    wanted: &wanted,
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                },
            )
        };
        let stored = |switch: index::Switch| {
            crate::shell::settings_records::switch_is_on(switch).expect("kept")
        };
        // Activated as the palette runs it: a flip its section asks the
        // person about first changes nothing until they confirm it.
        let activate = |row: &SearchResult, switch: index::Switch| {
            let run = |args: &ActionArgs| {
                let mut document = AutoCommit::new();
                (row.on_open)(
                    &mut ui::widget::Edits::lend(&mut document),
                    &automerge::ROOT,
                    &chrome,
                    args,
                )
            };
            let before = stored(switch);
            match run(&ActionArgs::none()) {
                Err(ActionError::ConfirmationRequired) => {
                    assert_eq!(
                        stored(switch),
                        before,
                        "{}: nothing changed unconfirmed",
                        row.id
                    );
                    let confirmed = ActionArgs::bind(
                        &row.params,
                        vec![(
                            CONFIRM.to_owned(),
                            crate::shell::search::action_api::ArgValue::Boolean(true),
                        )],
                    )
                    .expect("the switch takes a confirmation");
                    run(&confirmed)
                }
                outcome => outcome,
            }
            .unwrap_or_else(|error| panic!("{} flips: {error:?}", row.id));
        };

        let mut switches = 0;
        for setting in index::SETTINGS {
            let Some(switch) = setting.switch else {
                continue;
            };
            let id = setting.id;
            // A setting the palette offers as a quick action of the same
            // name is that row.
            let Some(row) = resolved(setting) else {
                assert!(QUICK_ACTION_SETTINGS.contains(&id), "{id} is offered");
                continue;
            };
            switches += 1;
            assert!(
                provider.toggles_in_place(&row),
                "{id}: the palette stays open on a switch so its row shows the new state"
            );
            let original = stored(switch);
            assert_eq!(
                row.badge,
                Some(super::super::actions::on_off_badge(original))
            );
            if let Some(listed) = listed(setting) {
                assert_eq!(listed.badge, row.badge, "{id}: listed as resolved");
            }

            let before =
                std::fs::read_to_string(paths.change_log()).map_or(0, |log| log.lines().count());
            activate(&row, switch);
            assert_eq!(
                stored(switch),
                !original,
                "{id}: the first activation flips it"
            );
            let log = std::fs::read_to_string(paths.change_log()).expect("change log");
            let entries: Vec<serde_json::Value> = log
                .lines()
                .skip(before)
                .map(|line| serde_json::from_str(line).expect("one JSON entry per line"))
                .collect();
            assert_eq!(entries.len(), 1, "{id}: one leaf changed: {entries:?}");
            let entry = &entries[0];
            assert_eq!(
                entry["action"],
                format!("{}/field-edit", switch.section),
                "{id}: the switch flips through its section's field edit: {entry}"
            );
            // The log keeps the section's top field whole; a switch nested
            // in it (`focus.enabled`) is read inside.
            let mut path = switch.path();
            assert_eq!(entry["leaf"], path.next().unwrap_or_default(), "{entry}");
            let after = path.fold(&entry["after"], |value, field| &value[field]);
            assert_eq!(*after, !original, "{entry}");
            let row = resolved(setting).expect("still offered");
            assert_eq!(
                row.badge,
                Some(super::super::actions::on_off_badge(!original)),
                "{id}: the row shows the state it was just switched to"
            );

            // Changed elsewhere (in Settings, on another device): the row
            // reads what is stored, not what search last wrote.
            crate::shell::settings_records::flip(
                switch,
                true,
                &settings_store::InvokingAction::action("elsewhere"),
            )
            .expect("changed elsewhere");
            let row = resolved(setting).expect("still offered");
            assert_eq!(
                row.badge,
                Some(super::super::actions::on_off_badge(original)),
                "{id}: the row follows a change made elsewhere"
            );
            activate(&row, switch);
            assert_eq!(stored(switch), !original, "{id}: it flips the live value");
            activate(&resolved(setting).expect("still offered"), switch);
            assert_eq!(
                stored(switch),
                original,
                "{id}: the second activation puts the original value back"
            );
        }
        assert!(switches > 0, "the palette offers the index's switches");
    }
}
