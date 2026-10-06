//! The header over the canvas: the artwork's title, set like the label
//! beside a picture on a wall, taking back and bringing back steps and how
//! near the artwork is; and the strip of what to do with a selection.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::text_render::FontChoice;
use ui::widget::EditableLabel;
use ui::widget::{ButtonRole, ControlSize};

use super::icon_button;
use crate::actions::{Align, CanvasAction};
use crate::model::Artwork;
use crate::resource::ArtworkResource;
use crate::selection::{Combine, Select, UseSelection};
use crate::tools::{Sheet, Tools, ViewRequest};

/// The content column's header: the artwork's title, then adding a
/// picture, exporting, undo and redo, and the zoom. The window shell owns
/// the row, the inspector's toggle and History.
pub fn header(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Vec<Element> {
    // Finish editing the title as one resource action and undo step.
    let titled = cx.value(|art: &Artwork| art.title.clone());
    let title = cx.switch(&titled, |cx, title| {
        Box::new(
            cx.actions().bind(
                EditableLabel::new(title)
                    .edits_on_click()
                    .with_placeholder(ui::i18n::I18nManager::lookup_plain("canvas-untitled"))
                    .with_font_family(FontChoice::Serif)
                    .with_text_role(TextRole::Heading)
                    .with_id("canvas-title")
                    .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
                        "canvas-artwork-title",
                    )),
                ArtworkResource::SET_TITLE,
            ),
        ) as Element
    });
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let row = Row::new()
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        // The title takes the room the controls leave, and holds them at
        // the far end.
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(title)
                        .push(notice(cx, tools)),
                )),
        ))
        .push(add_picture(tools))
        .push(Box::new(sheet_button(
            tools,
            "canvas-export-title",
            Icon::Download,
            Sheet::Export,
        )))
        .push(menu(cx, tools))
        .push(Box::new(icon_button(
            cx,
            Icon::Undo2,
            &lookup("canvas-undo"),
            ArtworkResource::UNDO.invoke(),
        )))
        .push(Box::new(icon_button(
            cx,
            Icon::Redo2,
            &lookup("canvas-redo"),
            ArtworkResource::REDO.invoke(),
        )))
        .push(zoom(tools));
    // One group: each control is one of the artwork's actions (or a view
    // of it) an agent already has, so none is offered again on its own.
    vec![Box::new(super::pane::Group::new(Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_min_width(0.0)
            .with_child(Box::new(row)),
    )))]
}

/// Choose a picture to add: on an empty sheet it becomes the artwork, the
/// sheet taking its size; otherwise it is placed on the artwork. The
/// canvas takes the answer when the chooser gives it.
fn add_picture(tools: &Tools) -> Element {
    let picking = tools.picking.clone();
    let name = ui::i18n::I18nManager::lookup_plain("canvas-add-picture");
    let title = ui::i18n::I18nManager::lookup_plain("canvas-add-picture-title");
    let confirm = ui::i18n::I18nManager::lookup_plain("canvas-add-picture-confirm");
    let kinds = ui::i18n::I18nManager::lookup_plain("canvas-pictures");
    Box::new(
        Button::icon_only("canvas-add-picture", Icon::Image, &name)
            .control_size(ControlSize::Small)
            .on_view(move || {
                let request = ui::portal::PickRequest::open(&title, &confirm)
                    .filter(
                        ui::portal::FileFilter::new(&kinds)
                            .mimetype("image/png")
                            .mimetype("image/jpeg")
                            .mimetype("image/webp"),
                    )
                    .send_contents();
                *picking.borrow_mut() = Some(request);
            }),
    )
}

/// What of the artwork the strip over the canvas follows: whether there is
/// a selection, and each mask with the name of the layer it masks.
#[derive(Clone, Default, PartialEq)]
struct StripFacts {
    area: bool,
    /// Each mask's id, and the id and name of the layer it masks.
    masks: Vec<(String, String, String)>,
}

