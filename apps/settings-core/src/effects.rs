//! Everything Settings does, as one typed effect vocabulary.
//!
//! Settings edits no document of its own: a preference lives in the
//! session's settings records (read and written through the app's settings
//! document, `crate::store`), a device's state lives in the device. Every
//! change a control, the palette, an agent or a script makes is therefore a
//! [`SettingsEffect`], answered by [`crate::SettingsApp`]'s
//! [`ResourceSurface::effect`](ui::framework::ResourceSurface::effect).
//!
//! The vocabulary is composed of one `#[derive(Effect)]` enum per area of
//! Settings (`settings_effects!`): each area declares its own effects and
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

/// One area's effect vocabulary as Settings answers it: its declarations,
/// how one of its effects is read back, and the area's `answer`. The union
/// ([`SettingsEffect`]) reaches every area through these alone, so the code
/// that dispatches effects names no area: an area brings its family
/// ([`effect_family!`]) and [`crate::area::registry`] lists them.
pub struct EffectFamily {
    /// Every effect the area declares.
    pub declarations: fn() -> Vec<EffectDecl>,
    /// Read the area's effect `id` back from its arguments.
    pub decode: DecodeEffect,
    /// Answer one of the area's effects.
    pub answer: AnswerEffect,
}

/// [`EffectFamily::decode`].
pub type DecodeEffect =
    fn(&str, &serde_json::Map<String, serde_json::Value>) -> Result<Box<dyn AreaEffect>, String>;

/// [`EffectFamily::answer`].
pub type AnswerEffect = fn(
    &mut SettingsApp,
    Box<dyn std::any::Any>,
    &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer;

/// An area's effect vocabulary: the `#[derive(Effect)]` enum and the family
/// that answers it.
pub trait AreaVocabulary: Effect + Clone + PartialEq + std::fmt::Debug + 'static {
    /// Its family.
    const FAMILY: &'static EffectFamily;
}

/// One area's effect, behind a reference: what [`SettingsEffect`] holds.
pub trait AreaEffect: std::fmt::Debug {
    /// [`Effect::id`].
    fn id(&self) -> &'static str;
    /// [`Effect::arguments`].
    fn arguments(&self) -> serde_json::Map<String, serde_json::Value>;
    /// [`Effect::bound`].
    fn bound(&self) -> tilo_actions::ActionSpec;
    /// [`Effect::bound_asking`].
    fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec;
    /// [`Effect::bound_asking_params`].
    fn bound_asking_params(
        &self,
        params: Vec<tilo_actions::ActionParam>,
    ) -> tilo_actions::ActionSpec;
    /// A copy of the effect.
    fn clone_effect(&self) -> Box<dyn AreaEffect>;
    /// Whether `other` is the same effect.
    fn same_as(&self, other: &dyn AreaEffect) -> bool;
    /// The effect, to compare or take back as its own type.
    fn as_any(&self) -> &dyn std::any::Any;
    /// The effect, to answer as its own type.
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
    /// Its area's family.
    fn family(&self) -> &'static EffectFamily;
}

impl<E: AreaVocabulary> AreaEffect for E {
    fn id(&self) -> &'static str {
        Effect::id(self)
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        Effect::arguments(self)
    }

    fn bound(&self) -> tilo_actions::ActionSpec {
        Effect::bound(self)
    }

    fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec {
        Effect::bound_asking(self, asking)
    }

    fn bound_asking_params(
        &self,
        params: Vec<tilo_actions::ActionParam>,
    ) -> tilo_actions::ActionSpec {
        Effect::bound_asking_params(self, params)
    }

    fn clone_effect(&self) -> Box<dyn AreaEffect> {
        Box::new(self.clone())
    }

    fn same_as(&self, other: &dyn AreaEffect) -> bool {
        other.as_any().downcast_ref::<E>() == Some(self)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn family(&self) -> &'static EffectFamily {
        E::FAMILY
    }
}

/// Make `$ty`, an area's `#[derive(Effect)]` enum, a vocabulary of
/// [`SettingsEffect`] answered by `$answer(app, effect, cx)`.
#[macro_export]
macro_rules! effect_family {
    ($ty:ty => $answer:path) => {
        impl $crate::effects::AreaVocabulary for $ty {
            const FAMILY: &'static $crate::effects::EffectFamily = &$crate::effects::EffectFamily {
                declarations: <$ty as ui::tilo_resource::Effect>::declarations,
                decode: |id, arguments| {
                    <$ty as ui::tilo_resource::Effect>::decode(id, arguments)
                        .map(|effect| Box::new(effect) as Box<dyn $crate::effects::AreaEffect>)
                },
                answer: |app, effect, cx| match effect.downcast::<$ty>() {
                    Ok(effect) => $answer(app, *effect, cx),
                    Err(_) => Err("an effect reached the wrong area of Settings".to_owned()),
                },
            };
        }
    };
}
pub use crate::effect_family;

