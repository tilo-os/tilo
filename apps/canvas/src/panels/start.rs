//! Starting: the welcome over a blank artwork, saying what can be done
//! here (by a person and by an agent), and the new-artwork dialog, a size
//! and a ground with its medium, each shown as the sheet it makes.

use std::collections::HashMap;

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use ui::widget::{AppModel, ListRow, ModelPath, Widget};
use ui::{PresenceCtx, TextRenderer, Theme};

use super::caption;
use crate::actions::{CanvasAction, NewArtwork};
use crate::document::{self, Recent, SIZES, STARTS};
use crate::model::{Artwork, Ground};
use crate::render::{Options, Renderer};
use crate::resource::ArtworkResource;
use crate::tools::{FileRequest, Sheet, Tools, ViewRequest};

/// The size chosen when it is none of the presets.
pub const CUSTOM: &str = "custom";

/// The new-artwork dialog's choices, kept while the window is open.
#[derive(Clone)]
pub struct Choices {
    /// A size's id, or [`CUSTOM`].
    pub size: LocalState<String>,
    pub landscape: LocalState<bool>,
    pub width: LocalState<String>,
    pub height: LocalState<String>,
    /// A start's id.
    pub start: LocalState<String>,
    pub title: LocalState<String>,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            size: LocalState::new("screen".to_owned()),
            landscape: LocalState::new(true),
            width: LocalState::new("1600".to_owned()),
            height: LocalState::new("1200".to_owned()),
            start: LocalState::new("paper".to_owned()),
            title: LocalState::new(String::new()),
        }
    }
}

impl Choices {
    /// The width and height chosen; `None` while a custom side is not a
    /// number in range.
    pub fn dimensions(&self) -> Option<(f64, f64)> {
        let chosen = self.size.get();
        if chosen == CUSTOM {
            let parse = |text: String| {
                text.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|value| (16.0..=crate::model::MAX_SIDE).contains(value))
            };
            return Some((parse(self.width.get())?, parse(self.height.get())?));
        }
        let preset = document::size(&chosen)?;
        Some(turned(preset.width, preset.height, self.landscape.get()))
    }

    /// The ground chosen.
    pub fn ground(&self) -> Ground {
        let start = document::start(&self.start.get()).unwrap_or(STARTS[0]);
        Ground {
            texture: start.texture.into(),
            colour: start.colour.into(),
        }
    }

    /// The action starting the artwork chosen.
    pub fn action(&self) -> Option<CanvasAction> {
        let (width, height) = self.dimensions()?;
        let title = self.title.get().trim().to_owned();
        Some(CanvasAction::NewArtwork(NewArtwork {
            title: Some(title).filter(|title| !title.is_empty()),
            width: Some(width),
            height: Some(height),
            start: Some(self.start.get()),
            ..NewArtwork::default()
        }))
    }
}

/// `width` by `height` turned to stand `landscape` or upright; a square
/// stays as it is.
fn turned(width: f64, height: f64, landscape: bool) -> (f64, f64) {
    let (long, short) = (width.max(height), width.min(height));
    if (width - height).abs() < f64::EPSILON {
        (width, height)
    } else if landscape {
        (long, short)
    } else {
        (short, long)
    }
}

/// Start the artwork the dialog is set to, and show it whole.
pub fn create(dispatch: &Dispatcher<ArtworkResource>, tools: &Tools) -> bool {
    let Some(action) = tools.new.action() else {
        return false;
    };
    dispatch.apply(&action);
    tools.file.set(None);
    tools.selection.set(Vec::new());
    tools.layer.set("layer-1".to_owned());
    tools.preview.set(None);
    tools.notice.set(None);
    tools.view.set(Some(ViewRequest::Fit));
    tools.new.title.set(String::new());
    tools.welcomed.set(true);
    tools.sheet.set(None);
    true
}

