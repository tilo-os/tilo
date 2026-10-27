//! The studio window: your themes in the sidebar, the presets as rendered
//! previews, and an editor for one theme with its contrast findings live.
//!
//! Every control is bound to the role it shows, by its path in the
//! [`Studio`]; switching polarity, resetting and deleting are the studio's
//! verbs. Applying and exporting are effects on this machine
//! ([`crate::apply`]) and refuse a theme that misses a contrast floor.

use std::sync::Arc;

use ui::framework::Refused;
use ui::icons::Icon;
use ui::prelude::*;
use ui::theme::{Preset, ThemeDocument};
use ui::tilo_resource::Path;
use ui::widget::{
    Actions as CardActions, ButtonRole, Card, CardGrid, CardWidth, Field, Form, FormSection,
    NavAction,
};

use crate::apply::AppearanceChoice;
use crate::preview::{Show, ThemePreview};
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
    /// The theme the editor shows, for the window bar's commands.
    shown: window_bar::Shown,
    /// Which miniature the editor previews: desktop, list, dialog or presence.
    preview_view: LocalState<String>,
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
    /// Whether this editor sent the draft's create and the studio does not
    /// hold the theme yet: edits made meanwhile are plain edits of it, sent
    /// after the create, rather than creates of their own that the studio
    /// would refuse.
    pending: LocalState<bool>,
}

impl Editing {
    fn new(cx: &ViewCx<Studio>, id: &str, base: Option<&str>) -> Self {
        let id = Id::from_raw(id);
        let draft = base.and_then(|base| {
            cx.read(|studio| studio.and_then(|studio| studio.draft(base).ok()))
                .map(|theme| Arc::new((base.to_owned(), theme)))
        });
        let pending = LocalState::new(false);
        let written = {
            let id = id.clone();
            let pending = pending.clone();
            cx.value(move |studio: &Studio| {
                let written = studio.themes.contains(&id);
                // Stored, the create is no longer pending; should the theme
                // go again (its creating edit undone), the next edit
                // creates it anew.
                if written {
                    pending.set(false);
                }
                written
            })
        };
        Self {
            id,
            draft,
            written,
            pending,
        }
    }

    /// The door refused `refusal` while the draft's create was pending: the
    /// theme was not written, so say why in `notice` and let the next edit
    /// create it again.
    fn refused(&self, refusal: &Refused, notice: &LocalState<String>) {
        if refusal.action == ui::tilo_resource::GROUP && self.pending.get() && !self.written.get() {
            self.pending.set(false);
            notice.set(format!("The theme was not created: {}", refusal.reason));
        }
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
    /// step, so the first edit of a draft is the one that creates it. Once
    /// the create is sent, later changes are the change alone, whether or
    /// not the studio holds the theme yet.
    fn invoke(&self, change: Invoke<Studio>) -> Invoke<Studio> {
        let create = self
            .draft
            .as_deref()
            .filter(|_| !self.written.get() && !self.pending.get())
            .map(|(base, _)| Studio::NEW_THEME.with((self.id.as_str().to_owned(), base.clone())));
        if create.is_some() {
            self.pending.set(true);
        }
        Invoke::group(create.into_iter().chain([change]))
    }

    /// Whether the theme is in the studio or on its way there.
    fn sent(&self) -> bool {
        self.written.get() || self.pending.get()
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
        shown: window_bar::Shown::default(),
        preview_view: LocalState::new(t("themes-preview-desktop")),
    };
    crate::effects::install(cx, &ui.notice, &ui.page, &ui.pane);
    {
        let ui = ui.clone();
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario
                && scenario != crate::GALLERY_SCENARIO
            {
                let view = match scenario {
                    "editor-list" => t("themes-preview-list"),
                    "editor-dialog" => t("themes-preview-dialog"),
                    _ => t("themes-preview-desktop"),
                };
                ui.preview_view.set(view);
                ui.open(Page::Theme(crate::SCENARIO_THEME.to_owned()));
            }
        });
    }
    let nav = sidebar(&ui);
    let content = {
        let ui = ui.clone();
        cx.switch(&ui.page.clone(), move |cx, page| {
            let editing = match page {
                Page::Gallery => None,
                Page::Theme(id) => Some(Editing::new(cx, id, None)),
                Page::Draft { id, base } => Some(Editing::new(cx, id, Some(base))),
            };
            ui.shown.borrow_mut().clone_from(&editing);
            match editing {
                Some(editing) => editor(cx, &ui, &editing),
                None => gallery(&ui),
            }
        })
    };
    let shell = NavShell::new()
        // The gallery's tiles and the editor's form need a page's width
        // beside the list; narrower, the window shows one at a time.
        .content_kind(ui::widget::ContentKind::Page)
        .with_id("themes-shell")
        .with_window_toolbar()
        .with_pane_state(&ui.pane)
        .sidebar(nav)
        .content(content);
    Box::new(window_bar::StudioWindow { shell, ui })
}

fn notice(ui: &Ui) -> Text {
    Text::from_state(&ui.notice, Clone::clone)
        .with_id("themes-notice")
        .with_role(TextRole::Caption)
        .with_live(ui::a11y::Politeness::Polite)
}