/// Every effect Settings answers: one area's effect. See the
/// [module docs](self).
pub struct SettingsEffect(Box<dyn AreaEffect>);

impl Clone for SettingsEffect {
    fn clone(&self) -> Self {
        Self(self.0.clone_effect())
    }
}

impl PartialEq for SettingsEffect {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_as(&*other.0)
    }
}

impl std::fmt::Debug for SettingsEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<E: AreaVocabulary> From<E> for SettingsEffect {
    fn from(effect: E) -> Self {
        Self(Box::new(effect))
    }
}

impl Effect for SettingsEffect {
    const OPERATION: &'static str = OPERATION;

    fn declarations() -> Vec<EffectDecl> {
        ui::tilo_resource::profiling::measure("settings.flattened_declarations", || {
            let mut declarations = Vec::new();
            for family in &crate::area::registry().families {
                declarations.extend((family.declarations)());
            }
            declarations
        })
    }

    fn id(&self) -> &'static str {
        self.0.id()
    }

    // A row binds one area's effect. Flattening every area's declarations
    // here repeats unrelated metadata work for each row. The child
    // vocabulary shares this union's operation and payload.
    fn bound(&self) -> tilo_actions::ActionSpec {
        self.0.bound()
    }

    fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec {
        self.0.bound_asking(asking)
    }

    fn bound_asking_params(
        &self,
        params: Vec<tilo_actions::ActionParam>,
    ) -> tilo_actions::ActionSpec {
        self.0.bound_asking_params(params)
    }

    fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        self.0.arguments()
    }

    fn decode(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        for family in &crate::area::registry().families {
            if (family.declarations)().iter().any(|decl| decl.id == id) {
                return (family.decode)(id, arguments).map(Self);
            }
        }
        Err(format!("Settings has no effect `{id}`"))
    }
}

/// Answer `effect` on `app`, in the module of its area.
pub fn answer(
    app: &mut SettingsApp,
    effect: SettingsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let family = effect.0.family();
    (family.answer)(app, effect.0.into_any(), cx)
}

effect_family!(NavigationEffect => answer_navigation);

// The provider panel's effect schema is declared beside the panel, so the
// standalone provider host and Settings publish one set of operations. The
// area that shows the panel answers them: `answer_provider` hands each
// typed effect to it.
effect_family!(provider_settings_ui::ProviderEffect => answer_provider);

/// Answer one effect of the shared provider vocabulary in the area whose
/// pane offers the provider panel.
#[allow(
    clippy::needless_pass_by_ref_mut,
    clippy::needless_pass_by_value,
    reason = "a family's answer signature is `AnswerEffect`'s: the effect arrives owned and the window is lent mutably"
)]
fn answer_provider(
    app: &mut SettingsApp,
    effect: provider_settings_ui::ProviderEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    for area in app.areas().iter() {
        if let Some(answer) = area.answer_provider(&effect, cx) {
            return answer;
        }
    }
    Err("this window shows no provider settings".to_owned())
}

/// Settings' own part: moving between the panes.
pub const PART: crate::area::Part = crate::area::Part {
    areas: &[],
    panes: &[],
    families: &[(10, NavigationEffect::FAMILY)],
    live_areas: &[],
    sections: &[],
    settings: &[],
};

/// One area of Settings whose live rows show what it reads of the device:
/// its printers, accounts, audio graph. Reading is what costs — on a device
/// it runs commands and D-Bus calls — so an area reads when first asked,
/// and the published rows of a live window name an area only once it has
/// read ([`SettingsApp::publishes_area`]).
pub struct LiveArea {
    /// The profiling phase its rows are measured under.
    pub name: &'static str,
    /// The pane that shows the area, and so reads it when shown.
    pub pane: Category,
    /// Whether the area has read what its rows show.
    pub is_open: fn(&SettingsApp) -> bool,
    /// Its rows, reading the area first if it has not.
    pub rows: fn(&SettingsApp) -> Vec<ui::framework::LiveRow>,
    /// How the window's warm-up reads the area off the UI thread, for an
    /// area whose read blocks on the device; `None` reads it in a sync.
    pub background: Option<BackgroundRead>,
}

/// An area's device read on the background executor
/// ([`crate::device_read::DeviceRead`]): started by the warm-up, and taken
/// in by the area's model on the UI thread once it has ended.
pub struct BackgroundRead {
    /// Start the read; `false` when the area reads in a sync instead.
    pub start: fn(&SettingsApp) -> bool,
    /// Whether the started read has ended.
    pub finished: fn(&SettingsApp) -> bool,
}

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
pub struct AreaReads {
    /// Publish only the areas that have read.
    deferred: std::cell::Cell<bool>,
    /// The window has drawn a frame, so reading may begin.
    presented: std::cell::Cell<bool>,
    /// A wake for the next read is on its way.
    woken: std::cell::Cell<bool>,
    /// The index in [`crate::area::Registry::live_areas`] reading continues from.
    next: std::cell::Cell<usize>,
    /// The area whose read runs on the background executor; it wakes the
    /// window when it ends, so no sync asks for another meanwhile.
    in_flight: std::cell::Cell<Option<usize>>,
}

