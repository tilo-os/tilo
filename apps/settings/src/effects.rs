//! Everything Settings does, as one typed effect vocabulary.
//!
//! Settings edits no document of its own: a preference lives in the
//! session's settings records (read and written through the app's settings
//! document, [`crate::store`]), a device's state lives in the device. Every
//! change a control, the palette, an agent or a script makes is therefore a
//! [`SettingsEffect`], answered by [`crate::SettingsApp`]'s
//! [`ResourceSurface::effect`](ui::framework::ResourceSurface::effect).
//!
//! The vocabulary is composed of one `#[derive(Effect)]` enum per area of
//! Settings ([`settings_effects!`]): each area declares its own effects and
//! answers them in its own module, and every effect id is unique across the
//! whole vocabulary. A control binds one of an area's effects with
//! [`effect`], which names it as part of [`SettingsEffect`].

use ui::framework::{EffectAnswer, EffectCx};
use ui::tilo_resource::{self, ActionRef, Effect, EffectDecl};

use crate::categories::Category;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

/// The operation every Settings effect travels under.
pub const OPERATION: &str = "settings.effect";

/// Declare [`SettingsEffect`] as the union of the areas' vocabularies, each
/// answered by its module's `answer(app, effect, cx)`.
macro_rules! settings_effects {
    ($($variant:ident($ty:ty) => $answer:path),* $(,)?) => {
        /// Every effect Settings answers. See the [module docs](self).
        #[derive(Clone, Debug, PartialEq)]
        pub enum SettingsEffect {
            $($variant($ty),)*
        }

        $(impl From<$ty> for SettingsEffect {
            fn from(effect: $ty) -> Self {
                Self::$variant(effect)
            }
        })*

        impl Effect for SettingsEffect {
            const OPERATION: &'static str = OPERATION;

            fn declarations() -> Vec<EffectDecl> {
                ui::tilo_resource::profiling::measure("settings.flattened_declarations", || {
                    let mut declarations = Vec::new();
                    $(declarations.extend(<$ty as Effect>::declarations());)*
                    declarations
                })
            }

            fn id(&self) -> &'static str {
                match self {
                    $(Self::$variant(effect) => effect.id(),)*
                }
            }

            // A row binds one area's effect. Flattening every area's
            // declarations here repeats unrelated metadata work for each row.
            // The child vocabulary shares this union's operation and payload.
            fn bound(&self) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound(),)*
                }
            }

            fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound_asking(asking),)*
                }
            }

            fn bound_asking_params(&self, params: Vec<tilo_actions::ActionParam>) -> tilo_actions::ActionSpec {
                match self {
                    $(Self::$variant(effect) => effect.bound_asking_params(params),)*
                }
            }

            fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
                match self {
                    $(Self::$variant(effect) => effect.arguments(),)*
                }
            }

            fn decode(
                id: &str,
                arguments: &serde_json::Map<String, serde_json::Value>,
            ) -> Result<Self, String> {
                $(if <$ty as Effect>::declarations().iter().any(|decl| decl.id == id) {
                    return <$ty as Effect>::decode(id, arguments).map(Self::$variant);
                })*
                Err(format!("Settings has no effect `{id}`"))
            }
        }

        /// Answer `effect` on `app`, in the module of its area.
        pub(crate) fn answer(
            app: &mut SettingsApp,
            effect: SettingsEffect,
            cx: &mut EffectCx<Settings, SettingsApp>,
        ) -> EffectAnswer {
            match effect {
                $(SettingsEffect::$variant(effect) => $answer(app, effect, cx),)*
            }
        }
    };
}

