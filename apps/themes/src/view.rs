//! The studio window: your themes in the sidebar, the presets as rendered
//! previews, and an editor for one theme with its contrast findings live.
//!
//! Every control is bound to the role it shows, by its path in the
//! [`Studio`]; switching polarity, resetting and deleting are the studio's
//! verbs. Applying and exporting are effects on this machine
//! ([`crate::apply`]) and refuse a theme that misses a contrast floor.

use std::sync::Arc;

use ui::icons::Icon;
use ui::prelude::*;
use ui::theme::{Preset, ThemeDocument};
use ui::tilo_resource::Path;
use ui::widget::{
    Actions as CardActions, ButtonRole, Card, CardGrid, CardWidth, Field, Form, FormSection,
    NavAction,
};

use crate::apply::AppearanceChoice;
use crate::preview::ThemePreview;
use crate::studio::{Rung, Studio, Theme};

/// What the content pane shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Gallery,
    Theme(String),
    /// A theme started from `base` (a preset id or a theme id) that only
    /// its first edit writes, under `id`: until then the editor shows the
    /// base and the studio holds nothing, so one opened and left untouched
    /// leaves no copy behind.
    Draft {
        id: String,
        base: String,
    },
}

impl Page {
    /// The theme this page edits, if it edits one.
    fn theme(&self) -> Option<&str> {
        match self {
            Self::Gallery => None,
            Self::Theme(id) | Self::Draft { id, .. } => Some(id),
        }
    }
}

/// View state shared by the parts of the window: never stored, synced or
/// undone.
#[derive(Clone)]
struct Ui {
    page: LocalState<Page>,
    pane: LocalState<NavPane>,
    notice: LocalState<String>,
    /// The person's theme choice, followed: which preset is in use.
    appearance: LocalState<AppearanceChoice>,
    dispatcher: Dispatcher<Studio>,
    live: Live<Studio>,
}

impl Ui {
    fn open(&self, page: Page) {
        self.page.set(page);
        self.pane.set(NavPane::Content);
        self.notice.set(String::new());
    }

    /// Open a draft of a theme from `base` (a preset id or a theme id): its
    /// first edit writes it.
    fn start_from(&self, base: &str) {
        let id = format!("t{}", uuid::Uuid::new_v4().simple());
        self.open(Page::Draft {
            id,
            base: base.to_owned(),
        });
    }

    fn apply(&self, theme: String) {
        self.dispatcher
            .effect(&crate::effects::ThemesEffect::ApplyTheme { theme });
    }

    fn export(&self, theme: String) {
        self.dispatcher
            .effect(&crate::effects::ThemesEffect::Export { theme });
    }
}

/// The theme the editor writes: one in the studio, or a draft whose first
/// edit writes it.
#[derive(Clone)]
struct Editing {
    id: Id,
    /// A draft's base and the theme it shows until it is written.
    draft: Option<Arc<(String, Theme)>>,
    /// Whether the studio holds the theme.
    written: LocalState<bool>,
}

impl Editing {
    fn new(cx: &ViewCx<Studio>, id: &str, base: Option<&str>) -> Self {
        let id = Id::from_raw(id);
        let draft = base.and_then(|base| {
            cx.read(|studio| studio.and_then(|studio| studio.draft(base).ok()))
                .map(|theme| Arc::new((base.to_owned(), theme)))
        });
        let written = {
            let id = id.clone();
            cx.value(move |studio: &Studio| studio.themes.contains(&id))
        };
        Self { id, draft, written }
    }

    /// `read` of the theme shown: the studio's, or the unwritten draft.
    fn read<V>(&self, studio: &Studio, read: impl FnOnce(&Theme) -> V) -> Option<V> {
        studio
            .themes
            .get(&self.id)
            .or_else(|| self.draft.as_deref().map(|(_, theme)| theme))
            .map(read)
    }

