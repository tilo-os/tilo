//! The strip over the canvas: the artwork's title, set like the label
//! beside a picture on a wall, taking back and bringing back steps, how
//! near the artwork is, and the sidebar shown or put away.

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::text_render::FontChoice;

use super::icon_button;
use crate::actions::CanvasAction;
use crate::model::Artwork;
use crate::resource::ArtworkResource;
use crate::selection::{Combine, Select, UseSelection};
use crate::tools::{Tools, ViewRequest};

/// The least room the title keeps, so it reads as a title, not a field.
const TITLE: f32 = 96.0; // about ten letters of the title face: a text width, not spacing
/// The zoom readout's width, so the buttons beside it hold still as it
/// changes.
const READOUT: f32 = 52.0; // "6400%" in the caption face: a text width, not spacing

/// The header.
pub fn header(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let title = cx
        .input(
            |art: &Artwork| art.title.clone(),
            ArtworkResource::SET_TITLE,
        )
        .with_plain_chrome()
        .with_font_family(FontChoice::Serif)
        .with_font_size(17.0)
        .with_placeholder(&ui::i18n::I18nManager::lookup_plain("canvas-untitled"))
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-artwork-title"));
    let lookup = ui::i18n::I18nManager::lookup_plain;
    Box::new(
        Row::new()
            .no_wrap()
            .with_padding(10.0, 6.0)
            .with_gap(2.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            // The title takes the room the controls leave, and holds them
            // at the far end.
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(TITLE)
                    .with_child(Box::new(title)),
            ))
            .push(add_picture(tools))
            .push(export(tools))
            .push(Box::new(Spacer::fixed(6.0)))
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
            .push(Box::new(Spacer::fixed(10.0)))
            .push(zoom(tools))
            .push(Box::new(Spacer::fixed(10.0)))
            .push(Box::new(toggle(
                &tools.sidebar,
                "canvas-sidebar",
                Icon::PanelRight,
            ))),
    )
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
        Button::new("canvas-add-picture", "", None)
            .with_icon(Icon::Image)
            .ghost()
            .with_accessible_name(&name)
            .tooltip(name.clone())
            .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
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