settings_effects! {
    Navigation(NavigationEffect) => answer_navigation,
    Appearance(crate::panes::appearance::AppearanceEffect) => crate::panes::appearance::answer,
    Accessibility(crate::panes::accessibility::AccessibilityEffect) => crate::panes::accessibility::answer,
    DefaultApps(crate::panes::personal_apps::DefaultAppsEffect) => crate::panes::personal_apps::answer,
    TimeDate(crate::panes::time_date::TimeDateEffect) => crate::panes::time_date::answer,
    Shortcuts(crate::panes::keyboard_shortcuts::ShortcutEffect) => crate::panes::keyboard_shortcuts::answer,
    Input(crate::panes::mouse_keyboard::InputEffect) => crate::panes::mouse_keyboard::answer,
    Power(crate::panes::power::PowerEffect) => crate::panes::power::answer,
    Updates(crate::panes::updates::UpdatesEffect) => crate::panes::updates::answer,
    DiskUnlock(crate::panes::disk_unlock::DiskUnlockEffect) => crate::panes::disk_unlock::answer,
    Drives(crate::panes::drives::DrivesEffect) => crate::panes::drives::answer,
    Storage(crate::panes::storage::StorageEffect) => crate::panes::storage::answer,
    About(crate::panes::about::AboutEffect) => crate::panes::about::answer,
    Permissions(crate::panes::permissions::PermissionsEffect) => crate::panes::permissions::answer,
    Printers(crate::panes::printers::PrinterEffect) => crate::panes::printers::answer,
    Users(crate::panes::users::UsersEffect) => crate::panes::users::answer,
    Bluetooth(crate::panes::bluetooth::BluetoothEffect) => crate::panes::bluetooth::answer,
    Speakers(crate::panes::speakers::SpeakerEffect) => crate::panes::speakers::answer,
    SpeakerFlow(crate::panes::speakers::SpeakerFlow) => crate::panes::speakers::answer_flow,
    AgentModels(crate::panes::agent_models::AgentModelsEffect) => crate::panes::agent_models::answer,
    DeviceMesh(crate::panes::device_mesh::DeviceMeshEffect) => crate::panes::device_mesh::answer,
    Audio(crate::panes::audio::AudioEffect) => crate::panes::audio::answer,
    Accounts(crate::panes::accounts::AccountsEffect) => crate::panes::accounts::answer,
    Displays(crate::panes::display::DisplayEffect) => crate::panes::display::answer,
    Network(crate::panes::network::NetworkEffect) => crate::panes::network::answer,
}

/// One area of Settings whose live rows show what it reads of the device:
/// its printers, accounts, audio graph. Reading is what costs — on a device
/// it runs commands and D-Bus calls — so an area reads when first asked,
/// and the published rows of a live window name an area only once it has
/// read ([`SettingsApp::publishes_area`]).
pub(crate) struct LiveArea {
    /// The profiling phase its rows are measured under.
    pub(crate) name: &'static str,
    /// The pane that shows the area, and so reads it when shown.
    pub(crate) pane: Category,
    /// Whether the area has read what its rows show.
    pub(crate) is_open: fn(&SettingsApp) -> bool,
    /// Its rows, reading the area first if it has not.
    pub(crate) rows: fn(&SettingsApp) -> Vec<ui::framework::LiveRow>,
    /// How the window's warm-up reads the area off the UI thread, for an
    /// area whose read blocks on the device; `None` reads it in a sync.
    pub(crate) background: Option<BackgroundRead>,
}

/// An area's device read on the background executor
/// ([`crate::device_read::DeviceRead`]): started by the warm-up, and taken
/// in by the area's model on the UI thread once it has ended.
pub(crate) struct BackgroundRead {
    /// Start the read; `false` when the area reads in a sync instead.
    pub(crate) start: fn(&SettingsApp) -> bool,
    /// Whether the started read has ended.
    pub(crate) finished: fn(&SettingsApp) -> bool,
}