/// The strip over the canvas while there is something to do with what is
/// selected, or a mask is being painted: an area to fill, outline, mask the
/// chosen layer to, clear, invert or drop; shapes to combine into one; the
/// mask being painted, and the way back to its layer. Nothing otherwise.
/// One row at any width: what does not fit its label keeps its icon.
pub fn context_bar(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let facts = cx.value(|art: &Artwork| StripFacts {
        area: art.selection.is_some(),
        masks: art
            .layers
            .iter()
            .filter(|layer| layer.is_mask())
            .map(|mask| {
                let owner = art
                    .layer(&mask.mask_of)
                    .map_or_else(|| mask.mask_of.clone(), |owner| owner.name.clone());
                (mask.id.clone(), mask.mask_of.clone(), owner)
            })
            .collect(),
    });
    let tools = tools.clone();
    cx.switch(&facts, move |cx, facts| {
        let facts = facts.clone();
        let tools = tools.clone();
        cx.switch(&tools.selection.clone(), move |cx, picked| {
            let combine = picked.len() >= 2;
            let align = !picked.is_empty();
            let facts = facts.clone();
            let tools = tools.clone();
            cx.switch(&tools.layer.clone(), move |cx, layer| {
                let masked = facts
                    .masks
                    .iter()
                    .find(|(mask, _, _)| mask == layer)
                    .map(|(_, owner, name)| (owner.clone(), name.clone()));
                if !facts.area && !align && masked.is_none() {
                    return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
                }
                let area = facts.area;
                let tools = tools.clone();
                cx.switch(&tools.room.clone(), move |cx, room| {
                    strip(cx, &tools, (area, combine, masked.as_ref()), *room)
                })
            })
        })
    })
}

/// What a command of the strip does: the artwork's edit it makes, read
/// when it is chosen (the brush's layer and paint as they are then).
type Edit = std::sync::Arc<dyn Fn() -> CanvasAction + Send + Sync>;

/// How a command's button is bound to its action.
type Bind = Box<dyn FnOnce(Button, &ViewCx<ArtworkResource>) -> Element>;

/// A command of the strip: its icon, its label's string id, and the edit
/// it makes, through its own action as a button or through the generic
/// `apply` from the menu of what does not fit.
struct Command {
    icon: Icon,
    id: String,
    bind: Bind,
    edit: Edit,
    /// Shown by its icon alone at any width, as a row of alignments is.
    iconic: bool,
    /// Asked of the canvas rather than made as an action (work that runs
    /// off the window's thread): never moved into the menu, which can only
    /// apply actions.
    local: bool,
}

impl Command {
    fn new(
        icon: Icon,
        id: &str,
        action: impl ui::tilo_resource::Binds<ArtworkResource, ()>,
        edit: Edit,
    ) -> Self {
        Self {
            icon,
            id: id.to_owned(),
            bind: Box::new(move |button, cx| Box::new(cx.actions().bind(button, action))),
            edit,
            iconic: false,
            local: false,
        }
    }

    /// A command the canvas carries out off the window's thread: `select`,
    /// handed to it when the button is pressed.
    fn asking(
        icon: Icon,
        id: &str,
        select: impl Fn() -> crate::resource::FindArea + 'static,
    ) -> Self {
        Self {
            icon,
            id: id.to_owned(),
            bind: Box::new(move |button, cx| {
                let dispatch = cx.dispatcher();
                Box::new(button.on_effect(move || {
                    dispatch.effect(&crate::resource::ViewOp::FindArea(select()));
                }))
            }),
            edit: std::sync::Arc::new(|| CanvasAction::Select(Box::default())),
            iconic: true,
            local: true,
        }
    }

    /// The command by its icon alone, its label its tooltip.
    fn iconic(mut self) -> Self {
        self.iconic = true;
        self
    }

    fn label(&self) -> String {
        ui::i18n::I18nManager::lookup_plain(&self.id)
    }

