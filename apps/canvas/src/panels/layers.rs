//! The layers panel: one row per layer, top first, with its picture, its
//! name (double-click to rename), its medium, and whether it shows and
//! holds still; under it, the chosen layer's opacity and order, and adding
//! a layer in any medium. Every control is one of the artwork's actions.

use std::cell::RefCell;
use std::rc::Rc;

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{Badge, BadgeTone, EditableLabel, ListRow};

use super::{caption, icon_button, percent};
use crate::model::Artwork;
use crate::resource::ArtworkResource;
use crate::tools::Tools;

/// A layer as its row shows it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "shown, held still and clipped are independent facts of a layer"
)]
#[derive(Clone, Default, PartialEq)]
struct LayerRow {
    id: String,
    name: String,
    medium: String,
    visible: bool,
    locked: bool,
    opacity: f64,
    /// Its mask, shown beside it in its row rather than as a row of its own.
    mask: Option<MaskRow>,
    clip: bool,
}

/// A layer's mask, as its layer's row shows it.
#[derive(Clone, Default, PartialEq)]
struct MaskRow {
    id: String,
    invert: bool,
    /// Switched on: the layer shows through it.
    visible: bool,
}

/// The short name of a medium's engine, `plain` for vector art.
fn engine_label(art: &Artwork, medium: &str) -> String {
    let engine = crate::media::resolve(art, medium)
        .filter(|_| !medium.is_empty())
        .map(|medium| medium.engine);
    let id = match engine.as_deref() {
        Some("oil") => "canvas-medium-oil",
        Some("watercolour") => "canvas-medium-watercolour",
        _ => "canvas-medium-plain",
    };
    ui::i18n::I18nManager::lookup_plain(id)
}

/// The layers' rows, top first; a mask rides in its layer's row.
fn rows(art: &Artwork) -> Vec<LayerRow> {
    art.layers
        .iter()
        .rev()
        .filter(|layer| !layer.is_mask())
        .map(|layer| LayerRow {
            id: layer.id.clone(),
            name: layer.name.clone(),
            medium: engine_label(art, &layer.medium),
            visible: layer.visible,
            locked: layer.locked,
            opacity: layer.opacity,
            mask: art.mask_of(&layer.id).map(|mask| MaskRow {
                id: mask.id.clone(),
                invert: mask.invert,
                visible: mask.visible,
            }),
            clip: layer.clip,
        })
        .collect()
}

/// A layer's picture in its row, the artwork's shape at a glance.
const THUMB: (f32, f32) = (44.0, 33.0); // a picture's size, not spacing

/// How tall the list of layers grows before it scrolls.
const LIST: f32 = 232.0; // about four layer rows: a share of the sidebar, not spacing

/// The panel: the layers, scrolling when there are many, and under them
/// the chosen layer's own controls, always in view.
pub fn panel(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let rows = cx.value(rows);
    let list = {
        let tools = tools.clone();
        let known: Rc<RefCell<Vec<String>>> = Rc::default();
        cx.switch(&rows, move |cx, rows| {
            follow_new_layers(&known, rows, &tools.layer);
            let mut list = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(2.0);
            for row in rows {
                list = list.push(layer_row(cx, &tools, row));
            }
            Box::new(list)
        })
    };
    // The chosen layer's own controls, built again when another is chosen.
    let footer = cx.switch(&tools.layer, |cx, id| chosen_layer(cx, id));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(0.0)
            .push(Box::new(
                Scrollable::new_vertical()
                    .with_max_height(LIST)
                    .with_child(Box::new(
                        Column::new()
                            .with_intrinsic_height()
                            .with_vertical_padding(4.0)
                            .with_horizontal_padding(8.0, 8.0)
                            .push(list),
                    )),
            ))
            .push(Box::new(ui::widget::Divider::horizontal()))
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_vertical_padding(8.0)
                    .with_horizontal_padding(16.0, 12.0)
                    .push(footer),
            )),
    )
}

/// New marks go on the chosen layer; a layer added since (by anyone) is
/// where the next mark goes, and a removed one's choice falls to the top.
fn follow_new_layers(known: &RefCell<Vec<String>>, rows: &[LayerRow], target: &LocalState<String>) {
    // A mask counts too: one just added is where the next mark goes, as a
    // person adding a mask means to paint it.
    let ids: Vec<String> = rows
        .iter()
        .flat_map(|row| {
            std::iter::once(row.id.clone()).chain(row.mask.as_ref().map(|mask| mask.id.clone()))
        })
        .collect();
    let mut known = known.borrow_mut();
    let added = ids.iter().find(|id| !known.contains(id)).cloned();
    let chosen = target.with(Clone::clone);
    let next = match added {
        Some(added) if !known.is_empty() => Some(added),
        _ if !ids.contains(&chosen) => ids.first().cloned(),
        _ => None,
    };
    if let Some(next) = next {
        target.set(next);
    }
    *known = ids;
}