    /// A value following `read` of the theme shown.
    fn value<V>(&self, cx: &ViewCx<Studio>, read: impl Fn(&Theme) -> V + 'static) -> LocalState<V>
    where
        V: PartialEq + Default + Clone + 'static,
    {
        let editing = self.clone();
        cx.value(move |studio: &Studio| editing.read(studio, &read).unwrap_or_default())
    }

    /// The path of the theme's role at `role` in the studio.
    fn path<T>(&self, role: &Path<Theme, T>) -> Path<Studio, T> {
        role.under(&Studio::paths().themes.item_path(&self.id))
    }

    /// `change`, writing an unwritten draft first: one action, one undo
    /// step, so the first edit of a draft is the one that creates it.
    fn invoke(&self, change: Invoke<Studio>) -> Invoke<Studio> {
        let create = self
            .draft
            .as_deref()
            .filter(|_| !self.written.get())
            .map(|(base, _)| Studio::NEW_THEME.with((self.id.as_str().to_owned(), base.clone())));
        Invoke::group(create.into_iter().chain([change]))
    }

    /// `control`, invoking what `change` makes of each committed input.
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        cx: &ViewCx<Studio>,
        control: C,
        change: impl Fn(I) -> Invoke<Studio> + Send + Sync + 'static,
    ) -> C {
        cx.actions().bind(
            control,
            Writes {
                editing: self.clone(),
                change,
            },
        )
    }

    /// `control`, showing the role at `role` and setting it to what `value`
    /// makes of each committed input.
    fn set<T, I, C>(
        &self,
        cx: &ViewCx<Studio>,
        control: C,
        role: &Path<Theme, T>,
        value: impl Fn(I) -> T + Send + Sync + 'static,
    ) -> C
    where
        T: ui::tilo_resource::Settable + 'static,
        I: 'static,
        C: ActionControl<I>,
    {
        let path = self.path(role);
        self.bind(cx, control, move |input| {
            Invoke::from(path.set(value(input)))
        })
    }

    /// What applying or exporting refers to: the written theme, or the
    /// base an untouched draft shows.
    fn reference(&self) -> String {
        match self.draft.as_deref() {
            Some((base, _)) if !self.written.get() => base.clone(),
            _ => self.id.as_str().to_owned(),
        }
    }
}

/// A control's binding through [`Editing::invoke`]: always a group, which
/// is the change alone once the theme is written.
struct Writes<F> {
    editing: Editing,
    change: F,
}

impl<I, F> ui::tilo_resource::Binds<Studio, I> for Writes<F>
where
    F: Fn(I) -> Invoke<Studio> + Send + Sync + 'static,
{
    fn action_id(&self) -> &'static str {
        ui::tilo_resource::GROUP
    }

    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let invoke = self.editing.invoke((self.change)(input));
        match invoke.refusal() {
            Some(refusal) => Err(refusal.to_owned()),
            None => Ok(invoke.arguments),
        }
    }
}

/// The whole window.
pub(crate) fn root(cx: &ViewCx<Studio>) -> Element {
    let ui = Ui {
        page: LocalState::new(Page::Gallery),
        pane: LocalState::new(NavPane::Content),
        notice: LocalState::new(String::new()),
        appearance: cx.settings::<AppearanceChoice>().state(),
        dispatcher: cx.dispatcher(),
        live: cx.live().clone(),
    };
    crate::effects::install(cx, &ui.notice);
    {
        let ui = ui.clone();
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario
                && scenario != crate::GALLERY_SCENARIO
            {
                ui.open(Page::Theme(crate::SCENARIO_THEME.to_owned()));
            }
        });
    }
    let nav = sidebar(&ui);
    let content = {
        let ui = ui.clone();
        cx.switch(&ui.page.clone(), move |cx, page| match page {
            Page::Gallery => gallery(&ui),
            Page::Theme(id) => editor(cx, &ui, &Editing::new(cx, id, None)),
            Page::Draft { id, base } => editor(cx, &ui, &Editing::new(cx, id, Some(base))),
        })
    };
    Box::new(
        NavShell::new()
            // The gallery's tiles and the editor's form need a page's width
            // beside the list; narrower, the window shows one at a time.
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("themes-shell")
            .with_pane_state(&ui.pane)
            .sidebar(nav)
            .content(content),
    )
}

fn notice(ui: &Ui) -> Text {
    Text::from_state(&ui.notice, Clone::clone)
        .with_id("themes-notice")
        .with_role(TextRole::Caption)
        .with_live(ui::a11y::Politeness::Polite)
}

/// The navigation list's id, for tests and inspection.
pub const NAV_ID: &str = "themes-nav";
/// The destination that shows the presets.
const PRESETS: &str = "presets";
/// The prefix of a written theme's destination.
const THEME: &str = "theme:";
/// The section of written themes, and its "New theme" action.
const YOURS: &str = "yours";
const NEW_THEME: &str = "new-theme";

fn t(key: &str) -> String {
    crate::locales::CATALOG.t(key)
}

fn destination(page: &Page) -> String {
    page.theme()
        .map_or_else(|| PRESETS.to_owned(), |id| format!("{THEME}{id}"))
}

