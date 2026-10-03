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
pub mod panels;
pub mod preview;
pub mod render;
pub mod resource;
pub mod sample;
pub mod surface;
pub mod svg;
pub mod thumb;
pub mod tools;
pub mod view_ops;

pub use actions::CanvasAction;
pub use model::Artwork;
pub use resource::ArtworkResource;

use ui::LocalState;
use ui::prelude::*;

pub struct App;

impl ResourceApp for App {
    type Resource = ArtworkResource;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<ArtworkResource>) -> Element {
        locales::install();
        let tools = tools::Tools::default();
        let canvas = cx.surface(surface::CanvasSurface::new(tools.clone()));
        let canvas: Element = Box::new(preview::VersionPreview::new(canvas, tools.preview.clone()));
        let dock = LocalState::new(Dock::Layers);
        let staged = dock.clone();
        cx.on_stage(move |scenario| {
            if scenario == Some("history") {
                staged.set(Dock::History);
            }
        });
        Box::new(
            Column::new()
                .with_no_padding()
                .with_fill_remaining_height()
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_fill_remaining_height()
                        .push(rail(&tools))
                        .push(canvas)
                        .push(sidebar(cx, &tools, &dock)),
                )),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "landscape",
            "close-up",
            "selection",
            "turned",
            "points",
            "history",
        ]
    }

    /// The screenshots paint their samples through the same actions an
    /// agent would; a plain `--shots` shows the empty sheet a new artwork
    /// is.
    fn scenario(name: Option<&str>) -> Vec<Invoke<ArtworkResource>> {
        let actions = match name {
            Some("landscape" | "close-up" | "history") => sample::landscape(),
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

/// What the lower half of the sidebar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Dock {
    #[default]
    Layers,
    History,
}

/// The tool rail's width.
const RAIL: f32 = 56.0; // a pointer target and its inset, laid out before a theme is known
/// The sidebar's width.
const SIDEBAR: f32 = 300.0; // the colour picker's and swatches' width plus insets: a layout width, not spacing
/// The height of the layers and history dock under the inspector.
const DOCK: f32 = 320.0; // a share of the sidebar's height for the dock: a layout size, not spacing

/// The tools, top to bottom, each with its key.
fn rail(tools: &tools::Tools) -> Element {
    let choices = tools::Tool::ALL
        .iter()
        .map(|tool| {
            let segment = ui::widget::toggle_group::Segment::new(
                ui::i18n::I18nManager::lookup_plain(tool.label()),
            )
            .with_icon(tool.icon())
            .with_hint(tool.shortcut().to_uppercase());
            (*tool, segment)
        })
        .collect();
    let rail = ui::widget::ToggleGroup::from_state(choices, &tools.tool)
        .vertical()
        .on_view("Tool", |_| {});
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(RAIL)
            .filling()
            .with_background()
            .with_child(Box::new(
                Column::new()
                    .with_vertical_padding(8.0)
                    .with_horizontal_padding(6.0, 6.0)
                    .push(Box::new(rail)),
            )),
    )
}

/// The inspector above, the layers or the history below.
fn sidebar(cx: &ViewCx<ArtworkResource>, tools: &tools::Tools, dock: &LocalState<Dock>) -> Element {
    let inspector = Column::new()
        .with_intrinsic_height()
        .with_gap(12.0)
        .with_vertical_padding(12.0)
        .with_horizontal_padding(12.0, 12.0)
        .push(panels::inspector::panel(cx, tools));
    let tabs = ui::widget::ToggleGroup::from_state(
        vec![
            (
                Dock::Layers,
                ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(
                    "canvas-layers",
                )),
            ),
            (
                Dock::History,
                ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(
                    "canvas-history",
                )),
            ),
        ],
        dock,
    )
    .underline()
    .with_flexible_width()
    .with_id("canvas-dock");
    let tools_for_dock = tools.clone();
    let docked = cx.switch(dock, move |cx, dock| match dock {
        Dock::Layers => panels::layers::panel(cx, &tools_for_dock),
        Dock::History => panels::history::panel(cx, &tools_for_dock),
    });
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(SIDEBAR)
            .filling()
            .with_background()
            .with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .push(Box::new(
                        Scrollable::new_vertical()
                            .fill_parent_height()
                            .with_child(Box::new(inspector)),
                    ))
                    .push(Box::new(ui::widget::Divider::horizontal()))
                    .push(Box::new(tabs))
                    .push(Box::new(
                        Container::new()
                            .with_padding(10.0)
                            .with_height(DOCK)
                            .with_child(Box::new(
                                Scrollable::new_vertical().with_child(Box::new(
                                    Column::new()
                                        .with_intrinsic_height()
                                        .with_vertical_padding(0.0)
                                        .with_horizontal_padding(2.0, 10.0)
                                        .push(docked),
                                )),
                            )),
                    )),
            )),
    )
}

ui::dev_reload_entry!(App);