/// A sheet of the artwork's proportions in a ground, as a choice shows it.
pub struct Swatch {
    rect: Rect,
    side: f32,
    sheet: Box<dyn Fn() -> (f64, f64)>,
    ground: Box<dyn Fn() -> Ground>,
    renderer: Renderer,
}

impl Swatch {
    /// A swatch `side` points square, drawing a sheet of the proportions
    /// `sheet` gives, in the ground `ground` gives.
    pub fn new(
        side: f32,
        sheet: impl Fn() -> (f64, f64) + 'static,
        ground: impl Fn() -> Ground + 'static,
    ) -> Self {
        Self {
            rect: Rect::ZERO,
            side,
            sheet: Box::new(sheet),
            ground: Box::new(ground),
            renderer: Renderer::default(),
        }
    }
}

/// The long side of the small artwork a swatch draws, in units: enough
/// for the ground's tooth to read, few enough cells to make at once.
const SWATCH_UNITS: f64 = 240.0;

impl Widget for Swatch {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::length(self.side),
                height: taffy::Dimension::length(self.side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "kurbo's Affine composition is float geometry"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let (width, height) = (self.sheet)();
        if width <= 0.0 || height <= 0.0 || self.rect.width() < 4.0 {
            return;
        }
        let fit = SWATCH_UNITS / width.max(height);
        let art = Artwork {
            width: (width * fit).round().max(1.0),
            height: (height * fit).round().max(1.0),
            ground: (self.ground)(),
            ..Artwork::default()
        };
        let room = self.rect.inset(-3.0);
        let scale = (room.width() / art.width).min(room.height() / art.height);
        let origin = Point::new(
            room.center().x - art.width * scale / 2.0,
            room.center().y - art.height * scale / 2.0,
        );
        let to_screen = transform * Affine::translate(origin.to_vec2()) * Affine::scale(scale);
        self.renderer
            .draw(&art, scene, to_screen, text, &Options::default());
        let edge = Rect::from_origin_size(origin, (art.width * scale, art.height * scale));
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(theme.bg_primary),
            None,
            &RoundedRect::from_rect(edge, 1.0),
        );
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn name(&self) -> &'static str {
        "Swatch"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// A choice tile: a swatch above its name and a detail, chosen into
/// `state`.
fn tile(
    width: f32,
    state: &LocalState<String>,
    value: &str,
    id: &str,
    swatch: Swatch,
    name: &str,
    detail: impl Fn() -> String + 'static,
) -> Element {
    let said = format!("{name}, {}", detail());
    let text = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(0.0)
        .push(Box::new(Text::new(name).with_role(TextRole::Label)))
        .push(Box::new(
            Text::dynamic(detail).with_role(TextRole::Caption).dimmed(),
        ));
    let content = Row::new()
        .no_wrap()
        .with_padding(6.0, 6.0)
        .with_gap(8.0)
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(swatch))
        .push(Box::new(text));
    Box::new(
        Container::new()
            .with_no_padding()
            .with_width(width)
            .with_child(Box::new(
                ListRow::new(content)
                    .choosing(state, value.to_owned())
                    .with_id(id)
                    .with_accessible_name(said),
            )),
    )
}

/// A size tile's width: four to a row in the dialog.
const SIZE_TILE: f32 = 150.0; // a quarter of the dialog's body: a layout width, not spacing
/// A ground tile's width: three to a row in the dialog.
const START_TILE: f32 = 202.0; // a third of the dialog's body: a layout width, not spacing
/// A swatch's side in a tile.
const SWATCH: f32 = 40.0; // a thumbnail's side: a layout size, not spacing

fn rows(tiles: Vec<Element>, per_row: usize) -> Element {
    let mut grid = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(6.0);
    let mut tiles = tiles.into_iter().peekable();
    while tiles.peek().is_some() {
        let mut row = Row::new().no_wrap().with_gap(6.0);
        for tile in tiles.by_ref().take(per_row) {
            row = row.push(tile);
        }
        grid = grid.push(Box::new(row));
    }
    Box::new(grid)
}