/// A layer's picture, and its mask's beside it when it has one: each, with
/// a mask, choosing what the brush paints.
fn pictures(cx: &ViewCx<ArtworkResource>, tools: &Tools, row: &LayerRow) -> Row {
    let thumb = |layer: &str, name: &str| -> Element {
        let mut picture = crate::thumb::Thumb::new(
            tools.renderer.clone(),
            Some(layer.to_owned()),
            THUMB.0,
            THUMB.1,
        );
        if row.mask.is_some() {
            picture = picture.ringed_while(&tools.layer, layer);
        }
        let picture = cx.surface(picture);
        if row.mask.is_none() {
            return picture;
        }
        // With a mask beside it, each picture chooses what is painted: the
        // layer's paint or its mask.
        let chosen = tools.layer.clone();
        let target = layer.to_owned();
        let choose = Button::new(&format!("canvas-paint-{layer}"), "", None)
            .ghost()
            .with_accessible_name(name)
            .tooltip(name)
            .with_size(THUMB.0, THUMB.1)
            .on_view(move || chosen.set(target.clone()));
        // The picture over its button: the button takes the press the
        // picture lets through, and the picture shows what it chooses.
        Box::new(
            Container::new()
                .with_no_padding()
                .with_size(THUMB.0, THUMB.1)
                .with_child(Box::new(
                    ui::widget::Stack::new()
                        .push(Box::new(choose))
                        .push(picture),
                )),
        )
    };
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
    let mut pictures = Row::new()
        .with_gap(4.0)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(thumb(&row.id, &named("canvas-layer-paint-name")));
    if let Some(mask) = &row.mask {
        pictures = pictures.push(thumb(&mask.id, &named("canvas-layer-paint-mask-name")));
    }
    pictures
}

fn layer_row(cx: &ViewCx<ArtworkResource>, tools: &Tools, row: &LayerRow) -> Element {
    let pictures = pictures(cx, tools, row);
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
    let rename = ArtworkResource::RENAME_LAYER.map({
        let id = row.id.clone();
        move |name: String| (id.clone(), name)
    });
    let name = cx.actions().bind(
        EditableLabel::new(&row.name)
            .with_font_size(14.0)
            .with_id(&format!("canvas-layer-name-{}", row.id))
            .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-layer-rename"))
            .renames_on_double_click()
            .filling(),
        rename,
    );
    let mut summary = Row::new()
        .with_gap(6.0)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(Badge::semantic_pill(
            row.medium.clone(),
            BadgeTone::Neutral,
        )));
    if row.opacity < 1.0 {
        summary = summary.push(caption(&percent(row.opacity)));
    }
    let text = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0)
        .push(Box::new(name))
        .push(Box::new(summary));
    let (eye, eye_name) = if row.visible {
        (Icon::Eye, named("canvas-layer-hide-name"))
    } else {
        (Icon::EyeOff, named("canvas-layer-show-name"))
    };
    let (lock, lock_name) = if row.locked {
        (Icon::Lock, named("canvas-layer-unlock-name"))
    } else {
        (Icon::LockOpen, named("canvas-layer-lock-name"))
    };
    let content = Row::new()
        .with_padding(6.0, 4.0)
        .with_gap(10.0)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(pictures))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_min_width(0.0)
                .with_child(Box::new(text)),
        ))
        .push(Box::new(icon_button(
            cx,
            eye,
            &eye_name,
            ArtworkResource::SET_LAYER_VISIBLE.with((row.id.clone(), !row.visible)),
        )))
        .push(Box::new(icon_button(
            cx,
            lock,
            &lock_name,
            ArtworkResource::SET_LAYER_LOCKED.with((row.id.clone(), !row.locked)),
        )));
    Box::new(
        ListRow::new(content)
            .on_raised()
            .choosing_any(
                &tools.layer,
                std::iter::once(row.id.clone())
                    .chain(row.mask.as_ref().map(|mask| mask.id.clone()))
                    .collect(),
            )
            .with_accessible_name(row.name.clone()),
    )
}

