//! Everything Settings does, as one typed effect vocabulary.

use ui::framework::{EffectAnswer, EffectCx};
use ui::tilo_resource::{self, ActionRef, Effect, EffectDecl};

use crate::categories::Category;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

pub const OPERATION: &str = "settings.effect";

/// One area's effect vocabulary as Settings answers it: its declarations,
/// how one of its effects is read back, and the area's `answer`. The union
/// ([`SettingsEffect`]) reaches every area through these alone, so the code
/// that dispatches effects names no area: an area brings its family
/// ([`effect_family!`]) and [`crate::area::registry`] lists them.
pub struct EffectFamily {
    pub declarations: fn() -> Vec<EffectDecl>,
    pub rows: fn() -> Vec<tilo_resource::effect::EffectRow>,
    pub decode: DecodeEffect,
    pub answer: AnswerEffect,
}

pub type DecodeEffect = fn(
    &str,
    &serde_json::Map<String, serde_json::Value>,
) -> Result<Box<dyn AreaEffect>, tilo_resource::EffectDecodeError>;

pub type AnswerEffect =
    fn(&SettingsApp, Box<dyn std::any::Any>, &mut EffectCx<Settings>) -> EffectAnswer;

pub trait AreaVocabulary: Effect + Clone + PartialEq + std::fmt::Debug + 'static {
    const FAMILY: &'static EffectFamily;
}