/// "1920 × 1080".
fn sides(width: f64, height: f64) -> String {
    format!("{width:.0} × {height:.0}")
}

/// The size tiles: the presets turned as the dialog says, and any size.
fn size_tiles(choices: &Choices) -> Vec<Element> {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let mut sizes: Vec<Element> = SIZES
        .iter()
        .map(|size| {
            let (width, height) = (size.width, size.height);
            let landscape = choices.landscape.clone();
            let ground = choices.clone();
            let turned = move || turned(width, height, landscape.get());
            let turned = std::rc::Rc::new(turned);
            let shown = std::rc::Rc::clone(&turned);
            tile(
                SIZE_TILE,
                &choices.size,
                size.id,
                &format!("canvas-new-size-{}", size.id),
                Swatch::new(SWATCH, move || turned(), move || ground.ground()),
                &lookup(size.label),
                move || {
                    let (width, height) = shown();
                    sides(width, height)
                },
            )
        })
        .collect();
    {
        let custom = choices.clone();
        let ground = choices.clone();
        sizes.push(tile(
            SIZE_TILE,
            &choices.size,
            CUSTOM,
            "canvas-new-size-custom",
            Swatch::new(
                SWATCH,
                move || {
                    let side = |text: String| text.trim().parse::<f64>().unwrap_or(1.0).max(1.0);
                    (side(custom.width.get()), side(custom.height.get()))
                },
                move || ground.ground(),
            ),
            &lookup("canvas-size-custom"),
            || ui::i18n::I18nManager::lookup_plain("canvas-size-custom-detail"),
        ));
    }
    sizes
}

/// The tiles of the grounds with the medium of their first layer.
fn start_tiles(choices: &Choices) -> Vec<Element> {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let starts: Vec<Element> = STARTS
        .iter()
        .map(|start| {
            let ground = Ground {
                texture: start.texture.into(),
                colour: start.colour.into(),
            };
            let medium = if start.medium.is_empty() {
                lookup("canvas-medium-plain")
            } else {
                lookup(&format!("canvas-medium-{}", start.medium))
            };
            let sheet = choices.clone();
            tile(
                START_TILE,
                &choices.start,
                start.id,
                &format!("canvas-new-start-{}", start.id),
                Swatch::new(
                    SWATCH,
                    move || sheet.dimensions().unwrap_or((4.0, 3.0)),
                    move || ground.clone(),
                ),
                &lookup(start.label),
                move || medium.clone(),
            )
        })
        .collect();
    starts
}

/// Portrait or landscape, and for any size its sides.
fn orientation(cx: &ViewCx<ArtworkResource>, choices: &Choices) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let orientation = ui::widget::ToggleGroup::from_state(
        vec![
            (
                false,
                ui::widget::toggle_group::Segment::new(lookup("canvas-portrait")),
            ),
            (
                true,
                ui::widget::toggle_group::Segment::new(lookup("canvas-landscape")),
            ),
        ],
        &choices.landscape,
    )
    .with_id("canvas-new-orientation");
    let custom = {
        let choices = choices.clone();
        cx.switch(&choices.size.clone(), move |_, size| {
            if size != CUSTOM {
                return Box::new(Spacer::fixed(0.0)) as Element;
            }
            let width = TextInput::from_state(&choices.width)
                .with_id("canvas-new-width")
                .with_accessible_name(lookup("canvas-artwork-width"));
            let height = TextInput::from_state(&choices.height)
                .with_id("canvas-new-height")
                .with_accessible_name(lookup("canvas-artwork-height"));
            Box::new(
                Row::new()
                    .no_wrap()
                    .with_gap(8.0)
                    .with_align_items(ui::taffy::AlignItems::Center)
                    .push(caption(&lookup("canvas-artwork-width")))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(SIDE_FIELD)
                            .with_child(Box::new(width)),
                    ))
                    .push(caption(&lookup("canvas-artwork-height")))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(SIDE_FIELD)
                            .with_child(Box::new(height)),
                    ))
                    .push(caption(&lookup("canvas-units"))),
            )
        })
    };
    Box::new(
        Row::new()
            .no_wrap()
            .with_gap(12.0)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(orientation))
            .push(custom),
    )
}

