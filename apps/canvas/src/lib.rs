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
pub mod adjust;
pub mod author;
pub mod doc;
pub mod fonts;
pub mod gesture;
pub mod locales;
pub mod media;
pub mod model;
pub mod panels;
pub mod picture;
pub mod preview;
pub mod region;
pub mod render;
pub mod resource;
pub mod sample;
pub mod selection;
pub mod surface;
pub mod svg;
pub mod thumb;
pub mod tools;
pub mod view_ops;

pub use actions::CanvasAction;
pub use model::Artwork;
pub use resource::ArtworkResource;

use panels::pane::{Edge, Pane};
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
        let work = Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(
                Pane::new(panels::header::header(cx, &tools), Edge::Bottom).grouping(),
            ))
            .push(Box::new(
                ui::widget::Stack::new()
                    .push(canvas)
                    .push(panels::header::context_bar(cx, &tools)),
            ));
        let sidebar = {
            let tools = tools.clone();
            cx.switch(&tools.sidebar.clone(), move |cx, shown| {
                if *shown {
                    Box::new(Pane::new(sidebar(cx, &tools, &dock), Edge::Left).grouping())
                        as Element
                } else {
                    Box::new(Spacer::fixed(0.0))
                }
            })
        };
        Box::new(
            Row::new()
                .no_wrap()
                .with_gap(0.0)
                .with_fill_remaining_height()
                .push(Box::new(Pane::new(rail(&tools), Edge::Right)))
                .push(Box::new(work))
                .push(sidebar),
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

    /// Pictures open as a new artwork the size of the picture.
    const OPENS: &'static [&'static str] = &["image/png", "image/jpeg", "image/webp"];

    fn open_file(name: &str, bytes: &[u8]) -> Result<Invoke<ArtworkResource>, String> {
        let open = resource::open_picture(name, bytes)?;
        Invoke::apply(&open).map_err(|error| error.to_string())
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
pub(crate) const SIDEBAR: f32 = 288.0; // the swatches' width plus insets: a layout width, not spacing

/// The tools, top to bottom, each with its key, and at the foot the paint
/// the brush is loaded with.
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
            .with_child(Box::new(
                Column::new()
                    .with_vertical_padding(8.0)
                    .with_horizontal_padding(6.0, 6.0)
                    .with_gap(0.0)
                    .push(Box::new(rail))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(panels::well::Well::new(tools.clone()))),
            )),
    )
}

/// The inspector above; under it the layers or the history.
fn sidebar(cx: &ViewCx<ArtworkResource>, tools: &tools::Tools, dock: &LocalState<Dock>) -> Element {
    let inspector = Column::new()
        .with_intrinsic_height()
        .with_gap(12.0)
        .with_vertical_padding(14.0)
        .with_horizontal_padding(16.0, 14.0)
        .push(panels::inspector::panel(cx, tools));
    // The dock folds away to its tabs, giving the inspector the height.
    let open = LocalState::new(true);
    let docked = {
        let tools = tools.clone();
        let dock = dock.clone();
        cx.switch(&open, move |cx, open| {
            if !*open {
                return Box::new(Spacer::fixed(0.0)) as Element;
            }
            let tools = tools.clone();
            cx.switch(&dock, move |cx, dock| match dock {
                Dock::Layers => panels::layers::panel(cx, &tools),
                Dock::History => panels::history::panel(cx, &tools),
            })
        })
    };
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(SIDEBAR)
            .filling()
            .with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(0.0)
                    .push(Box::new(
                        Scrollable::new_vertical()
                            .fill_parent_height()
                            .with_child(Box::new(inspector)),
                    ))
                    .push(Box::new(ui::widget::Divider::horizontal()))
                    .push(dock_tabs(cx, tools, dock, &open))
                    .push(docked),
            )),
    )
}

/// The layers and history tabs, and beside the layers adding one.
fn dock_tabs(
    cx: &ViewCx<ArtworkResource>,
    tools: &tools::Tools,
    dock: &LocalState<Dock>,
    open: &LocalState<bool>,
) -> Element {
    let tab = |which: Dock, id: &str| {
        (
            which,
            ui::widget::toggle_group::Segment::new(ui::i18n::I18nManager::lookup_plain(id)),
        )
    };
    let tabs = ui::widget::ToggleGroup::from_state(
        vec![
            tab(Dock::Layers, "canvas-layers"),
            tab(Dock::History, "canvas-history"),
        ],
        dock,
    )
    .underline()
    .with_flexible_width()
    .with_id("canvas-dock");
    let tools = tools.clone();
    let action = cx.switch(dock, move |cx, dock| match dock {
        Dock::Layers => panels::layers::add_menu(cx, &tools),
        Dock::History => panels::history::checkpoint_button(cx),
    });
    Box::new(
        Row::new()
            .no_wrap()
            .with_padding(12.0, 4.0)
            .with_gap(4.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_min_width(0.0)
                    .with_child(Box::new(tabs)),
            ))
            .push(action)
            .push(fold(cx, open)),
    )
}

/// Folding the dock away to its tabs, or opening it again.
fn fold(cx: &ViewCx<ArtworkResource>, open: &LocalState<bool>) -> Element {
    let flip = open.clone();
    cx.switch(open, move |_, shown| {
        let (icon, id) = if *shown {
            (ui::icons::Icon::ChevronDown, "canvas-dock-fold")
        } else {
            (ui::icons::Icon::ChevronUp, "canvas-dock-unfold")
        };
        let name = ui::i18n::I18nManager::lookup_plain(id);
        let flip = flip.clone();
        Box::new(
            Button::new(id, "", None)
                .with_icon(icon)
                .ghost()
                .with_accessible_name(&name)
                .tooltip(name.clone())
                .with_size(panels::ICON_BUTTON, panels::ICON_BUTTON)
                .on_view(move || flip.set(!flip.with(|open| *open))),
        )
    })
}

ui::dev_reload_entry!(App);