pub trait AreaEffect: std::fmt::Debug {
    fn id(&self) -> &'static str;
    fn arguments(&self) -> serde_json::Map<String, serde_json::Value>;
    fn bound(&self) -> tilo_actions::ActionSpec;
    fn bound_asking(&self, asking: &[&str]) -> tilo_actions::ActionSpec;
    fn bound_asking_params(
        &self,
        params: Vec<tilo_actions::ActionParam>,
    ) -> tilo_actions::ActionSpec;
    fn clone_effect(&self) -> Box<dyn AreaEffect>;
    fn same_as(&self, other: &dyn AreaEffect) -> bool;
    fn as_any(&self) -> &dyn std::any::Any;
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any>;
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

#[macro_export]
macro_rules! effect_family {
    ($ty:ty => $answer:path) => {
        impl $crate::effects::AreaVocabulary for $ty {
            const FAMILY: &'static $crate::effects::EffectFamily = &$crate::effects::EffectFamily {
                declarations: <$ty as ui::tilo_resource::Effect>::declarations,
                rows: <$ty as ui::tilo_resource::Effect>::rows,
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

    fn rows() -> Vec<ui::tilo_resource::effect::EffectRow> {
        crate::area::registry()
            .families
            .iter()
            .flat_map(|family| (family.rows)())
            .collect()
    }

    fn id(&self) -> &'static str {
        self.0.id()
    }

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
    ) -> Result<Self, tilo_resource::EffectDecodeError> {
        for family in &crate::area::registry().families {
            if (family.declarations)().iter().any(|decl| decl.id == id) {
                return (family.decode)(id, arguments).map(Self);
            }
        }
        Err(tilo_resource::EffectDecodeError::Unknown(id.to_owned()))
    }
}

pub fn answer(
    app: &SettingsApp,
    effect: SettingsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    if let Some(waiting) = after_area_read(app, &effect, cx) {
        return Ok(waiting);
    }
    let family = effect.0.family();
    (family.answer)(app, effect.0.into_any(), cx)
}

/// When `effect` is answered on the model of an area whose read of the
/// device has not ended (an agent or the palette reached it before the
/// window's warm-up did), start that read if need be and answer once it
/// has ended: building the model now would wait for the device on the UI
/// thread. Only an effect declared to answer later can; one declared to
/// answer at once is answered on the model as it is read now.
fn after_area_read(
    app: &SettingsApp,
    effect: &SettingsEffect,
    cx: &mut EffectCx<Settings>,
) -> Option<ui::framework::Effected> {
    let id = effect.id();
    let read = crate::area::registry()
        .live_areas
        .iter()
        .filter(|area| !(area.is_open)(app))
        .filter_map(|area| area.background.as_ref())
        .find(|read| {
            (read.answers)().iter().any(|decl| {
                decl.id == id && matches!(decl.answers, tilo_resource::Answers::Later { .. })
            })
        })?;
    (read.start)(app);
    let wait = (read.wait)(app)?;
    let effect = effect.clone();
    Some(app.job(cx, wait, move |app, (), cx| answer(app, effect, cx)))
}

effect_family!(NavigationEffect => answer_navigation);

pub const PART: crate::area::Part = crate::area::Part {
    areas: &[],
    families: &[(10, NavigationEffect::FAMILY)],
    live_areas: &[],
    settings: &[],
};

/// One area of Settings whose live rows show what it reads of the device:
/// its printers, accounts, audio graph. Reading is what costs — on a device
/// it runs commands and D-Bus calls — so an area reads when first asked,
/// and the published rows of a live window name an area only once it has
/// read ([`SettingsApp::publishes_area`]).
pub struct LiveArea {
    pub name: &'static str,
    pub pane: Category,
    pub is_open: fn(&SettingsApp) -> bool,
    pub rows: fn(&SettingsApp) -> Vec<ui::framework::LiveRow>,
    pub background: Option<BackgroundRead>,
}

pub struct BackgroundRead {
    pub start: fn(&SettingsApp) -> bool,
    pub finished: fn(&SettingsApp) -> bool,
    pub wait: fn(&SettingsApp) -> Option<crate::device_read::Wait>,
    pub answers: fn() -> Vec<EffectDecl>,
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
    deferred: std::cell::Cell<bool>,
    presented: std::cell::Cell<bool>,
    woken: std::cell::Cell<bool>,
    next: std::cell::Cell<usize>,
    in_flight: std::cell::Cell<Option<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Read,
    Waiting,
    Done,
}

#[cfg(test)]
thread_local! {
    static DEFERRED_IN_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn defer_area_reads_in_tests(deferred: bool) {
    DEFERRED_IN_TESTS.with(|cell| cell.set(deferred));
}

impl AreaReads {
    pub fn of(devices: bool) -> Self {
        #[cfg(test)]
        let deferred = devices || DEFERRED_IN_TESTS.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let deferred = devices;
        Self {
            deferred: std::cell::Cell::new(deferred),
            ..Self::default()
        }
    }

    pub fn deferred(&self) -> bool {
        self.deferred.get()
    }

    pub fn presented(&self) {
        self.presented.set(true);
    }

    pub fn take_due(&self) -> bool {
        self.woken.set(false);
        self.deferred.get() && self.presented.get()
    }

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

/// The live row `id` among the areas read: an address an agent kept or a
/// search hit names. A row of an area not read yet is not known until its
/// read lands ([`row_pending`]); asking never reads it here.
pub fn row(app: &SettingsApp, id: &str) -> Option<ui::framework::LiveRow> {
    rows(app).into_iter().find(|row| row.id() == id)
}

pub struct RowsMemo<K> {
    kept: std::cell::RefCell<Option<(K, Vec<ui::framework::LiveRow>)>>,
}

impl<K> Default for RowsMemo<K> {
    fn default() -> Self {
        Self {
            kept: std::cell::RefCell::new(None),
        }
    }
}

impl<K: PartialEq> RowsMemo<K> {
    pub fn rows(
        &self,
        key: K,
        make: impl FnOnce() -> Vec<ui::framework::LiveRow>,
    ) -> Vec<ui::framework::LiveRow> {
        let mut kept = self.kept.borrow_mut();
        if let Some((_, rows)) = kept.as_ref().filter(|(was, _)| *was == key) {
            return rows.clone();
        }
        let rows = make();
        *kept = Some((key, rows.clone()));
        rows
    }
}

/// Whether row `id`, which no area read so far shows, may be one of an area
/// not read yet: that area's read is started, and the agent addressing the
/// row is answered once it lands.
pub fn row_pending(app: &SettingsApp, id: &str) -> bool {
    let _ = id;
    let mut pending = false;
    for area in crate::area::registry().live_areas.iter().copied() {
        if app.publishes_area(area) {
            continue;
        }
        pending = true;
        if let Some(read) = &area.background {
            (read.start)(app);
        }
    }
    if pending {
        app.read_areas_now();
    }
    pending
}

pub fn statuses(app: &SettingsApp) -> Vec<ui::framework::EffectStatus> {
    app.areas()
        .iter()
        .flat_map(crate::area::PaneArea::statuses)
        .collect()
}

#[must_use]
pub const fn effect<E, A: serde::Serialize>(
    effect: ActionRef<E, A>,
) -> ActionRef<SettingsEffect, A> {
    ActionRef::new(effect.id, effect.params, *effect.kind())
}

#[must_use]
pub fn pane_row_id(category: Category) -> String {
    format!("settings.section:{}", category.slug())
}

#[must_use]
pub fn open_pane(category: Category) -> SettingsEffect {
    NavigationEffect::OpenPane {
        pane: category.slug().to_owned(),
    }
    .into()
}

#[cfg(test)]
pub fn show_pane(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    category: Category,
) {
    app.effect_on(&pane_row_id(category), &open_pane(category))
        .expect("the pane opens");
}

#[cfg(test)]
pub fn page_of(root: &dyn ui::widget::Widget) -> &dyn ui::widget::Widget {
    fn scrollers<'a>(
        widget: &'a dyn ui::widget::Widget,
        out: &mut Vec<&'a dyn ui::widget::Widget>,
    ) {
        if widget.name() == "Scrollable" {
            out.push(widget);
            return;
        }
        for child in widget.children() {
            scrollers(child, out);
        }
    }
    let mut found = Vec::new();
    scrollers(root, &mut found);
    found
        .into_iter()
        .max_by(|a, b| a.get_rect().x0.total_cmp(&b.get_rect().x0))
        .expect("the window's content scroller")
}

#[must_use]
pub fn pane_of_row(row: &str) -> Option<Category> {
    Category::from_slug(row.strip_prefix("settings.section:")?)
}

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

pub const SETTING_ANCHOR: &str = "settings.setting:";

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum NavigationEffect {
    /// Open a pane of Settings.
    #[effect(view)]
    OpenPane {
        #[param("The pane: its id, such as `appearance` or `network`")]
        pane: String,
    },
    /// Open the pane a setting is on, scrolled to it, with the keyboard on
    /// its control.
    #[effect(view)]
    ShowSetting {
        #[param("The setting: its id, such as `keyboard.layout` or `appearance.style`")]
        setting: String,
    },
}

fn answer_navigation(
    app: &SettingsApp,
    effect: NavigationEffect,
    _cx: &mut EffectCx<Settings>,
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
            .with_widget::<crate::window::Inspected, _>(|app| {
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
            .with_widget::<crate::window::Inspected, _>(|app| super::rows(app))
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
                tilo_actions::door::survey(&[tilo_actions::ResourceSpec::new(
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

        fn rows() -> Vec<ui::tilo_resource::effect::EffectRow> {
            SettingsEffect::rows()
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
        ) -> Result<Self, tilo_resource::EffectDecodeError> {
            SettingsEffect::decode(id, arguments).map(Self)
        }
    }

    #[test]
    fn mounted_live_row_bindings_preserve_flattened_metadata_and_payloads() {
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        let resource_rows = app
            .with_widget::<crate::window::Inspected, _>(|app| rows(app))
            .unwrap();
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
                    tilo_actions::door::survey(&[tilo_actions::ResourceSpec::new(
                        "comparison",
                        "comparison",
                        "comparison"
                    )
                    .with_action(direct.clone())]),
                    tilo_actions::door::survey(&[tilo_actions::ResourceSpec::new(
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
            SettingsEffect::decode(open.id(), &open.arguments()).ok(),
            Some(open)
        );
        assert!(SettingsEffect::decode("no-such-effect", &serde_json::Map::new()).is_err());
    }
}

#[cfg(test)]
#[path = "area_reads_tests.rs"]
mod area_reads_tests;
