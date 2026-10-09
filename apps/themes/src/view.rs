//! The studio window: your themes in the sidebar, the presets as rendered
//! previews, and an editor for one theme with its contrast findings live.
//!
//! Every value a control shows is read from the resource and every edit is
//! a [`Studio`] action. Applying and exporting are effects on this machine
//! ([`crate::apply`]) and refuse a theme that misses a contrast floor.

use ui::prelude::*;
use ui::theme::{Preset, ThemeDocument};
use ui::tokens::Emphasis;
use ui::widget::{ButtonRole, Field, Form, FormSection};

use crate::preview::ThemePreview;
use crate::studio::{Rung, Studio, Theme};

/// What the content pane shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Gallery,
    Theme(String),
}

/// View state shared by the parts of the window: never stored, synced or
/// undone.
#[derive(Clone)]
struct Ui {
    page: LocalState<Page>,
    pane: LocalState<NavPane>,
    notice: LocalState<String>,
    dispatcher: Dispatcher<Studio>,
    live: Live<Studio>,
}

impl Ui {
    fn open(&self, page: Page) {
        self.page.set(page);
        self.pane.set(NavPane::Content);
        self.notice.set(String::new());
    }

    /// Start a theme from `base` (a preset id or a theme id) and open it.
    fn start_from(&self, base: &str) {
        let id = format!("t{}", uuid::Uuid::new_v4().simple());
        self.dispatcher
            .invoke(Studio::NEW_THEME.with((id.clone(), base.to_owned())));
        self.open(Page::Theme(id));
    }

    fn document(&self, id: &str) -> Option<ThemeDocument> {
        let key = Id::from_raw(id);
        self.live
            .with(|studio| studio.themes.get(&key).map(|theme| theme.document(id)))
    }

    fn apply(&self, document: Option<&ThemeDocument>) {
        if let Some(document) = document {
            self.dispatcher
                .effect(&crate::effects::ThemesEffect::ApplyTheme {
                    theme: document.id.clone(),
                });
        }
    }

    fn export(&self, document: Option<&ThemeDocument>) {
        if let Some(document) = document {
            self.dispatcher
                .effect(&crate::effects::ThemesEffect::Export {
                    theme: document.id.clone(),
                });
        }
    }
}

/// The whole window.
pub(crate) fn root(cx: &ViewCx<Studio>) -> Element {
    let ui = Ui {
        page: LocalState::new(Page::Gallery),
        pane: LocalState::new(NavPane::Content),
        notice: LocalState::new(String::new()),
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
    let nav = sidebar(cx, &ui);
    let content = {
        let ui = ui.clone();
        cx.switch(&ui.page.clone(), move |cx, page| match page {
            Page::Gallery => gallery(&ui),
            Page::Theme(id) => editor(cx, &ui, id),
        })
    };
    Box::new(
        NavShell::new()
            .with_id("themes-shell")
            .mode(NavMode::Overlay)
            .with_pane_state(&ui.pane)
            .sidebar(nav)
            .content(content),
    )
}

fn heading(key: &str) -> Text {
    Text::new(key)
        .with_role(TextRole::Label)
        .with_heading_level(2)
        .dimmed()
}

fn notice(ui: &Ui) -> Text {
    Text::from_state(&ui.notice, Clone::clone)
        .with_id("themes-notice")
        .with_role(TextRole::Caption)
        .with_live(ui::a11y::Politeness::Polite)
}

// ---------------------------------------------------------------- sidebar

fn sidebar(cx: &ViewCx<Studio>, ui: &Ui) -> Element {
    let gallery = {
        let ui = ui.clone();
        let page = ui.page.clone();
        ModelView::from_state(&page, move |page| {
            let open = ui.clone();
            let button = Button::new("themes-gallery", "themes-gallery")
                .on_view(move || open.open(Page::Gallery));
            Box::new(if *page == Page::Gallery {
                button
            } else {
                button.role(ButtonRole::Plain)
            })
        })
    };
    let rows = {
        let ui = ui.clone();
        cx.each(
            |studio: &Studio| &studio.themes,
            move |row: &mut ViewCx<Studio, Theme>| -> Element {
                let id = row
                    .item()
                    .map(|id| id.as_str().to_owned())
                    .unwrap_or_default();
                let name = row.value(|theme: &Theme| theme.name.clone());
                let page = ui.page.clone();
                let ui = ui.clone();
                Box::new(ModelView::from_document(
                    move |_, _| (name.get(), page.get() == Page::Theme(id.clone())),
                    {
                        let id = row
                            .item()
                            .map(|id| id.as_str().to_owned())
                            .unwrap_or_default();
                        move |(name, open): &(String, bool)| -> Element {
                            let target = ui.clone();
                            let page = Page::Theme(id.clone());
                            let label = if name.trim().is_empty() {
                                "themes-untitled"
                            } else {
                                name.as_str()
                            };
                            let button = Button::new(&format!("theme-{id}"), label)
                                .on_view(move || target.open(page.clone()));
                            Box::new(if *open {
                                button
                            } else {
                                button.role(ButtonRole::Plain)
                            })
                        }
                    },
                ))
            },
        )
    };
    let new_theme = {
        let ui = ui.clone();
        Button::new("themes-new", "themes-new").on_view(move || ui.start_from("dark"))
    };
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(ui::column![
                gallery,
                heading("themes-yours"),
                rows,
                new_theme,
            ])),
    )
}