/// The destinations: the presets, then the themes you wrote under their
/// own header, whose trailing action starts a new one.
fn sections(studio: &Studio) -> Vec<NavSection> {
    let presets = NavItem::new(PRESETS, t("themes-presets")).icon(Icon::LayoutGrid);
    let yours = studio
        .themes
        .iter()
        .map(|(id, theme)| {
            let name = if theme.name.trim().is_empty() {
                t("themes-untitled")
            } else {
                theme.name.clone()
            };
            NavItem::new(format!("{THEME}{}", id.as_str()), name).icon(Icon::Palette)
        })
        .collect();
    vec![
        NavSection::untitled("gallery", vec![presets]),
        NavSection::new(YOURS, t("themes-yours"), yours)
            .trailing_action(NavAction::new(NEW_THEME, Icon::Plus, t("themes-new")))
            .empty_caption(t("themes-none")),
    ]
}

/// The sidebar: the toolkit's navigation list, kept in step with the
/// studio and the open page, so its rows, selection, keyboard and narrow
/// layout are every other Tilo sidebar's.
fn sidebar(ui: &Ui) -> Element {
    let source = {
        let live = ui.live.clone();
        let page = ui.page.clone();
        move || Some((live.with(sections), Some(destination(&page.get()))))
    };
    let select = {
        let ui = ui.clone();
        move |id: String| {
            if id == PRESETS {
                ui.open(Page::Gallery);
            } else if let Some(theme) = id.strip_prefix(THEME) {
                ui.open(Page::Theme(theme.to_owned()));
            }
        }
    };
    let new_theme = {
        let ui = ui.clone();
        move |section: &str, action: &str| {
            if section == YOURS && action == NEW_THEME {
                ui.start_from("dark");
            }
        }
    };
    let delete = {
        let ui = ui.clone();
        move |id: String| {
            let Some(theme) = id.strip_prefix(THEME) else {
                return;
            };
            ui.dispatcher
                .submit(Studio::DELETE.with(Id::from_raw(theme.to_owned())));
            if ui.page.get().theme() == Some(theme) {
                ui.open(Page::Gallery);
            }
        }
    };
    NavList::new(Vec::new())
        .with_id(NAV_ID)
        .with_accessible_name(crate::APP_TITLE)
        .follow(source)
        .on_select(select)
        .on_section_action(new_theme)
        .on_delete(delete)
        .into_widget()
}

fn gallery(ui: &Ui) -> Element {
    // One tile per preset, as many columns as fit tiles of the card grid's
    // wide minimum; the tiles stretch to fill each row and resize with it.
    let tiles = Preset::ALL
        .into_iter()
        .map(|preset| preset_tile(ui, preset))
        .collect();
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(ui::column![
                Text::new("themes-presets")
                    .with_role(TextRole::Title)
                    .with_heading_level(1),
                notice(ui),
                CardGrid::new(tiles).with_card_width(CardWidth::Wide),
            ])),
    )
}

/// One preset: its preview leading a card, its name and its actions in one
/// row under it. The preset the system shows says "In use" in place of
/// "Use".
fn preset_tile(ui: &Ui, preset: Preset) -> Element {
    let document = preset.document();
    let actions = {
        let ui = ui.clone();
        ModelView::from_state_by(
            &ui.appearance.clone(),
            move |choice: &AppearanceChoice| choice.shows(preset.id(), document.is_dark()),
            move |choice: &AppearanceChoice| -> Element {
                let in_use = choice.shows(preset.id(), preset.document().is_dark());
                let use_it = Button::new(
                    &format!("use-{}", preset.id()),
                    if in_use {
                        "themes-in-use"
                    } else {
                        "themes-use"
                    },
                )
                .role(ButtonRole::Primary)
                .with_enabled(!in_use);
                let use_it = {
                    let ui = ui.clone();
                    use_it.on_own_device_effect(move || ui.apply(preset.id().to_owned()))
                };
                let customize = {
                    let ui = ui.clone();
                    Button::new(&format!("customize-{}", preset.id()), "themes-customize")
                        .on_view(move || ui.start_from(preset.id()))
                };
                Box::new(CardActions::new(use_it).secondary(customize))
            },
        )
    };
    let card = Card::with_media(Box::new(
        ThemePreview::tile(&preset.document()).with_id(format!("preview-{}", preset.id())),
    ))
    .with_title_action(preset.name(), Box::new(actions));
    Box::new(InUse {
        card,
        preset,
        appearance: ui.appearance.clone(),
    })
}