/// Export the artwork as a picture, saved where the person chooses: the
/// same rendering as the `export` action.
fn export(tools: &Tools) -> Element {
    let mut menu = Dropdown::from_state(Vec::new(), &tools.export)
        .as_menu(Icon::Download)
        .with_id("canvas-export")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-export"));
    menu.set_value_options(
        ["png", "jpeg", "webp"]
            .into_iter()
            .map(|format| {
                (
                    format.to_owned(),
                    ui::i18n::I18nManager::lookup_plain(&format!("canvas-export-{format}")),
                )
            })
            .collect(),
    );
    Box::new(menu)
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
            let facts = facts.clone();
            let tools = tools.clone();
            cx.switch(&tools.layer.clone(), move |cx, layer| {
                let masked = facts
                    .masks
                    .iter()
                    .find(|(mask, _, _)| mask == layer)
                    .map(|(_, owner, name)| (owner.clone(), name.clone()));
                if !facts.area && !combine && masked.is_none() {
                    return Box::new(Spacer::fixed(0.0)) as Element;
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
        }
    }

    fn label(&self) -> String {
        ui::i18n::I18nManager::lookup_plain(&self.id)
    }

    /// The command's button: its icon and label, or the icon alone with
    /// the label as its tooltip and spoken name.
    fn button(self, cx: &ViewCx<ArtworkResource>, labelled: bool) -> Element {
        let label = self.label();
        let button = if labelled {
            Button::new(&self.id, &label, None)
                .with_icon(self.icon)
                .ghost()
                .fit_content(super::ICON_BUTTON)
        } else {
            Button::new(&self.id, "", None)
                .with_icon(self.icon)
                .ghost()
                .with_accessible_name(&label)
                .tooltip(label.clone())
                .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
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

/// A button's room in the strip, with the gap after it.
const SLOT: f32 = super::ICON_BUTTON + 2.0;

/// The strip's one row, as wide as `room` lets it be: labels are given in
/// the commands' order while they fit, every command keeps its icon while
/// that fits, and the last ones go into a menu when it does not.
fn strip(
    cx: &ViewCx<ArtworkResource>,
    tools: &Tools,
    (area, combine, masked): (bool, bool, Option<&(String, String)>),
    room: u32,
) -> Element {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a width in pixels, far below f32's exact range"
    )]
    let room = room as f32 - 24.0; // the row's insets
    let mut groups: Vec<Vec<Command>> = Vec::new();
    if area {
        groups.push(area_commands(tools));
    }
    if combine {
        groups.push(combine_commands(tools));
    }
    let count: usize = groups.iter().map(Vec::len).sum();
    #[allow(clippy::cast_precision_loss, reason = "a handful of buttons")]
    let rules = groups.len().saturating_sub(1) as f32 * 9.0;
    #[allow(clippy::cast_precision_loss, reason = "a handful of buttons")]
    let icons = count as f32 * SLOT + rules;
    // Painting a mask is said where the painting happens, with the way
    // back to the layer's own paint, said in full when there is room.
    let lead = masked.map(|(owner, name)| {
        let full =
            ui::i18n::I18nManager::format("canvas-painting-mask", &[("name", name.clone().into())]);
        let short = ui::i18n::I18nManager::lookup_plain("canvas-medium-mask");
        let said = if text_room(&full, 7.0) + 30.0 + SLOT + 10.0 + icons <= room {
            full
        } else {
            short
        };
        let width = text_room(&said, 7.0) + 30.0 + SLOT + 10.0;
        (said, owner.clone(), width)
    });
    let mut spare = room - icons - lead.as_ref().map_or(0.0, |(_, _, width)| *width);
    // What does not fit even as icons goes into the menu, from the end.
    let mut menu: Vec<Command> = Vec::new();
    while spare < 0.0 && groups.iter().map(Vec::len).sum::<usize>() > 0 {
        let Some(group) = groups.iter_mut().rev().find(|group| !group.is_empty()) else {
            break;
        };
        if let Some(command) = group.pop() {
            spare += SLOT;
            if menu.is_empty() {
                spare -= SLOT;
            }
            menu.insert(0, command);
        }
    }
    groups.retain(|group| !group.is_empty());
    let mut row = Row::new()
        .no_wrap()
        .with_padding(10.0, 4.0)
        .with_gap(2.0)
        .with_align_items(ui::taffy::AlignItems::Center);
    if let Some((said, owner, _)) = lead {
        row = row
            .push(Box::new(ui::widget::Badge::semantic_pill(
                said,
                ui::widget::BadgeTone::Accent,
            )))
            .push(mask_back(tools, owner))
            .push(Box::new(Spacer::fixed(8.0)));
    }
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            row = row
                .push(Box::new(Spacer::fixed(3.0)))
                .push(Box::new(ui::widget::Divider::vertical()))
                .push(Box::new(Spacer::fixed(3.0)));
        }
        for command in group {
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
    Box::new(super::pane::Pane::new(
        Box::new(row),
        super::pane::Edge::Bottom,
    ))
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
        Button::new("canvas-paint-layer", "", None)
            .with_icon(Icon::Brush)
            .ghost()
            .with_accessible_name(&label)
            .tooltip(label.clone())
            .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
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
        .with_font_feature(ui::text_render::OpenTypeTag(*b"tnum"), 1)
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
                    .with_width(READOUT)
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
        Button::new(id, "", None)
            .with_icon(icon)
            .ghost()
            .with_accessible_name(&name)
            .tooltip(name.clone())
            .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
            .on_view(move || view.set(Some(request))),
    )
}

/// An icon button that shows or hides what `state` says, as a view action.
fn toggle(state: &LocalState<bool>, id: &str, icon: Icon) -> Button {
    let flip = state.clone();
    let name = ui::i18n::I18nManager::lookup_plain(id);
    Button::new(id, "", None)
        .with_icon(icon)
        .ghost()
        .with_accessible_name(&name)
        .tooltip(name.clone())
        .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
        .on_view(move || flip.set(!flip.with(|on| *on)))
}