/// The editor's notice, which also says when the door refused the draft's
/// create ([`Editing::refused`]).
fn editor_notice(ui: &Ui, editing: &Editing) -> ModelView<Option<Refused>> {
    let ui = ui.clone();
    let editing = editing.clone();
    ModelView::from_state(
        &ui.dispatcher.refused(),
        move |refusal: &Option<Refused>| -> Element {
            if let Some(refusal) = refusal {
                editing.refused(refusal, &ui.notice);
            }
            Box::new(notice(&ui))
        },
    )
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
        // The cards keep their height to their width: the scrollbar's lane
        // stays, so the grid's width does not change as it starts to scroll.
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_stable_vertical_gutter()
            .with_child(Box::new(ui::column![
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

/// The miniature a preview label names, in the current locale.
fn preview_show(picked: &str) -> Show {
    if picked == t("themes-preview-list") {
        Show::List
    } else if picked == t("themes-preview-dialog") {
        Show::Dialog
    } else if picked == t("themes-preview-presence") {
        Show::Presence
    } else {
        Show::Desktop
    }
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
    let preview = {
        let source = document.clone();
        let preview_view = ui.preview_view.clone();
        ModelView::from_state(&preview_view, move |picked: &String| {
            let source = source.clone();
            let picked = picked.clone();
            Box::new(ModelView::from_state(
                &source,
                move |document: &Option<ThemeDocument>| -> Element {
                    let show = preview_show(&picked);
                    document.as_ref().map_or_else(
                        || -> Element { Box::new(Text::new("themes-missing")) },
                        |document| -> Element {
                            Box::new(
                                ThemePreview::new(document, 260.0)
                                    .show(show)
                                    .with_id("theme-preview"),
                            )
                        },
                    )
                },
            )) as Element
        })
    };
    let preview_switch = {
        let mode = ui.preview_view.clone();
        Row::new().with_padding_rhythm(None, None).push(Box::new(
            Dropdown::from_state(
                [
                    t("themes-preview-desktop"),
                    t("themes-preview-list"),
                    t("themes-preview-dialog"),
                    t("themes-preview-presence"),
                ]
                .to_vec(),
                &ui.preview_view,
            )
            .on_change(move |picked| mode.set(picked.to_owned()))
            .with_id("theme-preview-view")
            .with_accessible_name(t("themes-preview")),
        ))
    };
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
            .with_child(Box::new(ui::column![
                preview,
                preview_switch,
                editor_notice(ui, editing),
                form,
            ])),
    )
}

mod window_bar;

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    const BASE: &str = "dark-graphite";

    /// An editor of a draft of `BASE` under `id`, outside any window: what
    /// its controls send is applied when the test says, as a door that has
    /// not caught up yet would.
    fn draft(studio: &Replica<Studio>, id: &str) -> Editing {
        let theme = studio.state().unwrap().draft(BASE).unwrap();
        Editing {
            id: Id::from_raw(id),
            draft: Some(Arc::new((BASE.to_owned(), theme))),
            written: LocalState::new(false),
            pending: LocalState::new(false),
        }
    }

    #[test]
    fn edits_sent_before_the_create_is_stored_edit_the_one_theme() {
        let mut studio = Replica::<Studio>::new();
        let editing = draft(&studio, "t1");
        let roles = Theme::paths();
        let first = editing.invoke(Invoke::from(editing.path(&roles.density).set(1.2)));
        let second = editing.invoke(Invoke::from(editing.path(&roles.window_radius).set(20.0)));
        assert!(
            serde_json::Value::Object(second.arguments.clone())
                .to_string()
                .contains("field-edit")
        );
        assert!(
            !serde_json::Value::Object(second.arguments.clone())
                .to_string()
                .contains("new-theme"),
            "the create is sent once"
        );
        studio
            .apply(first)
            .expect("the first edit creates the theme");
        studio.apply(second).expect("the second edits it");
        let state = studio.state().unwrap();
        assert_eq!(state.themes.len(), 1);
        let theme = &state.themes[&Id::from_raw("t1")];
        assert!((theme.density - 1.2).abs() < 1e-6);
        assert!((theme.window_radius - 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_refused_create_is_said_and_the_next_edit_creates_again() {
        let mut studio = Replica::<Studio>::new();
        let editing = draft(&studio, "t1");
        let notice = LocalState::new(String::new());
        let density = editing.path(&Theme::paths().density);
        let refused = editing.invoke(Invoke::from(density.set(9.0)));
        let reason = studio
            .apply(refused)
            .expect_err("a density outside its band is refused")
            .to_string();
        editing.refused(
            &Refused {
                action: ui::tilo_resource::GROUP.to_owned(),
                item: None,
                reason: reason.clone(),
                by_room: false,
            },
            &notice,
        );
        assert!(notice.get().contains(&reason), "{}", notice.get());
        studio
            .apply(editing.invoke(Invoke::from(density.set(1.2))))
            .expect("the next edit creates the theme");
        assert_eq!(studio.state().unwrap().themes.len(), 1);
    }
}