/// Every area with live rows, in the order their rows are published.
pub(crate) const LIVE_AREAS: &[LiveArea] = &[
    LiveArea {
        name: "settings.rows.notifications",
        pane: Category::Notifications,
        is_open: |app| app.notifications().is_open(),
        rows: |app| app.notifications().rows(),
        background: Some(BackgroundRead {
            start: |app| app.notifications().start_read(),
            finished: |app| app.notifications().read_finished(),
        }),
    },
    LiveArea {
        name: "settings.rows.default_apps",
        pane: Category::DefaultApps,
        is_open: |app| app.default_apps().is_open(),
        rows: |app| app.default_apps().rows(),
        background: Some(BackgroundRead {
            start: |app| app.default_apps().start_read(),
            finished: |app| app.default_apps().read_finished(),
        }),
    },
    // Input reads a small snapshot file the compositor keeps, on a timer of
    // its own; it has nothing slow to put off.
    LiveArea {
        name: "settings.rows.input",
        pane: Category::Keyboard,
        is_open: |_| true,
        rows: |app| app.input().rows(),
        background: None,
    },
    // Printers read on a thread of their own.
    LiveArea {
        name: "settings.rows.printers",
        pane: Category::Printers,
        is_open: |app| app.printers().is_open(),
        rows: |app| app.printers().rows(),
        background: None,
    },
    LiveArea {
        name: "settings.rows.users",
        pane: Category::Users,
        is_open: |app| app.users().is_open(),
        rows: |app| app.users().rows(),
        background: Some(BackgroundRead {
            start: |app| app.users().start_read(),
            finished: |app| app.users().read_finished(),
        }),
    },
    LiveArea {
        name: "settings.rows.bluetooth",
        pane: Category::Bluetooth,
        is_open: |app| app.bluetooth().is_open(),
        rows: |app| app.bluetooth().rows(),
        background: Some(BackgroundRead {
            start: |app| app.bluetooth().start_read(),
            finished: |app| app.bluetooth().read_finished(),
        }),
    },
    // Speakers ask the broker on a thread of their own.
    LiveArea {
        name: "settings.rows.speakers",
        pane: Category::Speakers,
        is_open: |app| app.speakers().is_open(),
        rows: |app| app.speakers().rows(),
        background: None,
    },
    LiveArea {
        name: "settings.rows.agent_models",
        pane: Category::AgentModels,
        is_open: |app| app.agent_models().is_open(),
        rows: |app| app.agent_models().rows(),
        background: Some(BackgroundRead {
            start: |app| app.agent_models().start_read(),
            finished: |app| app.agent_models().read_finished(),
        }),
    },
    LiveArea {
        name: "settings.rows.device_mesh",
        pane: Category::DeviceMesh,
        is_open: |app| app.device_mesh().is_open(),
        rows: |app| app.device_mesh().rows(),
        background: Some(BackgroundRead {
            start: |app| app.device_mesh().start_read(),
            finished: |app| app.device_mesh().read_finished(),
        }),
    },
    LiveArea {
        name: "settings.rows.audio",
        pane: Category::Sound,
        is_open: |app| app.audio().is_open(),
        rows: |app| app.audio().rows(),
        background: Some(BackgroundRead {
            start: |app| app.audio().start_read(),
            finished: |app| app.audio().read_finished(),
        }),
    },
    LiveArea {
        name: "settings.rows.accounts",
        pane: Category::ExternalAccounts,
        is_open: |app| app.accounts().is_open(),
        rows: |app| app.accounts().rows(),
        background: Some(BackgroundRead {
            start: |app| app.accounts().start_read(),
            finished: |app| app.accounts().read_finished(),
        }),
    },
    // Displays probe brightness on threads of their own; the rest is a few
    // small files.
    LiveArea {
        name: "settings.rows.displays",
        pane: Category::Displays,
        is_open: |app| app.displays().is_open(),
        rows: |app| app.displays().rows(),
        background: None,
    },
    // Networks are read on a thread of their own.
    LiveArea {
        name: "settings.rows.network",
        pane: Category::Network,
        is_open: |app| app.network().is_open(),
        rows: |app| app.network().rows(),
        background: None,
    },
    // Disk unlock's rows show only what its pane has already read.
    LiveArea {
        name: "settings.rows.disk_unlock",
        pane: Category::Permissions,
        is_open: |_| true,
        rows: |app| app.disk_unlock().rows(),
        background: None,
    },
    // Remembered drives' rows show only what the Storage pane has read.
    LiveArea {
        name: "settings.rows.drives",
        pane: Category::Storage,
        is_open: |_| true,
        rows: |app| app.drives().rows(),
        background: None,
    },
];