/// A preset's card, selected while the system shows its preset: the
/// design system's selected edge marks the theme in use across the grid.
struct InUse {
    card: Card,
    preset: Preset,
    appearance: LocalState<AppearanceChoice>,
}

impl InUse {
    /// Select the card while its preset is in use; whether that changed.
    fn follow(&mut self) -> bool {
        let in_use = self
            .appearance
            .with(|choice| choice.shows(self.preset.id(), self.preset.document().is_dark()));
        let changed = self.card.get_selected() != in_use;
        self.card.set_selected(in_use);
        changed
    }
}

impl ui::widget::Decorator for InUse {
    type Inner = Card;

    fn inner(&self) -> &Card {
        &self.card
    }

    fn inner_mut(&mut self) -> &mut Card {
        &mut self.card
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.follow();
        Widget::sync(&mut self.card, state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let changed = self.follow();
        Widget::update(&mut self.card, state, path) || changed
    }
}

/// A slider over `band` for the numeric role at `role`.
fn slider(
    cx: &ViewCx<Studio>,
    editing: &Editing,
    band: (f64, f64),
    role: &Path<Theme, f64>,
) -> Slider {
    let (lo, hi) = band;
    let read = role.clone();
    let state = editing.value(cx, move |theme| {
        read.get(theme).copied().unwrap_or_default()
    });
    editing
        .set(
            cx,
            Slider::from_state(&state).range(lo, hi),
            role,
            std::convert::identity,
        )
        .with_id(&format!("{}-{}", band_id(lo, hi), editing.id.as_str()))
}

fn band_id(lo: f64, hi: f64) -> String {
    format!("slider-{lo}-{hi}")
}

/// A slider over `band` for an optional hue at `role`: moving it gives the
/// theme that hue.
fn hue_slider(
    cx: &ViewCx<Studio>,
    editing: &Editing,
    band: (f64, f64),
    role: &Path<Theme, Option<f64>>,
    absent: f64,
) -> Slider {
    let (lo, hi) = band;
    let read = role.clone();
    let state = editing.value(cx, move |theme| {
        read.get(theme).copied().flatten().unwrap_or(absent)
    });
    editing
        .set(cx, Slider::from_state(&state).range(lo, hi), role, Some)
        .with_id(&format!("{}-{}", band_id(lo, hi), editing.id.as_str()))
}

fn rung_slider(cx: &ViewCx<Studio>, editing: &Editing, rung: Rung) -> Slider {
    slider(cx, editing, (0.0, 1.0), &rung.path(&Theme::paths()))
        .with_id(&format!("lightness-{}", rung.id()))
}

/// A text field for the text at `role`; its committed edits replace it.
fn text(cx: &ViewCx<Studio>, editing: &Editing, role: &Path<Theme, String>) -> TextInput {
    let read = role.clone();
    let state = editing.value(cx, move |theme| {
        read.get(theme).cloned().unwrap_or_default()
    });
    let path = editing.path(role);
    editing.bind(cx, TextInput::from_state(&state), move |text: String| {
        Invoke::from(path.replace(text))
    })
}

fn rung_label(rung: Rung) -> &'static str {
    match rung {
        Rung::Base => "themes-rung-base",
        Rung::Content => "themes-rung-content",
        Rung::Sidebar => "themes-rung-sidebar",
        Rung::Inspector => "themes-rung-inspector",
        Rung::Raised => "themes-rung-raised",
        Rung::Overlay => "themes-rung-overlay",
        Rung::Primary => "themes-rung-primary",
        Rung::Secondary => "themes-rung-secondary",
        Rung::Tertiary => "themes-rung-tertiary",
    }
}