// ---------------------------------------------------------------- gallery

fn gallery(ui: &Ui) -> Element {
    let mut cards = Row::new().with_id("themes-presets");
    for preset in Preset::ALL {
        cards = cards.push(preset_card(ui, preset));
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(ui::column![
                Text::new("themes-presets")
                    .with_role(TextRole::Title)
                    .with_heading_level(1),
                cards,
                notice(ui),
            ])),
    )
}

fn preset_card(ui: &Ui, preset: Preset) -> Element {
    let use_it = {
        let ui = ui.clone();
        Button::new(&format!("use-{}", preset.id()), "themes-use")
            .on_effect(move || ui.apply(Some(&preset.document())))
    };
    let customize = {
        let ui = ui.clone();
        Button::new(&format!("customize-{}", preset.id()), "themes-customize")
            .role(ButtonRole::Plain)
            .on_view(move || ui.start_from(preset.id()))
    };
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(288.0)
            .with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_id(format!("preset-{}", preset.id()))
                    .push(Box::new(
                        ThemePreview::new(&preset.document(), 180.0)
                            .with_id(format!("preview-{}", preset.id())),
                    ))
                    .push(Box::new(
                        Text::new(preset.name()).with_role(TextRole::Label),
                    ))
                    .push(Box::new(ui::row![use_it, customize])),
            )),
    )
}

// ---------------------------------------------------------------- editor

/// A slider over `band` for one numeric role of the theme `id`.
fn slider<A: ui::tilo_resource::Params + From<(Id, f64)>>(
    cx: &ViewCx<Studio>,
    id: &Id,
    band: (f64, f64),
    read: impl Fn(&Theme) -> f64 + 'static,
    action: ui::tilo_resource::ActionRef<Studio, A>,
) -> Slider {
    let (lo, hi) = band;
    let span = (hi - lo).max(f64::EPSILON);
    let key = id.clone();
    let target = id.clone();
    cx.control_with(
        Slider::from_state,
        move |studio: &Studio| {
            studio
                .themes
                .get(&key)
                .map_or(0.0, |theme| ((read(theme) - lo) / span).clamp(0.0, 1.0))
        },
        action.map(move |norm: f64| (target.clone(), lo + norm * span)),
    )
    .with_id(&format!("{}-{}", band_id(lo, hi), id.as_str()))
}

fn band_id(lo: f64, hi: f64) -> String {
    format!("slider-{lo}-{hi}")
}