/// When the areas of a window read. A window that reaches the device opens
/// on its first pane without reading every other area first: until its
/// first frame is drawn it publishes the areas that have read (the pane
/// shown has), then reads one more area at a time until every one has
/// ([`AreaReads::step`]), so a little after the first frame its published
/// rows are what reading them all up front published. An area whose read
/// blocks on the device ([`LiveArea::background`]) reads on the background
/// executor and is taken in by the sync its end wakes; the others read in a
/// sync, one per sync. A screenshot or a test reads stand-ins, which cost
/// nothing: it publishes every area at once.
#[derive(Default)]
pub(crate) struct AreaReads {
    /// Publish only the areas that have read.
    deferred: std::cell::Cell<bool>,
    /// The window has drawn a frame, so reading may begin.
    presented: std::cell::Cell<bool>,
    /// A wake for the next read is on its way.
    woken: std::cell::Cell<bool>,
    /// The index in [`LIVE_AREAS`] reading continues from.
    next: std::cell::Cell<usize>,
    /// The area whose read runs on the background executor; it wakes the
    /// window when it ends, so no sync asks for another meanwhile.
    in_flight: std::cell::Cell<Option<usize>>,
}

/// What one step of the warm-up did ([`AreaReads::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// An area read, or its background read was taken in: the next sync
    /// goes on.
    Read,
    /// An area reads on the background executor; its end wakes the window.
    Waiting,
    /// Every area has read; every area is published from now on.
    Done,
}

#[cfg(test)]
thread_local! {
    static DEFERRED_IN_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Make the windows this thread attaches next read their areas as a live
/// window does, over the stand-ins a test reads.
#[cfg(test)]
pub(crate) fn defer_area_reads_in_tests(deferred: bool) {
    DEFERRED_IN_TESTS.with(|cell| cell.set(deferred));
}

impl AreaReads {
    /// The reads of a window attached through `cx`.
    pub(crate) fn of(cx: &crate::host::PaneCx) -> Self {
        #[cfg(test)]
        let deferred = cx.reaches_devices() || DEFERRED_IN_TESTS.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let deferred = cx.reaches_devices();
        Self {
            deferred: std::cell::Cell::new(deferred),
            ..Self::default()
        }
    }

    /// Whether only the areas that have read are published.
    pub(crate) fn deferred(&self) -> bool {
        self.deferred.get()
    }

    /// The window drew a frame.
    pub(crate) fn presented(&self) {
        self.presented.set(true);
    }

    /// Whether this sync reads the next area: the window has drawn and not
    /// every area has read. Taking it consumes the pending wake.
    pub(crate) fn take_due(&self) -> bool {
        self.woken.set(false);
        self.deferred.get() && self.presented.get()
    }

    /// The next area to read in `app`, with its index: the first after the
    /// last one read that has not. Each area is passed once, so reading
    /// ends after one pass even if an area's rows never read it; then every
    /// area is published.
    fn take_next(&self, app: &SettingsApp) -> Option<(usize, &'static LiveArea)> {
        let start = self.next.get();
        let found = LIVE_AREAS
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, area)| !(area.is_open)(app));
        let Some((index, area)) = found else {
            self.next.set(LIVE_AREAS.len());
            self.deferred.set(false);
            return None;
        };
        self.next.set(index + 1);
        Some((index, area))
    }

    /// Read on in `app`: take in the background read that has ended, or
    /// start the next area's read — on the background executor when its
    /// read blocks, else here. At most one background read runs at a time.
    pub(crate) fn step(&self, app: &SettingsApp) -> Step {
        if let Some(index) = self.in_flight.get() {
            let Some(area) = LIVE_AREAS.get(index) else {
                self.in_flight.set(None);
                return Step::Read;
            };
            let ended = (area.is_open)(app)
                || area
                    .background
                    .as_ref()
                    .is_none_or(|read| (read.finished)(app));
            if !ended {
                return Step::Waiting;
            }
            self.in_flight.set(None);
            // The area builds its model from what was read, here.
            let _ = (area.rows)(app);
            return Step::Read;
        }
        let Some((index, area)) = self.take_next(app) else {
            return Step::Done;
        };
        // A read the pane shown already started is waited for the same
        // way: taking it in now would wait for it on the UI thread.
        if let Some(read) = &area.background
            && ((read.start)(app) || !(read.finished)(app))
        {
            self.in_flight.set(Some(index));
            return Step::Waiting;
        }
        let _ = (area.rows)(app);
        Step::Read
    }

    /// Ask for a sync to read the next area, unless one is already asked
    /// for, a background read will wake the window when it ends, or reading
    /// has not begun.
    pub(crate) fn wake(&self) {
        if self.deferred.get()
            && self.presented.get()
            && self.in_flight.get().is_none()
            && !self.woken.replace(true)
        {
            ui::WakeHandle::new().wake();
        }
    }
}