/// What one step of the warm-up did ([`AreaReads::step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
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
pub fn defer_area_reads_in_tests(deferred: bool) {
    DEFERRED_IN_TESTS.with(|cell| cell.set(deferred));
}

impl AreaReads {
    /// The reads of a window attached through `cx`.
    pub fn of(cx: &crate::host::PaneCx) -> Self {
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
    pub fn deferred(&self) -> bool {
        self.deferred.get()
    }

    /// The window drew a frame.
    pub fn presented(&self) {
        self.presented.set(true);
    }

    /// Whether this sync reads the next area: the window has drawn and not
    /// every area has read. Taking it consumes the pending wake.
    pub fn take_due(&self) -> bool {
        self.woken.set(false);
        self.deferred.get() && self.presented.get()
    }

    /// The next area to read in `app`, with its index: the first after the
    /// last one read that has not. Each area is passed once, so reading
    /// ends after one pass even if an area's rows never read it; then every
    /// area is published.
    fn take_next(&self, app: &SettingsApp) -> Option<(usize, &'static LiveArea)> {
        let start = self.next.get();
        let live_areas = &crate::area::registry().live_areas;
        let found = live_areas
            .iter()
            .copied()
            .enumerate()
            .skip(start)
            .find(|(_, area)| !(area.is_open)(app));
        let Some((index, area)) = found else {
            self.next.set(live_areas.len());
            self.deferred.set(false);
            return None;
        };
        self.next.set(index + 1);
        Some((index, area))
    }

    /// Read on in `app`: take in the background read that has ended, or
    /// start the next area's read — on the background executor when its
    /// read blocks, else here. At most one background read runs at a time.
    pub fn step(&self, app: &SettingsApp) -> Step {
        if let Some(index) = self.in_flight.get() {
            let Some(area) = crate::area::registry().live_areas.get(index).copied() else {
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
    pub fn wake(&self) {
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
pub fn rows(app: &SettingsApp) -> Vec<ui::framework::LiveRow> {
    use ui::tilo_resource::profiling::measure;
    let mut rows = measure("settings.rows.panes", pane_rows);
    for area in crate::area::registry().live_areas.iter().copied() {
        if app.publishes_area(area) {
            rows.extend(measure(area.name, || (area.rows)(app)));
        }
    }
    rows
}

/// The live row `id`, published or not: an address an agent kept or a
/// search hit names reads the area it belongs to.
pub fn row(app: &SettingsApp, id: &str) -> Option<ui::framework::LiveRow> {
    fn find(rows: Vec<ui::framework::LiveRow>, id: &str) -> Option<ui::framework::LiveRow> {
        rows.into_iter().find(|row| row.id() == id)
    }
    // The areas that have read are asked first, so a published row never
    // reads another area.
    find(rows(app), id).or_else(|| {
        crate::area::registry()
            .live_areas
            .iter()
            .copied()
            .filter(|area| !app.publishes_area(area))
            .find_map(|area| find((area.rows)(app), id))
    })
}

/// The effects that differ from their declaration on this device now.
pub fn statuses(app: &SettingsApp) -> Vec<ui::framework::EffectStatus> {
    app.areas()
        .iter()
        .flat_map(crate::area::PaneArea::statuses)
        .collect()
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
pub fn show_pane(
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
    // The pane rows change only with the language, and the host asks for
    // the rows on every frame it publishes: they are built once per
    // language, not looked up again each time.
    thread_local! {
        static BUILT: std::cell::RefCell<Option<(u64, Vec<ui::framework::LiveRow>)>> =
            const { std::cell::RefCell::new(None) };
    }
    let language = ui::i18n::I18nManager::revision();
    BUILT.with(|built| {
        let mut built = built.borrow_mut();
        if let Some((revision, rows)) = built.as_ref()
            && *revision == language
        {
            return rows.clone();
        }
        let rows: Vec<_> = Category::ALL
            .iter()
            .map(|category| {
                ui::framework::LiveRow::new(
                    pane_row_id(*category),
                    "settings.section",
                    category.label(),
                )
                .operation(open_pane(*category).bound())
            })
            .collect();
        *built = Some((language, rows.clone()));
        rows
    })
}

/// How a search hit or an agent's `open` names a setting:
/// `settings.setting:<id>`, an id of [`settings_store::index::SETTINGS`].
pub const SETTING_ANCHOR: &str = "settings.setting:";

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
    /// Open the pane a setting is on, scrolled to it, with the keyboard on
    /// its control.
    #[effect(view, label = "Show a setting")]
    ShowSetting {
        #[param("The setting: its id, such as `keyboard.layout` or `appearance.style`")]
        setting: String,
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
        NavigationEffect::ShowSetting { setting } => {
            app.request_setting(&setting)?;
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