    /// The command's button: its icon and label, or the icon alone with
    /// the label as its tooltip and spoken name.
    fn button(self, cx: &ViewCx<ArtworkResource>, labelled: bool) -> Element {
        let label = self.label();
        let button = if labelled {
            Button::new(&self.id, &label)
                .with_icon(self.icon)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
        } else {
            Button::icon_only(&self.id, self.icon, &label).control_size(ControlSize::Small)
        };
        (self.bind)(button, cx)
    }

    /// About how wide its label makes its button, beyond the icon alone:
    /// an estimate on the generous side, since the strip is laid out
    /// before its text is measured.
    fn label_room(&self) -> f32 {
        text_room(&self.label(), 7.5) + 22.0
    }
}

/// About how wide `text` sets at `per_letter` px a letter: the strip is
/// laid out before its text is measured.
fn text_room(text: &str, per_letter: f32) -> f32 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a label's letter count is small"
    )]
    let letters = text.chars().count() as f32;
    letters * per_letter
}

/// The strip's one row, as wide as `room` lets it be: labels are given in
/// the commands' order while they fit, every command keeps its icon while
/// that fits, and the last ones go into a menu when it does not.
fn strip(
    cx: &ViewCx<ArtworkResource>,
    tools: &Tools,
    (area, combine, masked): (bool, bool, Option<&(String, String)>),
    room: u32,
) -> Element {
    let spacing = ui::tokens::Spacing::BASE;
    // A button's room in the strip: an icon button is a pointer target
    // square, and the strip lays its buttons edge to edge. The strip is
    // laid out before a theme is known, so it reads the stock metrics.
    let slot = ui::tokens::Metrics::BASE.min_pointer_target;
    // Less the bar's inset from the canvas and the row's own.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a width in pixels, far below f32's exact range"
    )]
    let room = room as f32 - 2.0 * (spacing.s2 + spacing.s3);
    let mut groups: Vec<Vec<Command>> = Vec::new();
    if area {
        groups.push(area_commands(tools));
    }
    if combine {
        groups.push(combine_commands(tools));
    }
    if tools.selection.with(|picked| !picked.is_empty()) {
        groups.push(align_commands(tools));
    }
    let count: usize = groups.iter().map(Vec::len).sum();
    #[allow(clippy::cast_precision_loss, reason = "a handful of buttons")]
    let rules = groups.len().saturating_sub(1) as f32 * (2.0 * spacing.s1 + 1.0);
    #[allow(clippy::cast_precision_loss, reason = "a handful of buttons")]
    let icons = count as f32 * slot + rules;
    // Painting a mask is said where the painting happens, with the way
    // back to the layer's own paint, said in full when there is room.
    let lead = masked.map(|(owner, name)| {
        let full =
            ui::i18n::I18nManager::format("canvas-painting-mask", &[("name", name.clone().into())]);
        let short = ui::i18n::I18nManager::lookup_plain("canvas-medium-mask");
        let said = if text_room(&full, 7.0) + 30.0 + slot + spacing.s2 + icons <= room {
            full
        } else {
            short
        };
        let width = text_room(&said, 7.0) + 30.0 + slot + spacing.s2;
        (said, owner.clone(), width)
    });
    let mut spare = room - icons - lead.as_ref().map_or(0.0, |(_, _, width)| *width);
    // What does not fit even as icons goes into the menu, from the end.
    let mut menu: Vec<Command> = Vec::new();
    while spare < 0.0 && groups.iter().map(Vec::len).sum::<usize>() > 0 {
        let Some(group) = groups
            .iter_mut()
            .rev()
            .find(|group| group.iter().any(|command| !command.local))
        else {
            break;
        };
        let Some(at) = group.iter().rposition(|command| !command.local) else {
            break;
        };
        let command = group.remove(at);
        spare += slot;
        if menu.is_empty() {
            spare -= slot;
        }
        menu.insert(0, command);
    }
    groups.retain(|group| !group.is_empty());
    let mut row = Row::new()
        .no_wrap()
        .with_padding(spacing.s3, spacing.s1)
        .with_gap(spacing.s0)
        .with_align_items(ui::taffy::AlignItems::Center);
    if let Some((said, owner, _)) = lead {
        row = row
            .push(Box::new(
                ui::widget::Badge::text(said).style(ui::widget::ChipStyle::Neutral),
            ))
            .push(mask_back(tools, owner))
            .push(Box::new(Spacer::token(ui::tokens::SpacingStep::S2)));
    }
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            row = row
                .push(Box::new(Spacer::token(ui::tokens::SpacingStep::S1)))
                .push(Box::new(ui::widget::Divider::vertical()))
                .push(Box::new(Spacer::token(ui::tokens::SpacingStep::S1)));
        }
        for command in group {
            if command.iconic {
                row = row.push(command.button(cx, false));
                continue;
            }
            // Labels go in order and stop at the first that does not fit,
            // so the labelled commands are always the first ones.
            let wanted = command.label_room();
            let labelled = spare >= wanted;
            spare = if labelled {
                spare - wanted
            } else {
                f32::NEG_INFINITY
            };
            row = row.push(command.button(cx, labelled));
        }
    }
    if !menu.is_empty() {
        row = row.push(overflow(cx, menu));
    }
    // Floating a step in from the canvas's edges.
    Box::new(
        Container::new()
            .with_padding(spacing.s2)
            .with_child(Box::new(super::pane::Band::new(Box::new(row)))),
    )
}

