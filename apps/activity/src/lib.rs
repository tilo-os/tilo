//! Activity: what changed in a workspace, and who changed it.

pub const APP_ID: &str = wire::APP_ID;
pub const APP_TITLE: &str = "Activity";

mod detail;
mod effects;
mod filter;
mod history;
pub mod locales;
mod replay;
#[cfg(feature = "samples")]
mod sample;
mod whats_new;

use ui::time::{Duration, SystemTime};

use ui::framework::RecordEnv;
use ui::prelude::*;
use ui::tilo_actions::remote::InvokeArgument;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::widget::{NavItem, NavSection};

pub use effects::ActivityEffect;
pub use filter::Filter;

/// What the window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Place {
    #[default]
    WhatsNew,
    History,
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
    pub(crate) place: LocalState<Option<String>>,
    pub(crate) pane: LocalState<ui::widget::NavPane>,
    pub(crate) everywhere: LocalState<bool>,
    pub(crate) selection: LocalState<Option<wire::Range>>,
    pub(crate) inspector: LocalState<bool>,
    pub(crate) reveal: LocalState<bool>,
    pub(crate) compare: LocalState<bool>,
    pub(crate) limit: LocalState<usize>,
    pub(crate) replay_at: LocalState<f64>,
    pub(crate) replay_playing: LocalState<bool>,
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

/// The session's door to the workspace's history, for the parts of the view that ask it for
/// a change.
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
    crate::locales::CATALOG.t(key)
}

pub(crate) fn trf(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::locales::CATALOG.t_args(key, args)
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
    ui::format_relative_time(at, ui::time::wall())
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
    const TITLE_KEY: &'static str = "activity-app-title";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1240, 780)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    #[cfg(feature = "samples")]
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
        #[cfg(feature = "samples")]
        {
            let stage = nav.clone();
            cx.on_stage(move |scenario| sample::stage(scenario, &stage));
        }

        let provider = Provider::of(cx);
        {
            let (nav, provider) = (nav.clone(), provider.clone());
            cx.effects(move |effect: ActivityEffect, _| {
                effects::answer(effect, &nav, &provider);
                Ok(ui::framework::Effected::done())
            });
        }
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

/// The window as it shows `nav`'s place: the sidebar of places, the place with its commands
/// in the header, and History's inspector.
fn window(cx: &ViewCx<Filter>, nav: &Nav) -> Element {
    let chosen = nav.clone();
    let sidebar = ui::widget::NavList::new(vec![NavSection::untitled(
        "places",
        Place::ALL
            .into_iter()
            .map(|place| NavItem::new(place.key(), tr(place.label())).icon(place.icon()))
            .collect(),
    )])
    .with_accessible_name(tr("activity-places"))
    .with_semantic_key("activity.places")
    .bind_selection(&nav.place)
    .on_view("activity.places", move |_| {
        chosen.pane.set(ui::widget::NavPane::Content);
    });

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

ui::reach_tests!(App);
