//! Canvas: vector drawing and painting in real media, as fluent for an agent
//! as for a person.
//!
//! The artwork is layers of marks, each a vector object with a stable id. A
//! plain layer draws its marks as crisp vector art; an oil or watercolour
//! layer paints them with a simulation of that medium, re-simulated from the
//! marks on every machine. Everything a person does with the pointer is one
//! of the artwork's actions, and `look` lets a caller with no screen see the
//! result.

/// The application id.
pub const APP_ID: &str = "so.tilo.canvas";
/// The window and launcher title.
pub const APP_TITLE: &str = "Canvas";

pub mod actions;
pub mod doc;
pub mod fonts;
pub mod gesture;
pub mod locales;
pub mod media;
pub mod model;
pub mod render;
pub mod resource;
pub mod sample;
pub mod surface;
pub mod svg;
pub mod view_ops;

pub use actions::CanvasAction;
pub use model::Artwork;
pub use resource::ArtworkResource;

use ui::prelude::*;

pub struct App;

impl ResourceApp for App {
    type Resource = ArtworkResource;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<ArtworkResource>) -> Element {
        locales::install();
        let tools = surface::Tools::default();
        let canvas = cx.surface(surface::CanvasSurface::new(tools.clone()));

        let chosen = tools.tool.clone();
        let rail = ui::widget::ToggleGroup::new(
            surface::Tool::ALL
                .iter()
                .map(|tool| {
                    ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(
                        tool.label(),
                    ))
                })
                .collect(),
            1,
        )
        .on_view("Tool", move |index| {
            chosen.set(surface::Tool::ALL.get(index).copied().unwrap_or_default());
        });

        let layers = cx.value(|art: &Artwork| {
            art.layers
                .iter()
                .rev()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>()
        });
        let target = tools.layer.clone();
        // A new layer is where the next mark goes, so the choice follows the
        // top layer whenever the layers change.
        let layer_choice = cx.switch(&layers, move |_cx, ids| {
            if let Some(top) = ids.first() {
                target.set(top.clone());
            }
            Box::new(labeled_field(
                "canvas-layer",
                Dropdown::from_state(ids.clone(), &target).with_id("canvas-layer"),
            ))
        });
        let summary = layer_panel(cx);
        let inspector = inspector(cx, &tools, layer_choice, summary);
        Box::new(
            Column::new()
                .with_no_padding()
                .with_fill_remaining_height()
                .push(Box::new(
                    Row::new()
                        .with_padding(12.0, 8.0)
                        .with_gap(12.0)
                        .push(Box::new(
                            Text::new("canvas-title").with_role(TextRole::Title),
                        ))
                        .push(Box::new(rail)),
                ))
                .push(Box::new(
                    Row::new()
                        .with_fill_remaining_height()
                        .push(canvas)
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_width(340.0)
                                .with_child(Box::new(
                                    Column::new()
                                        .with_no_padding()
                                        .with_fill_remaining_height()
                                        .push(Box::new(
                                            Scrollable::new_vertical()
                                                .with_semantic_key("canvas-inspector")
                                                .fill_parent_height()
                                                .with_child(Box::new(inspector)),
                                        )),
                                )),
                        )),
                )),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &["landscape", "close-up", "selection", "turned", "points"]
    }

    /// The screenshots paint their samples through the same actions an
    /// agent would; a plain `--shots` shows the empty sheet a new artwork
    /// is.
    fn scenario(name: Option<&str>) -> Vec<Invoke<ArtworkResource>> {
        let actions = match name {
            Some("landscape" | "close-up") => sample::landscape(),
            Some("selection" | "turned" | "points") => sample::shapes(),
            _ => return Vec::new(),
        };
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn label(art: &Artwork) -> String {
        if art.title.is_empty() {
            APP_TITLE.to_owned()
        } else {
            art.title.clone()
        }
    }
}

/// A layer as its row shows it.
#[derive(Clone, Default, PartialEq)]
struct LayerRow {
    id: String,
    name: String,
    medium: String,
    visible: bool,
    marks: usize,
}