/// The commands that did not fit, in a menu that makes each one's edit
/// through the artwork's generic `apply`.
fn overflow(cx: &ViewCx<ArtworkResource>, commands: Vec<Command>) -> Element {
    let options: Vec<(String, String)> = commands
        .iter()
        .map(|command| (command.id.clone(), command.label()))
        .collect();
    let edits: Vec<(String, Edit)> = commands
        .into_iter()
        .map(|command| (command.id, command.edit))
        .collect();
    let state = LocalState::new(String::new());
    let mut menu = Dropdown::from_state(Vec::new(), &state)
        .as_menu(Icon::MoreHorizontal)
        .with_id("canvas-strip-more")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-strip-more"));
    menu.set_value_options(options);
    let apply: ui::tilo_resource::ActionRef<ArtworkResource, CanvasAction> =
        ui::tilo_resource::ActionRef::new(
            ui::tilo_resource::APPLY,
            &[ui::tilo_resource::APPLY_PARAM],
        );
    Box::new(cx.actions().bind(
        menu,
        apply.map(move |chosen: String| {
            edits
                .iter()
                .find(|(id, _)| *id == chosen)
                .map_or_else(|| CanvasAction::Select(Box::default()), |(_, edit)| edit())
        }),
    ))
}

/// Back from a mask to its layer's own paint: a choice of the view, not
/// an edit.
fn mask_back(tools: &Tools, owner: String) -> Element {
    let chosen = tools.layer.clone();
    let label = ui::i18n::I18nManager::lookup_plain("canvas-paint-layer");
    Box::new(
        Button::icon_only("canvas-paint-layer", Icon::Brush, &label)
            .control_size(ControlSize::Small)
            .on_view(move || chosen.set(owner.clone())),
    )
}