/// The chosen layer's opacity, medium and place among the layers.
fn chosen_layer(cx: &ViewCx<ArtworkResource>, id: &str) -> Element {
    let Some(row) = cx.read(|art| {
        art.and_then(|art| {
            rows(art)
                .into_iter()
                .find(|row| row.id == id || row.mask.as_ref().is_some_and(|mask| mask.id == id))
        })
    }) else {
        return Box::new(Spacer::fixed(0.0));
    };
    let opacity = {
        let id = row.id.clone();
        cx.value(move |art: &Artwork| art.layer(&id).map_or(1.0, |layer| layer.opacity))
    };
    let slider = cx.actions().bind(
        Slider::from_state(&opacity)
            .with_id("canvas-layer-opacity")
            .with_accessible_name(&ui::i18n::I18nManager::lookup_plain("canvas-layer-opacity"))
            .commits_on_release(),
        ArtworkResource::SET_LAYER_OPACITY.map({
            let id = row.id.clone();
            move |value: f64| (id.clone(), (value * 100.0).round() / 100.0)
        }),
    );
    let label = {
        Text::dynamic(move || {
            ui::i18n::I18nManager::format(
                "canvas-layer-opacity-value",
                &[("percent", percent(opacity.with(|value| *value)).into())],
            )
        })
        .with_role(TextRole::Caption)
    };
    let order = |icon, name: &str, to: &str| {
        icon_button(
            cx,
            icon,
            &ui::i18n::I18nManager::lookup_plain(name),
            ArtworkResource::MOVE_LAYER.with((row.id.clone(), to.to_owned())),
        )
    };
    let raise = order(Icon::ArrowUp, "canvas-layer-up", "forward");
    let lower = order(Icon::ArrowDown, "canvas-layer-down", "backward");
    let masking = mask_buttons(cx, &row);
    let remove = icon_button(
        cx,
        Icon::Trash2,
        &ui::i18n::I18nManager::format(
            "canvas-layer-remove-name",
            &[("name", row.name.clone().into())],
        ),
        ArtworkResource::REMOVE_LAYER.with(row.id.clone()),
    );
    let mut column = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_gap(0.0)
        .push(Box::new(
            Row::new()
                .with_gap(2.0)
                .no_wrap()
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(label))
                .push(Box::new(Spacer::new()))
                .push(masking)
                .push(Box::new(raise))
                .push(Box::new(lower))
                .push(Box::new(remove)),
        ))
        .push(Box::new(slider));
    if let Some(mask) = &row.mask {
        column = column.push(mask_line(cx, &row, mask));
    }
    Box::new(column)
}

/// Giving the chosen layer a mask, and clipping it to the layer below.
fn mask_buttons(cx: &ViewCx<ArtworkResource>, row: &LayerRow) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut buttons = Row::new()
        .with_gap(2.0)
        .no_wrap()
        .with_align_items(ui::taffy::AlignItems::Center);
    if row.mask.is_none() {
        buttons = buttons.push(Box::new(icon_button(
            cx,
            Icon::SquareDashed,
            &lookup("canvas-layer-mask-add"),
            ArtworkResource::SET_LAYER_MASK.with((row.id.clone(), "reveal".into())),
        )));
    }
    let mut clip = icon_button(
        cx,
        Icon::Paperclip,
        &lookup("canvas-layer-clip"),
        ArtworkResource::SET_LAYER_CLIP.with((row.id.clone(), !row.clip)),
    );
    if row.clip {
        clip = clip.primary();
    }
    Box::new(buttons.push(Box::new(clip)))
}

/// The chosen layer's mask: switch it off, invert it, remove it. It is
/// painted by choosing its picture in the layer's row.
fn mask_line(cx: &ViewCx<ArtworkResource>, row: &LayerRow, mask: &MaskRow) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let (eye, name) = if mask.visible {
        (Icon::Eye, "canvas-layer-mask-off")
    } else {
        (Icon::EyeOff, "canvas-layer-mask-on")
    };
    let mut invert = icon_button(
        cx,
        Icon::ArrowUpDown,
        &lookup("canvas-layer-mask-invert"),
        ArtworkResource::SET_MASK_INVERT.with((row.id.clone(), !mask.invert)),
    );
    if mask.invert {
        invert = invert.primary();
    }
    Box::new(
        Row::new()
            .with_gap(2.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(caption(&lookup("canvas-medium-mask")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(icon_button(
                cx,
                eye,
                &lookup(name),
                ArtworkResource::SET_LAYER_VISIBLE.with((mask.id.clone(), !mask.visible)),
            )))
            .push(Box::new(invert))
            .push(Box::new(icon_button(
                cx,
                Icon::X,
                &lookup("canvas-layer-mask-remove"),
                ArtworkResource::SET_LAYER_MASK.with((row.id.clone(), "remove".into())),
            ))),
    )
}

/// Adding a layer: a menu of the media to paint it in, adding the chosen
/// one above the chosen layer and naming it for its medium.
pub fn add_menu(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let media = cx.read(|art| {
        let mut media = vec![(
            String::new(),
            ui::i18n::I18nManager::lookup_plain("canvas-medium-plain"),
        )];
        if let Some(art) = art {
            media.extend(
                crate::media::available(art)
                    .into_iter()
                    .map(|medium| (medium.id, medium.label)),
            );
        }
        media
    });
    let state = LocalState::new(String::new());
    let mut menu = Dropdown::from_state(Vec::new(), &state)
        .as_menu(Icon::Plus)
        .with_id("canvas-add-layer")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain("canvas-add-layer"));
    menu.set_value_options(media.clone());
    let chosen = tools.layer.clone();
    let add = ArtworkResource::ADD_LAYER_NAMED.map(move |medium: String| {
        // "Oil, alla prima" names its layer "Oil".
        let label = media
            .iter()
            .find(|(id, _)| *id == medium)
            .map(|(_, label)| label.as_str())
            .unwrap_or_default();
        let name = label
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        // Named here and chosen now, so the new layer is the one drawn on
        // next even before the artwork has it; it goes above the chosen one.
        let random = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("layer-{}", random.get(..8).unwrap_or_default());
        let above = Some(chosen.with(Clone::clone)).filter(|above| !above.is_empty());
        chosen.set(id.clone());
        (id, name, medium, above)
    });
    Box::new(cx.actions().bind(menu, add))
}