/// What creating the artwork does to the one in the window.
fn replacing(cx: &ViewCx<ArtworkResource>) -> Element {
    let replacing = {
        let live = cx.live().clone();
        Text::dynamic(move || {
            let art = live.get();
            if art.marks.is_empty() {
                ui::i18n::I18nManager::lookup_plain("canvas-new-tab-hint")
            } else {
                let title = if art.title.is_empty() {
                    ui::i18n::I18nManager::lookup_plain("canvas-untitled")
                } else {
                    art.title.clone()
                };
                ui::i18n::I18nManager::format("canvas-new-replaces", &[("title", title.into())])
            }
        })
        .with_role(TextRole::Caption)
        .dimmed()
    };
    Box::new(replacing)
}

/// The new-artwork dialog's body.
pub fn body(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let choices = tools.new.clone();
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let title = TextInput::from_state(&choices.title)
        .with_id("canvas-new-title")
        .with_placeholder(&lookup("canvas-untitled"))
        .with_accessible_name(lookup("canvas-artwork-title"));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0)
            .push(super::heading("canvas-new-size"))
            .push(rows(size_tiles(&choices), 4))
            .push(orientation(cx, &choices))
            .push(super::heading("canvas-new-start"))
            .push(rows(start_tiles(&choices), 3))
            .push(super::heading("canvas-artwork-title"))
            .push(Box::new(title))
            .push(replacing(cx)),
    )
}

/// A side field's width.
const SIDE_FIELD: f32 = 72.0; // "8192" in the body face with its insets: a text width, not spacing

/// The welcome over a blank artwork: what a person can do here, that an
/// agent can paint here too, and the artworks worked on lately.
pub fn welcome(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    // Shown while the artwork is blank and the welcome was not put away.
    let blank = cx.value(|art: &Artwork| art.marks.is_empty());
    let tools = tools.clone();
    cx.switch(&blank, move |cx, blank| {
        if !*blank {
            return Box::new(Spacer::fixed(0.0)) as Element;
        }
        let tools = tools.clone();
        cx.switch(&tools.welcomed.clone(), move |cx, welcomed| {
            if *welcomed {
                return Box::new(Spacer::fixed(0.0)) as Element;
            }
            let tools = tools.clone();
            cx.switch(&tools.sidebar.clone(), move |cx, shown| {
                placed(card(cx, &tools), *shown)
            })
        })
    })
}

/// The welcome's card.
fn card(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let button = |id: &str, icon: Icon, then: Box<dyn Fn()>| -> Element {
        let name = lookup(id);
        Box::new(Button::new(id, &name, None).with_icon(icon).on_view(then))
    };
    let new = {
        let sheet = tools.sheet.clone();
        button(
            "canvas-welcome-new",
            Icon::FilePlus,
            Box::new(move || sheet.set(Some(Sheet::NewArtwork))),
        )
    };
    let open = {
        let files = tools.files.clone();
        button(
            "canvas-welcome-open",
            Icon::FolderOpen,
            Box::new(move || files.set(Some(FileRequest::Open))),
        )
    };
    let close = {
        let welcomed = tools.welcomed.clone();
        let name = lookup("canvas-welcome-close");
        Box::new(
            Button::new("canvas-welcome-close", "", None)
                .with_icon(Icon::X)
                .ghost()
                .with_accessible_name(&name)
                .tooltip(name.clone())
                .with_size(super::ICON_BUTTON, super::ICON_BUTTON)
                .on_view(move || welcomed.set(true)),
        )
    };
    let tip = |icon: Icon, id: &str| -> Element {
        Box::new(
            Row::new()
                .no_wrap()
                .with_gap(10.0)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(ui::widget::IconWidget::new(icon, 16.0).dim()))
                .push(Box::new(Text::new(id))),
        )
    };
    let recents = recent_list(cx, tools);
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(12.0)
        .push(Box::new(
            Row::new()
                .no_wrap()
                .with_gap(8.0)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(
                            Text::new("canvas-welcome-title").with_role(TextRole::Title),
                        )),
                ))
                .push(close),
        ))
        .push(tip(Icon::Brush, "canvas-welcome-paint"))
        .push(tip(Icon::Square, "canvas-welcome-draw"))
        .push(tip(Icon::Bot, "canvas-welcome-agent"))
        .push(tip(Icon::Keyboard, "canvas-welcome-keys"))
        .push(Box::new(
            Row::new().no_wrap().with_gap(8.0).push(new).push(open),
        ))
        .push(recents);
    let card = Container::new()
        .with_no_padding()
        .with_width(WELCOME)
        .with_child(Box::new(Welcome(Box::new(ui::widget::Card::new(
            Box::new(body),
        )))));
    Box::new(card)
}