/// One row per layer, top first: its name, medium and marks, and controls
/// bound to the layer actions, rebuilt whenever the layers change.
fn layer_panel(cx: &ViewCx<ArtworkResource>) -> Element {
    let rows = cx.value(|art: &Artwork| {
        art.layers
            .iter()
            .rev()
            .map(|layer| LayerRow {
                id: layer.id.clone(),
                name: layer.name.clone(),
                medium: if layer.medium.is_empty() {
                    "plain".into()
                } else {
                    layer.medium.clone()
                },
                visible: layer.visible,
                marks: art.marks_of(&layer.id).count(),
            })
            .collect::<Vec<_>>()
    });
    cx.switch(&rows, |cx, rows| {
        let mut list = Column::new()
            .with_no_padding()
            .with_gap(6.0)
            .with_id("canvas-layers");
        for row in rows {
            list = list.push(layer_row(cx, row));
        }
        Box::new(list)
    })
}

/// A layer's name, medium and marks over its controls; each control's
/// accessible name says which layer it acts on.
fn layer_row(cx: &ViewCx<ArtworkResource>, row: &LayerRow) -> Element {
    let named = |id: &str| {
        ui::i18n::I18nManager::format(
            id,
            &[
                ("name", row.name.clone().into()),
                ("visible", if row.visible { "yes" } else { "no" }.into()),
            ],
        )
    };
    let visibility = if row.visible {
        "canvas-layer-hide"
    } else {
        "canvas-layer-show"
    };
    let buttons = [
        (
            visibility,
            "canvas-layer-visibility-name",
            ArtworkResource::SET_LAYER_VISIBLE.with((row.id.clone(), !row.visible)),
        ),
        (
            "canvas-layer-up",
            "canvas-layer-up-name",
            ArtworkResource::MOVE_LAYER.with((row.id.clone(), "forward".into())),
        ),
        (
            "canvas-layer-down",
            "canvas-layer-down-name",
            ArtworkResource::MOVE_LAYER.with((row.id.clone(), "backward".into())),
        ),
        (
            "canvas-layer-remove",
            "canvas-layer-remove-name",
            ArtworkResource::REMOVE_LAYER.with(row.id.clone()),
        ),
    ];
    let mut controls = Row::new().with_gap(4.0);
    for (label, name, invoke) in buttons {
        controls = controls.push(Box::new(
            cx.button(label, invoke).with_accessible_name(&named(name)),
        ));
    }
    let label = ui::i18n::I18nManager::format(
        "canvas-layer-row",
        &[
            ("name", row.name.clone().into()),
            ("medium", row.medium.clone().into()),
            ("marks", row.marks.into()),
        ],
    );
    let title = Text::new(&label);
    let title = if row.visible { title } else { title.dimmed() };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(2.0)
            .push(Box::new(title))
            .push(Box::new(controls)),
    )
}

/// The inspector: colour, brush, layers and adding layers.
fn inspector(
    cx: &ViewCx<ArtworkResource>,
    tools: &surface::Tools,
    layer_choice: Element,
    summary: Element,
) -> Column {
    let add = |label: &str, name: &str, medium: &str| {
        cx.button(
            label,
            ArtworkResource::ADD_LAYER.with((name.to_owned(), medium.to_owned())),
        )
    };
    Column::new()
        .with_gap(10.0)
        .with_vertical_padding(12.0)
        .with_horizontal_padding(12.0, 12.0)
        .push(Box::new(
            Text::new("canvas-colour").with_role(TextRole::Label),
        ))
        .push(Box::new(ui::widget::ColorPicker::from_state(&tools.colour)))
        .push(Box::new(labeled_field(
            "canvas-brush-size",
            Slider::from_state(&tools.size).with_id("canvas-brush-size"),
        )))
        .push(layer_choice)
        .push(summary)
        .push(Box::new(add("canvas-add-plain-layer", "Ink", "")))
        .push(Box::new(add("canvas-add-oil-layer", "Oil", "oil")))
        .push(Box::new(add(
            "canvas-add-watercolour-layer",
            "Watercolour",
            "watercolour",
        )))
}

ui::dev_reload_entry!(App);
