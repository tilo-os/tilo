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
#[derive(Clone, Default, PartialEq)]
struct LayerRow {
    id: String,
    name: String,
    medium: String,
    visible: bool,
    locked: bool,
    opacity: f64,
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

fn rows(art: &Artwork) -> Vec<LayerRow> {
    art.layers
        .iter()
        .rev()
        .map(|layer| LayerRow {
            id: layer.id.clone(),
            name: layer.name.clone(),
            medium: engine_label(art, &layer.medium),
            visible: layer.visible,
            locked: layer.locked,
            opacity: layer.opacity,
        })
        .collect()
}

/// The panel.
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
    let adding = LocalState::new("oil".to_owned());
    let add = add_layer(cx, &adding, tools);
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0)
            .push(list)
            .push(footer)
            .push(add),
    )
}

/// New marks go on the chosen layer; a layer added since (by anyone) is
/// where the next mark goes, and a removed one's choice falls to the top.
fn follow_new_layers(known: &RefCell<Vec<String>>, rows: &[LayerRow], target: &LocalState<String>) {
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
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

fn layer_row(cx: &ViewCx<ArtworkResource>, tools: &Tools, row: &LayerRow) -> Element {
    let thumb = cx.surface(crate::thumb::Thumb::new(
        tools.renderer.clone(),
        Some(row.id.clone()),
        44.0,
        33.0,
    ));
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
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
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
        .push(thumb)
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
            .choosing(&tools.layer, row.id.clone())
            .with_accessible_name(row.name.clone()),
    )
}

/// The chosen layer's opacity, medium and place among the layers.
fn chosen_layer(cx: &ViewCx<ArtworkResource>, id: &str) -> Element {
    let Some(row) =
        cx.read(|art| art.and_then(|art| rows(art).into_iter().find(|row| row.id == id)))
    else {
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
    let remove = icon_button(
        cx,
        Icon::Trash2,
        &ui::i18n::I18nManager::format(
            "canvas-layer-remove-name",
            &[("name", row.name.clone().into())],
        ),
        ArtworkResource::REMOVE_LAYER.with(row.id.clone()),
    );
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(
                Row::new()
                    .with_gap(2.0)
                    .no_wrap()
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .push(Box::new(label))
                    .push(super::filler())
                    .push(Box::new(raise))
                    .push(Box::new(lower))
                    .push(Box::new(remove)),
            ))
            .push(Box::new(slider)),
    )
}

/// Adding a layer: the medium to paint it in, and the button that adds it
/// above the chosen layer.
fn add_layer(cx: &ViewCx<ArtworkResource>, adding: &LocalState<String>, tools: &Tools) -> Element {
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
    let mut choice = Dropdown::from_state(Vec::new(), adding)
        .with_id("canvas-new-layer-medium")
        .with_accessible_name(ui::i18n::I18nManager::lookup_plain(
            "canvas-new-layer-medium",
        ));
    choice.set_value_options(media);
    let target = tools.layer.clone();
    let button = cx.switch(adding, move |cx, medium| {
        let medium = medium.clone();
        cx.switch(&target, move |cx, above| {
            let label = cx.read(|art| {
                art.and_then(|art| crate::media::resolve(art, &medium))
                    .filter(|_| !medium.is_empty())
                    .map_or_else(
                        || ui::i18n::I18nManager::lookup_plain("canvas-medium-plain"),
                        |medium| medium.label,
                    )
            });
            // "Oil, alla prima" names its layer "Oil".
            let name = label
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            let invoke = if above.is_empty() {
                ArtworkResource::ADD_LAYER.with((name, medium.clone()))
            } else {
                ArtworkResource::ADD_LAYER_ABOVE.with((name, medium.clone(), above.clone()))
            };
            Box::new(cx.button("canvas-add-layer", invoke).with_icon(Icon::Plus))
        })
    });
    Box::new(
        Row::new()
            .with_gap(6.0)
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(choice)),
            ))
            .push(button),
    )
}