/// The card centred over the canvas: inset by the rail, the header and
/// the sidebar when it is shown. Everywhere else the canvas stays the
/// artwork's.
fn placed(card: Element, sidebar: bool) -> Element {
    let right = if sidebar { crate::SIDEBAR } else { 0.0 };
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_horizontal_padding(crate::RAIL, right)
                    .with_extra_top_padding(HEADER)
                    .with_fill_remaining_height()
                    .with_gap(0.0)
                    .push(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_fill_remaining_height()
                            .with_justify(ui::taffy::JustifyContent::Center)
                            .with_align_items(ui::taffy::AlignItems::Center)
                            .push(card),
                    )),
            )),
    )
}

/// The header's height, which the welcome is centred below.
const HEADER: f32 = 52.0; // the header strip's height: a layout size, not spacing

/// The welcome card's width.
const WELCOME: f32 = 420.0; // a readable measure for its lines: a layout width, not spacing

/// The artworks worked on lately, each opened by a click.
fn recent_list(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let files = tools.files.clone();
    cx.switch(&tools.recents, move |_, recents: &Vec<Recent>| {
        if recents.is_empty() {
            return Box::new(Spacer::fixed(0.0)) as Element;
        }
        let mut list = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(2.0)
            .push(super::heading("canvas-recent"));
        for recent in recents.iter().take(4) {
            let name = if recent.title.is_empty() {
                std::path::Path::new(&recent.path)
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned())
            } else {
                recent.title.clone()
            };
            let place = std::path::Path::new(&recent.path)
                .parent()
                .map_or_else(String::new, |parent| parent.display().to_string());
            let content = Row::new()
                .no_wrap()
                .with_padding(8.0, 4.0)
                .with_gap(10.0)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    ui::widget::IconWidget::new(Icon::Image, 16.0).dim(),
                ))
                .push(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(0.0)
                        .push(Box::new(Text::new(&name)))
                        .push(caption(&format!(
                            "{} · {place}",
                            sides(recent.width, recent.height)
                        ))),
                ));
            // Choosing the row asks Files for it; the file desk takes the
            // request and clears it.
            list = list.push(Box::new(
                ListRow::new(content)
                    .choosing(&files, Some(FileRequest::OpenRecent(recent.path.clone())))
                    .with_id(format!("canvas-recent-{}", recent.at_ms))
                    .with_accessible_name(format!("{name}, {place}")),
            ));
        }
        Box::new(list)
    })
}

/// The welcome card as one group: its buttons are a person's way to the
/// artwork's own actions, which an agent already has by name.
struct Welcome(Element);

impl ui::widget::Decorator for Welcome {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.0.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.0.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "Welcome"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::Group)
                .with_name(ui::i18n::I18nManager::lookup_plain("canvas-welcome-title")),
        )
    }
}