/// A choice between two values of the role at `role`: the first option is
/// `on`, the second `off`.
fn choice<T>(
    cx: &ViewCx<Studio>,
    editing: &Editing,
    options: [&'static str; 2],
    role: &Path<Theme, T>,
    [on, off]: [T; 2],
) -> Dropdown
where
    T: ui::tilo_resource::Settable + PartialEq + Clone + Send + Sync + 'static,
{
    let [first, second] = options;
    let read = role.clone();
    let shown = on.clone();
    let state = editing.value(cx, move |theme| {
        String::from(if read.get(theme) == Some(&shown) {
            first
        } else {
            second
        })
    });
    editing.set(
        cx,
        Dropdown::from_state(vec![first.to_owned(), second.to_owned()], &state),
        role,
        move |picked: String| {
            if picked == first {
                on.clone()
            } else {
                off.clone()
            }
        },
    )
}

/// Dark or light: switching flips the ladder, so it is the studio's verb.
fn polarity(cx: &ViewCx<Studio>, editing: &Editing) -> Dropdown {
    let state = editing.value(cx, |theme| {
        String::from(if theme.dark { "Dark" } else { "Light" })
    });
    let id = editing.id.clone();
    editing.bind(
        cx,
        Dropdown::from_state(vec!["Dark".to_owned(), "Light".to_owned()], &state),
        move |picked: String| Studio::SWITCH_POLARITY.with((id.clone(), picked == "Dark")),
    )
}

/// The editor's commands: apply and export the theme while it is legible,
/// start another from it, reset it and delete it.
fn commands(cx: &ViewCx<Studio>, ui: &Ui, editing: &Editing) -> impl Widget + use<> {
    let legible = {
        let raw = editing.id.as_str().to_owned();
        editing.value(cx, move |theme| theme.findings(&raw).is_empty())
    };
    let ui = ui.clone();
    let editing = editing.clone();
    ModelView::from_state(&legible, move |legible: &bool| -> Element {
        let apply = {
            let ui = ui.clone();
            let editing = editing.clone();
            Button::new("theme-apply", "themes-apply")
                .role(ButtonRole::Primary)
                .with_enabled(*legible)
                .on_own_device_effect(move || ui.apply(editing.reference()))
        };
        let export = {
            let ui = ui.clone();
            let editing = editing.clone();
            Button::new("theme-export", "themes-export")
                .with_enabled(*legible)
                .on_own_device_effect(move || ui.export(editing.reference()))
        };
        let duplicate = {
            let ui = ui.clone();
            let editing = editing.clone();
            Button::new("theme-duplicate", "themes-duplicate")
                .role(ButtonRole::Plain)
                .on_view(move || ui.start_from(&editing.reference()))
        };
        let reset = {
            let ui = ui.clone();
            let editing = editing.clone();
            Button::new("theme-reset", "themes-reset-button")
                .role(ButtonRole::Plain)
                .on_view(move || {
                    ui.dispatcher
                        .submit(editing.invoke(Studio::RESET.with(editing.id.clone())));
                })
        };
        let delete = {
            let ui = ui.clone();
            let editing = editing.clone();
            Button::new("theme-delete", "themes-delete-button")
                .role(ButtonRole::Destructive)
                .on_view(move || {
                    // An unwritten draft has nothing to delete: leaving
                    // it is enough.
                    if editing.written.get() {
                        ui.dispatcher
                            .submit(Studio::DELETE.with(editing.id.clone()));
                    }
                    ui.open(Page::Gallery);
                })
        };
        // Keep the primary effects together and the editing/lifecycle
        // commands on a second line so phone-sized editors retain targets.
        Box::new(ui::column![
            ui::row![apply, export],
            ui::row![duplicate, reset, delete]
        ])
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the editor lists every role of the theme document in order"
)]
fn editor(cx: &ViewCx<Studio>, ui: &Ui, editing: &Editing) -> Element {
    let raw = editing.id.as_str().to_owned();
    let document = {
        let raw = raw.clone();
        let editing = editing.clone();
        cx.value(move |studio: &Studio| editing.read(studio, |theme| theme.document(&raw)))
    };
    let preview = ModelView::from_state(&document, |document: &Option<ThemeDocument>| {
        document.as_ref().map_or_else(
            || -> Element { Box::new(Text::new("themes-missing")) },
            |document| -> Element {
                Box::new(ThemePreview::new(document, 260.0).with_id("theme-preview"))
            },
        )
    });
    let presence = ModelView::from_state(&document, |document: &Option<ThemeDocument>| {
        document.as_ref().map_or_else(
            || -> Element { Box::new(Spacer::new()) },
            |document| -> Element {
                Box::new(
                    ThemePreview::new(document, 24.0)
                        .presence()
                        .with_id("presence-strip"),
                )
            },
        )
    });
    let findings = {
        let editing = editing.clone();
        cx.text(move |studio: &Studio| {
            let Some(findings) = editing.read(studio, |theme| theme.findings(&raw)) else {
                return String::new();
            };
            if findings.is_empty() {
                String::from("Every pairing meets its contrast floor.")
            } else {
                format!("Too little contrast: {}", findings.join("; "))
            }
        })
        .with_id("contrast-findings")
        .with_live(ui::a11y::Politeness::Polite)
    };
    let actions = commands(cx, ui, editing);

    let theme = Theme::paths();
    let name = text(cx, editing, &theme.name).with_id("theme-name");

    let mut ladder = FormSection::new("themes-ladder");
    for rung in Rung::SURFACES.into_iter().chain(Rung::INK) {
        ladder = ladder.field(Field::new(rung_label(rung), rung_slider(cx, editing, rung)));
    }

    let face = text(cx, editing, &theme.face).with_id("theme-face");

    let form = Form::new()
        .section(FormSection::new("themes-name").field(Field::new("themes-name-label", name)))
        .section(
            FormSection::new("themes-palette")
                .field(Field::new("themes-polarity", polarity(cx, editing)).with_trailing_control())
                .field(Field::new(
                    "themes-hue",
                    hue_slider(cx, editing, (0.0, 360.0), &theme.hue, 0.0),
                ))
                .field(
                    Field::new(
                        "themes-chroma",
                        slider(cx, editing, (0.0, 0.010), &theme.chroma),
                    )
                    .with_help("themes-chroma-help"),
                ),
        )
        .section(ladder.content(Box::new(findings)))
        .section(
            FormSection::new("themes-emphasis")
                .field(
                    Field::new(
                        "themes-emphasis-kind",
                        choice(
                            cx,
                            editing,
                            ["Accent", "Monochrome"],
                            &theme.monochrome,
                            [false, true],
                        ),
                    )
                    .with_trailing_control(),
                )
                .field(
                    Field::new(
                        "themes-accent",
                        choice(
                            cx,
                            editing,
                            ["Your colour", "Chosen hue"],
                            &theme.tint,
                            [None, Some(150.0)],
                        ),
                    )
                    .with_trailing_control(),
                )
                .field(Field::new(
                    "themes-tint",
                    hue_slider(cx, editing, (0.0, 360.0), &theme.tint, 150.0),
                )),
        )
        .section(
            FormSection::new("themes-presence")
                .content(Box::new(presence))
                .field(Field::new(
                    "themes-presence-lightness-label",
                    slider(cx, editing, (0.35, 0.90), &theme.presence.lightness),
                ))
                .field(Field::new(
                    "themes-presence-chroma-label",
                    slider(cx, editing, (0.04, 0.20), &theme.presence.chroma),
                )),
        )
        .section(
            FormSection::new("themes-shape-density")
                .field(Field::new(
                    "themes-radius",
                    slider(cx, editing, (0.0, 28.0), &theme.window_radius),
                ))
                .field(Field::new(
                    "themes-density",
                    slider(cx, editing, (0.75, 1.5), &theme.density),
                )),
        )
        .section(
            FormSection::new("themes-type")
                .field(Field::new("themes-face", face).with_help("themes-face-help"))
                .field(Field::new(
                    "themes-size",
                    slider(cx, editing, (12.0, 18.0), &theme.base_size),
                ))
                .field(
                    Field::new(
                        "themes-distinct-letters",
                        choice(
                            cx,
                            editing,
                            ["Always", "With the typeface"],
                            &theme.distinct_letters,
                            [Some(true), None],
                        ),
                    )
                    .with_help("themes-distinct-letters-help")
                    .with_trailing_control(),
                ),
        )
        .section(
            FormSection::new("themes-motion-material")
                .field(Field::new(
                    "themes-motion",
                    slider(cx, editing, (0.0, 2.0), &theme.motion),
                ))
                .field(Field::new(
                    "themes-blur",
                    slider(cx, editing, (0.0, 48.0), &theme.blur),
                ))
                .field(Field::new(
                    "themes-tint-opacity",
                    slider(cx, editing, (0.6, 1.0), &theme.tint_opacity),
                ))
                .field(
                    Field::new(
                        "themes-tab-strip",
                        choice(
                            cx,
                            editing,
                            ["Frosted", "Solid"],
                            &theme.frosted_strip,
                            [true, false],
                        ),
                    )
                    .with_help("themes-tab-strip-help")
                    .with_trailing_control(),
                )
                .field(Field::new(
                    "themes-rim-span",
                    slider(cx, editing, (0.0, 1.0), &theme.rim_span),
                ))
                .field(Field::new(
                    "themes-rim-alpha",
                    slider(cx, editing, (0.0, 0.5), &theme.rim_alpha),
                ))
                .field(Field::new(
                    "themes-hover-alpha",
                    slider(cx, editing, (0.7, 1.0), &theme.hover_alpha),
                )),
        );

    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(ui::column![preview, actions, notice(ui), form,])),
    )
}