/// Fill, outline, mask, clear, invert, select nothing.
fn area_commands(tools: &Tools) -> Vec<Command> {
    let using = |icon, what: &'static str| {
        let loaded = tools.loaded();
        let reads = loaded.clone();
        Command::new(
            icon,
            &format!("canvas-area-{what}"),
            ArtworkResource::USE_SELECTION
                .map(move |()| (what.to_owned(), loaded.layer(), Some(loaded.paint()))),
            std::sync::Arc::new(move || {
                CanvasAction::UseSelection(UseSelection {
                    what: what.to_owned(),
                    layer: reads.layer(),
                    colour: Some(reads.paint()),
                    ..UseSelection::default()
                })
            }),
        )
    };
    vec![
        using(Icon::PaintBucket, "fill"),
        using(Icon::SquareDashed, "stroke"),
        using(Icon::Layers, "mask"),
        using(Icon::Eraser, "clear"),
        {
            let zoom = tools.zoom.clone();
            Command::asking(Icon::Brush, "canvas-area-refine", move || {
                crate::resource::FindArea {
                    refine: Some(EDGE / zoom.with(|zoom| zoom.max(1e-6))),
                    ..Default::default()
                }
            })
        },
        refine(tools, Icon::Plus, "canvas-area-grow", Refine::Grow(GROW)),
        refine(
            tools,
            Icon::Minus,
            "canvas-area-shrink",
            Refine::Grow(-GROW),
        ),
        refine(
            tools,
            Icon::Droplet,
            "canvas-area-feather",
            Refine::Feather(FEATHER),
        ),
        Command::new(
            Icon::Contrast,
            "canvas-area-invert",
            ArtworkResource::SELECT_INVERT.map(|()| true),
            std::sync::Arc::new(|| {
                CanvasAction::Select(Box::new(Select {
                    invert: Some(true),
                    ..Select::default()
                }))
            }),
        ),
        Command::new(
            Icon::X,
            "canvas-area-none",
            ArtworkResource::SELECT_MODE.map(|()| "none".to_owned()),
            std::sync::Arc::new(|| {
                CanvasAction::Select(Box::new(Select {
                    mode: Some("none".into()),
                    ..Select::default()
                }))
            }),
        ),
    ]
}

/// How far, in screen pixels, a click fits the selection's edge to the
/// picture.
use crate::content_geometry::EDGE;
/// How wide, in screen pixels, a click softens the selection's edge.
use crate::content_geometry::FEATHER;
/// How far, in screen pixels, a click grows or shrinks the selection.
use crate::content_geometry::GROW;

/// A change of the selection's edge, in screen pixels.
#[derive(Clone, Copy)]
enum Refine {
    Grow(f64),
    Feather(f64),
}

/// Grow, shrink or feather the selection by a step seen at the zoom the
/// artwork is shown at, as a person judges an edge by eye.
fn refine(tools: &Tools, icon: Icon, id: &str, change: Refine) -> Command {
    let zoom = tools.zoom.clone();
    let units = move |pixels: f64| pixels / zoom.with(|zoom| zoom.max(1e-6));
    let reads = units.clone();
    let edit = std::sync::Arc::new(move || {
        let (grow, feather) = match change {
            Refine::Grow(pixels) => (Some(reads(pixels)), None),
            Refine::Feather(pixels) => (None, Some(reads(pixels))),
        };
        CanvasAction::Select(Box::new(Select {
            feather,
            grow,
            ..Select::default()
        }))
    });
    match change {
        Refine::Grow(pixels) => Command::new(
            icon,
            id,
            ArtworkResource::SELECT_GROW.map(move |()| units(pixels)),
            edit,
        ),
        Refine::Feather(pixels) => Command::new(
            icon,
            id,
            ArtworkResource::SELECT_FEATHER.map(move |()| units(pixels)),
            edit,
        ),
    }
    .iconic()
}

/// Unite, subtract, intersect or exclude the picked shapes.
fn combine_commands(tools: &Tools) -> Vec<Command> {
    [
        (Icon::SquaresUnite, "union"),
        (Icon::SquaresSubtract, "subtract"),
        (Icon::SquaresIntersect, "intersect"),
        (Icon::SquaresExclude, "exclude"),
    ]
    .into_iter()
    .map(|(icon, op)| {
        let picked = tools.selection.clone();
        let reads = picked.clone();
        Command::new(
            icon,
            &format!("canvas-combine-{op}"),
            ArtworkResource::COMBINE.map(move |()| (picked.with(Clone::clone), op.to_owned())),
            std::sync::Arc::new(move || {
                CanvasAction::Combine(Combine {
                    ids: reads.with(Clone::clone),
                    op: op.to_owned(),
                })
            }),
        )
    })
    .collect()
}

