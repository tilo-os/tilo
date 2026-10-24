//! Activity: what changed in a workspace, and who changed it.
//!
//! Three places, one beside the other in the sidebar:
//!
//! * **What's new** ([`whats_new`]): the work sessions since this device
//!   last looked, where you left off, the on-device summary and the morning
//!   sources you chose. Leaving it, or marking it viewed, counts it seen.
//! * **History** ([`history`]): every change, grouped by work group and
//!   day, narrowed by period, person and lens ([`Filter`]); a selected run
//!   of changes shows in the inspector ([`detail`]) with what it touched and
//!   what can be done with it: compare, undo, restore, name, split, merge.
//! * **Replay** ([`replay`]): the workspace's layout at each recent change,
//!   stepped or played.
//!
//! The history is the workspace's, not the app's: the session reads it and
//! serves it under [`wire::PROVIDER`], live, to whoever may read it. The app
//! follows those reads ([`ui::framework::RecordEnv::query`]) and asks for
//! changes through the same door ([`Provider`]). Its own record holds only
//! which changes History shows.

/// The application id.
pub const APP_ID: &str = wire::APP_ID;
/// The window and launcher title.
pub const APP_TITLE: &str = "Activity";

mod detail;
mod filter;
mod history;
pub mod locales;
mod replay;
mod sample;
mod whats_new;

use std::time::{Duration, SystemTime};

use ui::framework::RecordEnv;
use ui::i18n::I18nManager;
use ui::prelude::*;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{NavItem, NavSection};

pub use filter::Filter;

/// What the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// What changed since this device last looked.
    #[default]
    WhatsNew,
    /// Every change.
    History,
    /// The layout at each change.
    Replay,
}

impl Place {
    const ALL: [Self; 3] = [Self::WhatsNew, Self::History, Self::Replay];

    const fn key(self) -> &'static str {
        match self {
            Self::WhatsNew => "whats-new",
            Self::History => "history",
            Self::Replay => "replay",
        }
    }

    fn of(key: Option<&str>) -> Self {
        Self::ALL
            .into_iter()
            .find(|place| Some(place.key()) == key)
            .unwrap_or_default()
    }

    const fn label(self) -> &'static str {
        match self {
            Self::WhatsNew => "activity-place-whats-new",
            Self::History => "activity-place-history",
            Self::Replay => "activity-place-replay",
        }
    }

    const fn icon(self) -> ui::icons::Icon {
        match self {
            Self::WhatsNew => ui::icons::Icon::Inbox,
            Self::History => ui::icons::Icon::History,
            Self::Replay => ui::icons::Icon::Film,
        }
    }
}

/// The window's view state, shared by its parts: never stored or synced.
#[derive(Clone)]
pub(crate) struct Nav {
    /// The key of the place shown, as the sidebar selects it.
    pub(crate) place: LocalState<Option<String>>,
    pub(crate) pane: LocalState<ui::widget::NavPane>,
    /// History reads every workspace on this device, not just this one.
    pub(crate) everywhere: LocalState<bool>,
    /// The run of changes History selected.
    pub(crate) selection: LocalState<Option<wire::Range>>,
    /// History's inspector shows: docked beside it in a wide window, as a
    /// sheet over it in a narrow one.
    pub(crate) inspector: LocalState<bool>,
    /// History is to open with its inspector showing what was chosen
    /// elsewhere: a work session in What's new, a focus asked for outside.
    pub(crate) reveal: LocalState<bool>,
    /// The selection's changes are compared before and after.
    pub(crate) compare: LocalState<bool>,
    /// How many changes History reads; none for the first page.
    pub(crate) limit: LocalState<usize>,
    /// The change Replay shows, as the slider's position.
    pub(crate) replay_at: LocalState<f64>,
    pub(crate) replay_playing: LocalState<bool>,
    /// The last focus asked for from outside the app that was shown.
    pub(crate) shown_focus: LocalState<u64>,
}

impl Nav {
    pub(crate) fn place(&self) -> Place {
        Place::of(self.place.get().as_deref())
    }

    pub(crate) fn show(&self, place: Place) {
        self.place.set(Some(place.key().to_owned()));
        self.pane.set(ui::widget::NavPane::Content);
    }

    /// Select `range` and show what it is; with none, select nothing.
    pub(crate) fn select(&self, range: Option<wire::Range>) {
        self.compare.set(false);
        if range.is_some() {
            self.inspector.set(true);
        }
        self.selection.set(range);
    }

    /// Show `range` selected in History.
    pub(crate) fn open_in_history(&self, range: wire::Range) {
        self.select(Some(range));
        self.reveal.set(true);
        self.show(Place::History);
    }
}

/// The session's door to the workspace's history, for the parts of the
/// view that ask it for a change.
#[derive(Clone)]
pub(crate) struct Provider(RecordEnv<Filter>);

impl Provider {
    pub(crate) fn of(cx: &ViewCx<Filter>) -> Self {
        Self(RecordEnv::clone(cx))
    }

    /// Ask for `action` with `value` as its argument.
    pub(crate) fn ask(&self, action: &str, value: &impl serde::Serialize) {
        self.0.invoke(wire::target(action), wire::argument(value));
    }

    /// Follow the read `action` with `value` as its argument.
    pub(crate) fn follow<T>(&self, action: &str, value: Option<&impl serde::Serialize>) -> Query<T>
    where
        T: serde::de::DeserializeOwned + Send + Sync + 'static,
    {
        self.0.query(
            wire::target(action),
            value.map_or(InvokeArgument::None, wire::argument),
        )
    }
}