fn rung_slider(cx: &ViewCx<Studio>, id: &Id, rung: Rung) -> Slider {
    let key = id.clone();
    let target = id.clone();
    cx.control_with(
        Slider::from_state,
        move |studio: &Studio| {
            studio
                .themes
                .get(&key)
                .map_or(0.0, |theme| f64::from(rung.of(&theme.palette.0)))
        },
        Studio::SET_LIGHTNESS
            .map(move |lightness: f64| (target.clone(), rung.id().to_owned(), lightness)),
    )
    .with_id(&format!("lightness-{}", rung.id()))
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

fn choice<A: ui::tilo_resource::Params + From<(Id, bool)>>(
    cx: &ViewCx<Studio>,
    id: &Id,
    options: [&'static str; 2],
    read: impl Fn(&Theme) -> bool + 'static,
    action: ui::tilo_resource::ActionRef<Studio, A>,
) -> Dropdown {
    let key = id.clone();
    let target = id.clone();
    let [first, second] = options;
    cx.control_with(
        |state| Dropdown::from_state(vec![first.to_owned(), second.to_owned()], state),
        move |studio: &Studio| {
            let on = studio.themes.get(&key).is_some_and(&read);
            String::from(if on { first } else { second })
        },
        action.map(move |picked: String| (target.clone(), picked == first)),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the editor lists every role of the theme document in order"
)]
fn editor(cx: &ViewCx<Studio>, ui: &Ui, raw: &str) -> Element {
    let id = Id::from_raw(raw);
    let document = {
        let key = id.clone();
        let raw = raw.to_owned();
        cx.value(move |studio: &Studio| studio.themes.get(&key).map(|theme| theme.document(&raw)))
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
        let key = id.clone();
        let raw = raw.to_owned();
        cx.text(move |studio: &Studio| {
            let Some(theme) = studio.themes.get(&key) else {
                return String::new();
            };
            let findings = theme.findings(&raw);
            if findings.is_empty() {
                String::from("Every pairing meets its contrast floor.")
            } else {
                format!("Too little contrast: {}", findings.join("; "))
            }
        })
        .with_id("contrast-findings")
        .with_live(ui::a11y::Politeness::Polite)
    };
    let actions = {
        let ui = ui.clone();
        let raw = raw.to_owned();
        let legible = cx.value({
            let key = id.clone();
            let raw = raw.clone();
            move |studio: &Studio| {
                studio
                    .themes
                    .get(&key)
                    .is_some_and(|theme| theme.findings(&raw).is_empty())
            }
        });
        ModelView::from_state(&legible, move |legible: &bool| -> Element {
            let apply = {
                let ui = ui.clone();
                let raw = raw.clone();
                Button::new("theme-apply", "themes-apply")
                    .role(ButtonRole::Primary)
                    .with_enabled(*legible)
                    .on_effect(move || ui.apply(ui.document(&raw).as_ref()))
            };
            let export = {
                let ui = ui.clone();
                let raw = raw.clone();
                Button::new("theme-export", "themes-export")
                    .with_enabled(*legible)
                    .on_effect(move || ui.export(ui.document(&raw).as_ref()))
            };
            let duplicate = {
                let ui = ui.clone();
                let raw = raw.clone();
                Button::new("theme-duplicate", "themes-duplicate")
                    .role(ButtonRole::Plain)
                    .on_view(move || ui.start_from(&raw))
            };
            let reset = {
                let ui = ui.clone();
                let raw = raw.clone();
                Button::new("theme-reset", "themes-reset")
                    .role(ButtonRole::Plain)
                    .on_view(move || {
                        ui.dispatcher
                            .invoke(Studio::RESET.with(Id::from_raw(raw.clone())));
                    })
            };
            let delete = {
                let ui = ui.clone();
                let raw = raw.clone();
                Button::new("theme-delete", "themes-delete")
                    .role(ButtonRole::Destructive)
                    .on_view(move || {
                        ui.dispatcher
                            .invoke(Studio::DELETE.with(Id::from_raw(raw.clone())));
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
    };

    let name = {
        let key = id.clone();
        let target = id.clone();
        cx.input_with(
            move |studio: &Studio| {
                studio
                    .themes
                    .get(&key)
                    .map(|theme| theme.name.clone())
                    .unwrap_or_default()
            },
            Studio::RENAME.map(move |name: String| (target.clone(), name)),
        )
        .with_id("theme-name")
    };

    let mut ladder = FormSection::new("themes-ladder");
    for rung in Rung::SURFACES.into_iter().chain(Rung::INK) {
        ladder = ladder.field(Field::new(rung_label(rung), rung_slider(cx, &id, rung)));
    }

    let face = {
        let key = id.clone();
        let target = id.clone();
        cx.input_with(
            move |studio: &Studio| {
                studio
                    .themes
                    .get(&key)
                    .map(|theme| theme.face.clone())
                    .unwrap_or_default()
            },
            Studio::SET_FACE.map(move |face: String| (target.clone(), face)),
        )
        .with_id("theme-face")
    };

    let form = Form::new()
        .section(FormSection::new("themes-name").field(Field::new("themes-name-label", name)))
        .section(
            FormSection::new("themes-palette")
                .field(
                    Field::new(
                        "themes-polarity",
                        choice(
                            cx,
                            &id,
                            ["Dark", "Light"],
                            |theme| theme.palette.0.dark,
                            Studio::SET_DARK,
                        ),
                    )
                    .with_trailing_control(),
                )
                .field(Field::new(
                    "themes-hue",
                    slider(
                        cx,
                        &id,
                        (0.0, 360.0),
                        |theme| f64::from(theme.palette.0.hue.unwrap_or_default()),
                        Studio::SET_HUE,
                    ),
                ))
                .field(
                    Field::new(
                        "themes-chroma",
                        slider(
                            cx,
                            &id,
                            (0.0, 0.010),
                            |theme| f64::from(theme.palette.0.chroma),
                            Studio::SET_CHROMA,
                        ),
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
                            &id,
                            ["Tinted", "Inverted"],
                            |theme| matches!(theme.emphasis.0, Emphasis::Tinted { .. }),
                            Studio::SET_TINTED,
                        ),
                    )
                    .with_trailing_control(),
                )
                .field(Field::new(
                    "themes-tint",
                    slider(
                        cx,
                        &id,
                        (0.0, 360.0),
                        |theme| match theme.emphasis.0 {
                            Emphasis::Tinted { hue } => f64::from(hue),
                            Emphasis::Inverted => 150.0,
                        },
                        Studio::SET_TINT_HUE,
                    ),
                )),
        )
        .section(
            FormSection::new("themes-presence")
                .content(Box::new(presence))
                .field(Field::new(
                    "themes-presence-lightness",
                    slider(
                        cx,
                        &id,
                        (0.35, 0.90),
                        |theme| f64::from(theme.presence.0.lightness),
                        Studio::SET_PRESENCE_LIGHTNESS,
                    ),
                ))
                .field(Field::new(
                    "themes-presence-chroma",
                    slider(
                        cx,
                        &id,
                        (0.04, 0.20),
                        |theme| f64::from(theme.presence.0.chroma),
                        Studio::SET_PRESENCE_CHROMA,
                    ),
                )),
        )
        .section(
            FormSection::new("themes-shape-density")
                .field(Field::new(
                    "themes-radius",
                    slider(
                        cx,
                        &id,
                        (0.0, 28.0),
                        |theme| f64::from(theme.window_radius),
                        Studio::SET_WINDOW_RADIUS,
                    ),
                ))
                .field(Field::new(
                    "themes-density",
                    slider(
                        cx,
                        &id,
                        (0.75, 1.5),
                        |theme| f64::from(theme.density),
                        Studio::SET_DENSITY,
                    ),
                )),
        )
        .section(
            FormSection::new("themes-type")
                .field(Field::new("themes-face", face).with_help("themes-face-help"))
                .field(Field::new(
                    "themes-size",
                    slider(
                        cx,
                        &id,
                        (12.0, 18.0),
                        |theme| f64::from(theme.base_size),
                        Studio::SET_BASE_SIZE,
                    ),
                ))
                .field(
                    Field::new(
                        "themes-distinct-letters",
                        choice(
                            cx,
                            &id,
                            ["Always", "With the typeface"],
                            |theme| theme.distinct_letters.0 == Some(true),
                            Studio::SET_DISTINCT_LETTERS,
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
                    slider(
                        cx,
                        &id,
                        (0.0, 2.0),
                        |theme| f64::from(theme.motion),
                        Studio::SET_MOTION,
                    ),
                ))
                .field(Field::new(
                    "themes-blur",
                    slider(
                        cx,
                        &id,
                        (0.0, 48.0),
                        |theme| f64::from(theme.blur),
                        Studio::SET_BLUR,
                    ),
                ))
                .field(Field::new(
                    "themes-tint-opacity",
                    slider(
                        cx,
                        &id,
                        (0.6, 1.0),
                        |theme| f64::from(theme.tint_opacity),
                        Studio::SET_TINT_OPACITY,
                    ),
                )),
        );

    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(ui::column![preview, actions, notice(ui), form,])),
    )
}