/// Line the picked marks up along an edge or centre line, or space them
/// evenly: among themselves when there are several, on the artwork when
/// there is one.
fn align_commands(tools: &Tools) -> Vec<Command> {
    let edges = [
        (Icon::AlignStartVertical, "left"),
        (Icon::AlignCenterVertical, "center"),
        (Icon::AlignEndVertical, "right"),
        (Icon::AlignStartHorizontal, "top"),
        (Icon::AlignCenterHorizontal, "middle"),
        (Icon::AlignEndHorizontal, "bottom"),
    ];
    let to = |picked: &[String]| (picked.len() == 1).then(|| "artwork".to_owned());
    let mut commands: Vec<Command> = edges
        .into_iter()
        .map(|(icon, edge)| {
            let picked = tools.selection.clone();
            let reads = picked.clone();
            Command::new(
                icon,
                &format!("canvas-align-{edge}"),
                ArtworkResource::ALIGN.map(move |()| {
                    let ids = picked.with(Clone::clone);
                    let to = to(&ids);
                    (ids, edge.to_owned(), to)
                }),
                std::sync::Arc::new(move || {
                    let ids = reads.with(Clone::clone);
                    CanvasAction::Align(Align {
                        to: to(&ids),
                        ids,
                        edge: Some(edge.to_owned()),
                        distribute: None,
                    })
                }),
            )
            .iconic()
        })
        .collect();
    // Spacing needs three to have a gap to even out.
    if tools.selection.with(|picked| picked.len() >= 3) {
        for (icon, axis) in [
            (Icon::AlignHorizontalDistributeCenter, "horizontal"),
            (Icon::AlignVerticalDistributeCenter, "vertical"),
        ] {
            let picked = tools.selection.clone();
            let reads = picked.clone();
            commands.push(
                Command::new(
                    icon,
                    &format!("canvas-distribute-{axis}"),
                    ArtworkResource::DISTRIBUTE
                        .map(move |()| (picked.with(Clone::clone), axis.to_owned())),
                    std::sync::Arc::new(move || {
                        CanvasAction::Align(Align {
                            ids: reads.with(Clone::clone),
                            edge: None,
                            distribute: Some(axis.to_owned()),
                            to: None,
                        })
                    }),
                )
                .iconic(),
            );
        }
    }
    commands
}

/// What the last open, save or export came to, in a caption under the
/// title: one line, cut short where it would crowd the header.
fn notice(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    cx.switch(&tools.notice, |_, notice| {
        let Some(said) = notice else {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
        };
        let shown = if said.chars().count() > NOTICE_LETTERS {
            let cut: String = said.chars().take(NOTICE_LETTERS - 1).collect();
            format!("{}…", cut.trim_end())
        } else {
            said.clone()
        };
        Box::new(
            Text::new(&shown)
                .with_role(TextRole::Caption)
                .dimmed()
                .with_live(ui::a11y::Politeness::Polite),
        )
    })
}

/// The most letters of a notice shown beside the title.
const NOTICE_LETTERS: usize = 44;

/// A button opening one of the window's dialogs.
fn sheet_button(tools: &Tools, id: &str, icon: Icon, which: Sheet) -> Button {
    let sheet = tools.sheet.clone();
    let name = ui::i18n::I18nManager::lookup_plain(id);
    Button::new(id, "")
        .with_icon(icon)
        .role(ui::widget::ButtonRole::Plain)
        .with_accessible_name(&name)
        .tooltip(name.clone())
        .control_size(ui::widget::ControlSize::Small)
        .on_view(move || sheet.set(Some(which)))
}