pub(crate) fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}

pub(crate) fn trf(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    I18nManager::format(key, args)
}

/// When `at_ms` was, as "5 minutes ago".
pub(crate) fn ago(at_ms: i64) -> String {
    let at = u64::try_from(at_ms)
        .ok()
        .map_or(SystemTime::UNIX_EPOCH, |ms| {
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_millis(ms))
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
    ui::format_relative_time(at, SystemTime::now())
}

/// A page of the window: its content in a readable column that scrolls.
pub(crate) fn page(content: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Group);
    for part in content {
        column = column.push(part);
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(
                Container::new()
                    .readable()
                    .centered()
                    .with_inset(ui::tokens::Rhythm::Group)
                    .with_child(Box::new(column)),
            )),
    )
}

pub struct App;

impl ResourceApp for App {
    type Resource = Filter;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1240, 780)
    }

    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    fn fixture(fixture: &mut Fixture<Filter>) {
        sample::fixture(fixture);
    }

    fn view(cx: &mut ViewCx<Filter>) -> Element {
        let nav = Nav {
            place: cx.local(Some(Place::WhatsNew.key().to_owned())),
            pane: cx.local(ui::widget::NavPane::Content),
            everywhere: cx.local(false),
            selection: cx.local(None),
            inspector: cx.local(true),
            reveal: cx.local(false),
            compare: cx.local(false),
            limit: cx.local(0),
            replay_at: cx.local(0.0),
            replay_playing: cx.local(false),
            shown_focus: cx.local(0),
        };
        let stage = nav.clone();
        cx.on_stage(move |scenario| sample::stage(scenario, &stage));

        // What the person asked to be shown from outside the app (the
        // glance in the top bar, a file's own history) lands here once.
        let provider = Provider::of(cx);
        let focus = provider.follow::<Option<wire::Focus>>(wire::FOCUS, None::<&()>);
        let shown = nav;
        cx.switch(&focus.answer().state(), move |cx, answer| {
            if let Arrival::Ready(Some(focus)) = answer {
                show_focus(cx, &shown, focus);
            }
            let nav = shown.clone();
            cx.switch(&shown.place, move |cx, _| window(cx, &nav))
        })
    }
}

/// Show what `focus` asks for, unless it was shown.
fn show_focus(cx: &ViewCx<Filter>, nav: &Nav, focus: &wire::Focus) {
    if focus.serial == 0 || focus.serial <= nav.shown_focus.get() {
        return;
    }
    nav.shown_focus.set(focus.serial);
    match focus.view {
        wire::View::WhatsNew => nav.show(Place::WhatsNew),
        wire::View::Replay => nav.show(Place::Replay),
        wire::View::History => {
            let dispatcher = cx.dispatcher();
            let paths = Filter::paths();
            for edit in [
                paths.author.set(focus.author.clone().unwrap_or_default()),
                paths.lens.set(focus.lens.clone().unwrap_or_default()),
            ] {
                // A text field's edit always serializes.
                if let Ok(invoke) = Invoke::edit(&edit) {
                    dispatcher.submit(invoke);
                }
            }
            nav.everywhere.set(false);
            if let Some(entry) = focus.entry.as_deref() {
                nav.open_in_history(wire::Range::one(entry));
            } else {
                nav.select(None);
                nav.show(Place::History);
            }
        }
    }
    Provider::of(cx).ask(
        wire::FOCUSED,
        &wire::Focused {
            serial: focus.serial,
        },
    );
}

/// The window as it shows `nav`'s place: the sidebar of places, the place
/// with its commands in the header, and History's inspector.
fn window(cx: &ViewCx<Filter>, nav: &Nav) -> Element {
    let chosen = nav.clone();
    let sidebar = ui::widget::NavList::new(vec![NavSection::untitled(
        "places",
        Place::ALL
            .into_iter()
            .map(|place| NavItem::new(place.key(), tr(place.label())).icon(place.icon()))
            .collect(),
    )])
    .with_id("activity.places")
    .with_accessible_name(tr("activity-places"))
    .with_semantic_key("activity.places")
    .bind_selection(&nav.place)
    .on_select(move |_| chosen.pane.set(ui::widget::NavPane::Content));

    let place = nav.place();
    let shell = ui::widget::NavShell::new()
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("activity.shell")
        .mode(ui::widget::NavMode::Push)
        .with_pane_state(&nav.pane)
        .with_inspector_state(&nav.inspector)
        .sidebar(Box::new(sidebar))
        .sidebar_header(tr("activity-title"), Vec::new());
    Box::new(match place {
        Place::WhatsNew => {
            let (content, actions) = whats_new::view(cx, nav);
            shell
                .content(content)
                .content_header(tr(place.label()), actions)
        }
        Place::History => {
            let (content, actions) = history::view(cx, nav);
            let shell = shell
                .content(content)
                .content_header(tr(place.label()), actions)
                .inspector(detail::view(cx, nav))
                .inspector_header(tr("activity-detail-title"), Vec::new());
            if nav.reveal.get() {
                nav.reveal.set(false);
                shell.with_inspector_revealed()
            } else {
                shell
            }
        }
        Place::Replay => {
            let (content, actions) = replay::view(cx, nav);
            shell
                .content(content)
                .content_header(tr(place.label()), actions)
        }
    })
}

ui::dev_reload_entry!(App);

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::App::scenarios() {
            let mut app = TestApp::<super::App>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
