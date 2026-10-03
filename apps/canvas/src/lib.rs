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

use ui::icons::Icon;
use ui::prelude::*;
use ui::widget::{ControlSize, Section, ToggleGroup, toggle_group::Segment};

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
        let rail = ToggleGroup::new(
            surface::Tool::ALL
                .iter()
                .map(|tool| Segment::new(ui::i18n::I18nManager::lookup_plain(tool.label())))
                .collect(),
            1,
        )
        .control_size(ControlSize::Small)
        .on_view("Tool", move |index| {
            chosen.set(surface::Tool::ALL.get(index).copied().unwrap_or_default());
        });

        let inspector = inspector(cx, &tools);
        Box::new(
            NavShell::new()
                .with_id("canvas-shell")
                .content(canvas)
                .content_header(
                    "",
                    vec![Box::new(rail), Box::new(ui::widget::history_button())],
                )
                .inspector(Box::new(
                    Scrollable::new_vertical()
                        .with_semantic_key("canvas-inspector")
                        .fill_parent_height()
                        .with_child(Box::new(inspector)),
                )),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &["landscape"]
    }

    /// The landscape screenshot paints the sample through the same actions
    /// an agent would; a plain `--shots` shows the empty sheet a new
    /// artwork is.
    fn scenario(name: Option<&str>) -> Vec<Invoke<ArtworkResource>> {
        if name != Some("landscape") {
            return Vec::new();
        }
        sample::landscape()
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

/// A medium as a person reads it: the catalogue's name for a medium the app
/// ships, otherwise the name it was stored under.
fn medium_name(medium: &str) -> String {
    let key = format!(
        "canvas-medium-{}",
        if medium.is_empty() { "ink" } else { medium }
    );
    if ui::i18n::I18nManager::has_message(&key) {
        ui::i18n::I18nManager::lookup_plain(&key)
    } else {
        medium.to_owned()
    }
}

/// A layer's name over what it holds, with its controls beside the name; each
/// control's accessible name says which layer it acts on.
fn layer_row(cx: &ViewCx<ArtworkResource>, row: &LayerRow) -> Element {
    let named = |id: &str| ui::i18n::I18nManager::format(id, &[("name", row.name.clone().into())]);
    let visibility = if row.visible {
        ("canvas-layer-hide", "canvas-layer-hide-name")
    } else {
        ("canvas-layer-show", "canvas-layer-show-name")
    };
    let actions = cx.actions();
    let button = |(id, name): (&str, &str), icon: Icon, invoke: Invoke<ArtworkResource>| {
        actions.bind_invocation(
            Button::icon_only(&format!("{id}-{}", row.id), icon, &named(name))
                .control_size(ControlSize::Small),
            invoke,
        )
    };
    let controls = [
        Box::new(button(
            visibility,
            Icon::Eye,
            ArtworkResource::SET_LAYER_VISIBLE.with((row.id.clone(), !row.visible)),
        )) as Element,
        Box::new(button(
            ("canvas-layer-up", "canvas-layer-up-name"),
            Icon::ArrowUp,
            ArtworkResource::MOVE_LAYER.with((row.id.clone(), "forward".into())),
        )),
        Box::new(button(
            ("canvas-layer-down", "canvas-layer-down-name"),
            Icon::ArrowDown,
            ArtworkResource::MOVE_LAYER.with((row.id.clone(), "backward".into())),
        )),
        Box::new(button(
            ("canvas-layer-remove", "canvas-layer-remove-name"),
            Icon::Trash2,
            ArtworkResource::REMOVE_LAYER.with(row.id.clone()),
        )),
    ];
    let summary = ui::i18n::I18nManager::format(
        "canvas-layer-summary",
        &[
            ("medium", row.medium.clone().into()),
            ("marks", row.marks.into()),
        ],
    );
    let name = Text::new(&row.name).with_role(TextRole::Label);
    let summary = Text::new(&summary).with_role(TextRole::Caption);
    let (name, summary) = if row.visible {
        (name, summary)
    } else {
        (name.dimmed(), summary.dimmed())
    };
    let line = controls.into_iter().fold(
        Row::new()
            .no_wrap()
            .push(Box::new(name))
            .push(Box::new(Spacer::new())),
        Row::push,
    );
    // The name and what it holds belong together: no gap between them.
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(ui::tokens::Spacing::BASE.s0)
            .push(Box::new(line))
            .push(Box::new(summary)),
    )
}

/// The layers: where the next mark goes, each layer with its controls, and
/// adding one. Rebuilt whenever a layer changes; the drawing layer follows
/// the top layer when layers are added, removed or moved.
fn layers_section(cx: &ViewCx<ArtworkResource>, tools: &surface::Tools) -> Element {
    let rows = cx.value(|art: &Artwork| {
        art.layers
            .iter()
            .rev()
            .map(|layer| LayerRow {
                id: layer.id.clone(),
                name: layer.name.clone(),
                medium: medium_name(&layer.medium),
                visible: layer.visible,
                marks: art.marks_of(&layer.id).count(),
            })
            .collect::<Vec<_>>()
    });
    let target = tools.layer.clone();
    let known = std::cell::RefCell::new(Vec::<String>::new());
    cx.switch(&rows, move |cx, rows| {
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        if *known.borrow() != ids {
            if let Some(top) = ids.first() {
                target.set(top.clone());
            }
            *known.borrow_mut() = ids;
        }
        let mut choice = Dropdown::from_state(Vec::new(), &target)
            .with_id("canvas-layer")
            .caption(ui::i18n::I18nManager::lookup_plain("canvas-layer"));
        choice.set_value_options(
            rows.iter()
                .map(|row| (row.id.clone(), row.name.clone()))
                .collect(),
        );
        let add = |label: &str, name: &str, medium: &str| -> Element {
            Box::new(
                cx.button(
                    label,
                    ArtworkResource::ADD_LAYER.with((name.to_owned(), medium.to_owned())),
                )
                .fill_width(),
            )
        };
        let mut items: Vec<Element> = vec![Box::new(choice)];
        for row in rows {
            items.push(layer_row(cx, row));
        }
        items.push(add("canvas-add-plain-layer", "Ink", ""));
        items.push(add("canvas-add-oil-layer", "Oil", "oil"));
        items.push(add(
            "canvas-add-watercolour-layer",
            "Watercolour",
            "watercolour",
        ));
        Box::new(Section::titled(
            ui::i18n::I18nManager::lookup_plain("canvas-layers"),
            items,
        ))
    })
}

/// The inspector: colour, brush and layers, each a section on the ground.
fn inspector(cx: &ViewCx<ArtworkResource>, tools: &surface::Tools) -> Column {
    let section = |key: &str, items: Vec<Element>| -> Element {
        Box::new(Section::titled(
            ui::i18n::I18nManager::lookup_plain(key),
            items,
        ))
    };
    // The colour picker is 260 wide; a gutter of `s3` fits it in the inspector.
    let gutter = ui::tokens::Spacing::BASE.s3;
    Column::new()
        .with_intrinsic_height()
        .with_horizontal_padding(gutter, gutter)
        .push(section(
            "canvas-colour",
            vec![Box::new(ui::widget::ColorPicker::from_state(&tools.colour))],
        ))
        .push(section(
            "canvas-brush",
            vec![Box::new(
                Slider::from_state(&tools.size)
                    .with_id("canvas-brush-size")
                    .with_accessible_name(&ui::i18n::I18nManager::lookup_plain(
                        "canvas-brush-size",
                    )),
            )],
        ))
        .push(layers_section(cx, tools))
}

ui::dev_reload_entry!(App);