/// The live rows Settings publishes — the devices, networks, accounts and
/// other entities outside any document that it shows — with the effects
/// each offers bound to it: one per pane, then each area's that
/// [`SettingsApp::publishes_area`].
pub(crate) fn rows(app: &SettingsApp) -> Vec<ui::framework::LiveRow> {
    use ui::tilo_resource::profiling::measure;
    let mut rows = measure("settings.rows.panes", pane_rows);
    for area in LIVE_AREAS {
        if app.publishes_area(area) {
            rows.extend(measure(area.name, || (area.rows)(app)));
        }
    }
    rows
}

/// The live row `id`, published or not: an address an agent kept or a
/// search hit names reads the area it belongs to.
pub(crate) fn row(app: &SettingsApp, id: &str) -> Option<ui::framework::LiveRow> {
    fn find(rows: Vec<ui::framework::LiveRow>, id: &str) -> Option<ui::framework::LiveRow> {
        rows.into_iter().find(|row| row.id() == id)
    }
    // The areas that have read are asked first, so a published row never
    // reads another area.
    find(rows(app), id).or_else(|| {
        LIVE_AREAS
            .iter()
            .filter(|area| !app.publishes_area(area))
            .find_map(|area| find((area.rows)(app), id))
    })
}

/// The effects that differ from their declaration on this device now.
pub(crate) fn statuses(app: &SettingsApp) -> Vec<ui::framework::EffectStatus> {
    let software = app.software();
    let mut statuses = crate::panes::accessibility::statuses(software.speech.is_some());
    statuses.extend(app.bluetooth().statuses());
    statuses.extend(app.network().statuses());
    statuses
}

/// `effect` of one area, named as part of [`SettingsEffect`], for binding a
/// control: `cx.bind(toggle, effect(AppearanceEffect::SET_HIGH_CONTRAST))`.
#[must_use]
pub const fn effect<E, A: serde::Serialize>(
    effect: ActionRef<E, A>,
) -> ActionRef<SettingsEffect, A> {
    ActionRef::new(effect.id, effect.params)
}

/// The live row of `category`'s pane.
#[must_use]
pub fn pane_row_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

/// What opens `category`'s pane, offered on its row ([`pane_row_id`]).
#[must_use]
pub fn open_pane(category: Category) -> SettingsEffect {
    NavigationEffect::OpenPane {
        pane: category.slug().to_owned(),
    }
    .into()
}

/// Open `category`'s pane in `app` through its row, as the sidebar does.
#[cfg(test)]
pub(crate) fn show_pane(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    category: Category,
) {
    app.effect_on(&pane_row_id(category), &open_pane(category))
        .expect("the pane opens");
}