/// The artwork's menu: new, open, save, a copy, export, and the keys.
fn menu(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let tools = tools.clone();
    cx.switch(&tools.recents.clone(), move |_, recents| {
        let lookup = ui::i18n::I18nManager::lookup_plain;
        let items = [
            ("new", "canvas-menu-new"),
            ("open", "canvas-menu-open"),
            ("save", "canvas-menu-save"),
            ("save-as", "canvas-menu-save-as"),
            ("duplicate", "canvas-menu-duplicate"),
            ("export", "canvas-menu-export"),
            ("keys", "canvas-menu-keys"),
        ];
        let entry = |(value, id): &(&str, &str)| ((*value).to_owned(), lookup(id));
        // New and open first, then the artworks worked on lately to open
        // again in one step, then saving, exporting and the keys.
        let (start, rest) = items.split_at(2);
        let mut options: Vec<(String, String)> = start.iter().map(entry).collect();
        options.extend(recents.artworks.iter().take(MENU_RECENTS).map(|recent| {
            let name = if recent.title.is_empty() {
                std::path::Path::new(&recent.path)
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
            } else {
                recent.title.clone()
            };
            (
                format!("{RECENT}{}", recent.path),
                ui::i18n::I18nManager::format("canvas-menu-recent", &[("name", name.into())]),
            )
        }));
        options.extend(rest.iter().map(entry));
        // The choice is view state the file desk carries out.
        let mut menu = Dropdown::from_state(Vec::new(), &tools.menu)
            .as_menu(Icon::Menu)
            .with_id("canvas-menu")
            .with_accessible_name(lookup("canvas-menu"));
        menu.set_value_options(options);
        // Each command's keys at its row's end, from the table `?` shows.
        menu.set_option_shortcuts(
            [
                ("open", "canvas-keys-open"),
                ("save", "canvas-keys-save"),
                ("save-as", "canvas-keys-save-as"),
                ("export", "canvas-keys-export"),
                ("keys", "canvas-keys-shortcuts"),
            ]
            .into_iter()
            .filter_map(|(value, does)| Some((value, super::shortcuts::keys_for(does)?))),
        );
        Box::new(menu) as Element
    })
}

/// How a recent artwork's menu entry is told from the others.
pub(crate) const RECENT: &str = "recent:";
/// How many recent artworks the menu offers.
const MENU_RECENTS: usize = 5;

/// Zoom out, how near the artwork is now, zoom in, and fit it to the
/// window.
fn zoom(tools: &Tools) -> Element {
    let readout = {
        let zoom = tools.zoom.clone();
        Text::dynamic(move || {
            let percent = zoom.with(|zoom| zoom * 100.0);
            if percent < 10.0 {
                format!("{percent:.1}%")
            } else {
                format!("{percent:.0}%")
            }
        })
        .with_role(TextRole::Caption)
        .numeric()
    };
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(0.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(view_button(
                tools,
                "canvas-zoom-out",
                Icon::Minus,
                ViewRequest::ZoomOut,
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    // Room for "6400%", so the buttons beside the readout
                    // hold still as it changes: two pointer targets.
                    .with_width(2.0 * ui::tokens::Metrics::BASE.min_pointer_target)
                    .with_child(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_justify(ui::taffy::JustifyContent::Center)
                            .push(Box::new(readout)),
                    )),
            ))
            .push(view_button(
                tools,
                "canvas-zoom-in",
                Icon::Plus,
                ViewRequest::ZoomIn,
            ))
            .push(view_button(
                tools,
                "canvas-zoom-fit",
                Icon::Maximize2,
                ViewRequest::Fit,
            )),
    )
}

/// A button asking the canvas for a change of view: a view action, so an
/// agent zooms the way a person does.
fn view_button(tools: &Tools, id: &str, icon: Icon, request: ViewRequest) -> Element {
    let view = tools.view.clone();
    let name = ui::i18n::I18nManager::lookup_plain(id);
    Box::new(
        Button::icon_only(id, icon, &name)
            .control_size(ControlSize::Small)
            .on_view(move || view.set(Some(request))),
    )
}