/// The pane whose live row is `row` ([`pane_row_id`]).
#[must_use]
pub fn pane_of_row(row: &str) -> Option<Category> {
    Category::from_slug(row.strip_prefix("settings.section:")?)
}

/// One row per pane, each opening it.
fn pane_rows() -> Vec<ui::framework::LiveRow> {
    Category::ALL
        .iter()
        .map(|category| {
            ui::framework::LiveRow::new(
                pane_row_id(*category),
                "settings.section",
                category.label(),
            )
            .operation(open_pane(*category).bound())
        })
        .collect()
}

/// Moving between the panes of Settings.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NavigationEffect {
    /// Open a pane of Settings.
    #[effect(view, label = "Open settings pane")]
    OpenPane {
        #[param("The pane: its id, such as `appearance` or `network`")]
        pane: String,
    },
}

fn answer_navigation(
    app: &SettingsApp,
    effect: NavigationEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        NavigationEffect::OpenPane { pane } => {
            let category = Category::from_slug(&pane)
                .ok_or_else(|| format!("Settings has no pane `{pane}`"))?;
            app.request_pane(category);
            Ok(ui::framework::Effected::done())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flattened(SettingsEffect);

    #[test]
    fn mounted_live_rows_do_not_rebuild_unrelated_pane_declarations() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let (rows, phases) = app
            .surface::<SettingsApp, _>(|app| {
                ui::tilo_resource::profiling::observe(|| super::rows(app))
            })
            .unwrap();
        assert!(
            rows.iter()
                .any(|row| row.id() == "settings.display-output:eDP-1")
        );
        assert!(
            !phases
                .iter()
                .any(|phase| phase.phase == "settings.flattened_declarations"),
            "actual mounted rows must bind only their own pane vocabulary"
        );
    }

    #[test]
    fn mounted_display_asking_bindings_preserve_metadata_payload_and_output_custody() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let rows = app
            .surface::<SettingsApp, _>(|app| super::rows(app))
            .unwrap();
        let row = rows
            .into_iter()
            .find(|row| row.id() == "settings.display-output:eDP-1")
            .expect("the actual connected output is published")
            .into_resource();
        let mut compared = 0;
        for spec in row.actions.iter().filter(|spec| !spec.params().is_empty()) {
            let mut args = serde_json::Map::new();
            for param in spec.params() {
                let value = param.choices.first().map_or_else(
                    || match param.ty {
                        tilo_actions::ParamType::Text => serde_json::json!("observed"),
                        tilo_actions::ParamType::Integer => serde_json::json!(20),
                        tilo_actions::ParamType::Number => serde_json::json!(0.5),
                        tilo_actions::ParamType::Boolean => serde_json::json!(true),
                        other => panic!("unexpected actual display parameter: {other:?}"),
                    },
                    |value| serde_json::json!(value),
                );
                args.insert(param.name.to_string(), value);
            }
            let published = spec.build_json(&args).unwrap();
            let effect = SettingsEffect::from_payload(&published.payload).unwrap();
            let reference = Flattened(effect.clone()).bound_asking_params(spec.params().to_vec());
            let direct = effect.bound_asking_params(spec.params().to_vec());
            let survey = |spec| {
                tilo_actions::door::survey(&[tilo_actions::Resource::new(
                    "comparison",
                    "comparison",
                    "comparison",
                )
                .with_action(spec)])
            };
            assert_eq!(survey(direct.clone()), survey(reference.clone()));
            assert_eq!(
                direct.need_for(Some(&args), None),
                reference.need_for(Some(&args), None)
            );
            assert_eq!(
                direct.confirmation_for_json(&args),
                reference.confirmation_for_json(&args)
            );
            let actual = direct.build_json(&args).unwrap();
            let expected = reference.build_json(&args).unwrap();
            assert_eq!(actual.type_id, expected.type_id);
            assert_eq!(actual.payload, expected.payload);
            assert_eq!(actual.payload, published.payload);
            let asking: Vec<_> = spec.params().iter().map(|param| &*param.name).collect();
            assert_eq!(
                survey(effect.bound_asking(&asking)),
                survey(Flattened(effect).bound_asking(&asking))
            );
            args.insert("display".to_string(), serde_json::json!("foreign-output"));
            assert!(
                spec.build_json(&args).is_err(),
                "the output identity is not caller controlled"
            );
            compared += 1;
        }
        assert!(
            compared >= 7,
            "the actual output's parameter actions are observed"
        );
    }

    impl Effect for Flattened {
        const OPERATION: &'static str = super::OPERATION;

        fn declarations() -> Vec<EffectDecl> {
            SettingsEffect::declarations()
        }

        fn id(&self) -> &'static str {
            self.0.id()
        }

        fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
            self.0.arguments()
        }

        fn decode(
            id: &str,
            arguments: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<Self, String> {
            SettingsEffect::decode(id, arguments).map(Self)
        }
    }

    #[test]
    fn mounted_live_row_bindings_preserve_flattened_metadata_and_payloads() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let resource_rows = app.surface::<SettingsApp, _>(|app| rows(app)).unwrap();
        let mut compared = 0;
        for row in resource_rows {
            for spec in row.into_resource().actions {
                if spec.operation_type.as_deref() != Some(super::OPERATION)
                    || !spec.params().is_empty()
                {
                    continue;
                }
                let published = spec.build_json(&serde_json::Map::new()).unwrap();
                let effect = SettingsEffect::from_payload(&published.payload).unwrap();
                let direct = effect.bound();
                let flattened = Flattened(effect).bound();
                assert_eq!(
                    tilo_actions::door::survey(&[tilo_actions::Resource::new(
                        "comparison",
                        "comparison",
                        "comparison"
                    )
                    .with_action(direct.clone())]),
                    tilo_actions::door::survey(&[tilo_actions::Resource::new(
                        "comparison",
                        "comparison",
                        "comparison"
                    )
                    .with_action(flattened.clone())]),
                    "all published metadata including role/risk/result and confirmation is preserved"
                );
                assert_eq!(
                    direct.need_for(Some(&serde_json::Map::new()), None),
                    flattened.need_for(Some(&serde_json::Map::new()), None),
                    "invocation permission classification is unchanged",
                );
                assert_eq!(
                    direct.confirmation_for_json(&serde_json::Map::new()),
                    flattened.confirmation_for_json(&serde_json::Map::new()),
                    "bound dynamic risk retains the same confirmation policy",
                );
                let actual = direct.build_json(&serde_json::Map::new()).unwrap();
                let reference = flattened.build_json(&serde_json::Map::new()).unwrap();
                assert_eq!(actual.type_id, reference.type_id);
                assert_eq!(actual.payload, reference.payload);
                assert_eq!(
                    actual.payload, published.payload,
                    "the actual published row carries the same effect arguments"
                );
                compared += 1;
            }
        }
        assert!(
            compared >= Category::ALL.len(),
            "the actual mounted pane/live rows are observed"
        );
    }

    /// One vocabulary: every effect id names exactly one effect, and each
    /// decodes back to the value that encoded it.
    #[test]
    fn effect_ids_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for decl in SettingsEffect::declarations() {
            assert!(
                seen.insert(decl.id),
                "effect `{}` is declared twice",
                decl.id
            );
        }
        let open = SettingsEffect::from(NavigationEffect::OpenPane {
            pane: "appearance".into(),
        });
        assert_eq!(
            SettingsEffect::decode(open.id(), &open.arguments()),
            Ok(open)
        );
        assert!(SettingsEffect::decode("no-such-effect", &serde_json::Map::new()).is_err());
    }
}

#[cfg(test)]
#[path = "area_reads_tests.rs"]
mod area_reads_tests;
