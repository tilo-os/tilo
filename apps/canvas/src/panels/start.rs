//! Starting: the welcome over a blank artwork, saying what can be done
//! here (by a person and by an agent), and the new-artwork dialog, a size
//! and a ground with its medium, each shown as the sheet it makes.

use std::collections::HashMap;

use ui::LocalState;
use ui::icons::Icon;
use ui::prelude::*;
use ui::taffy::{self, NodeId, Style, TaffyTree};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::{AppModel, ModelPath, Widget};
use ui::{PresenceCtx, TextRenderer, Theme};

use super::caption;
use crate::actions::{CanvasAction, NewArtwork};
use crate::document::{self, RecentArtworks, SIZES, STARTS};
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
    if tools.new.size.get().starts_with("pixel-") {
        tools.workspace.set(crate::tools::Workspace::Pixel);
        tools.choose_preset(crate::brushes::Preset::Pixel);
        tools.tool.set(crate::tools::Tool::Brush);
    } else {
        tools.workspace.set(crate::tools::Workspace::Auto);
        tools.choose_preset(crate::brushes::Preset::Round);
        let start = document::start(&tools.new.start.get()).unwrap_or(STARTS[0]);
        tools.tool.set(if start.medium.is_empty() {
            crate::tools::Tool::Select
        } else {
            crate::tools::Tool::Brush
        });
    }
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
    sheet: Box<dyn Fn() -> (f64, f64)>,
    ground: Box<dyn Fn() -> Ground>,
    renderer: Renderer,
}

impl Swatch {
    /// A swatch with a Theme-sized thumbnail, drawing a sheet of the proportions
    /// `sheet` gives, in the ground `ground` gives.
    pub fn new(
        sheet: impl Fn() -> (f64, f64) + 'static,
        ground: impl Fn() -> Ground + 'static,
    ) -> Self {
        Self {
            rect: Rect::ZERO,
            sheet: Box::new(sheet),
            ground: Box::new(ground),
            renderer: Renderer::default(),
        }
    }
}

/// The long side of the small artwork a swatch draws, in units: enough
/// for the ground's tooth to read, few enough cells to make at once.
use crate::content_geometry::SWATCH_UNITS;

impl Widget for Swatch {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt as _;
        let side = theme.tokens.metrics.min_pointer_target;
        let style = Style {
            size: taffy::Size {
                width: taffy::Dimension::length(side),
                height: taffy::Dimension::length(side),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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
        let room = self
            .rect
            .inset(-f64::from(theme.tokens.spacing.rhythm.tight));
        let scale = (room.width() / art.width).min(room.height() / art.height);
        let origin = Point::new(
            room.center().x - art.width * scale / 2.0,
            room.center().y - art.height * scale / 2.0,
        );
        let to_screen = transform * Affine::translate(origin.to_vec2()) * Affine::scale(scale);
        let edge = Rect::from_origin_size(origin, (art.width * scale, art.height * scale));
        ui::widget::card::CardFace::new().paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            edge,
        );
        self.renderer
            .draw(&art, scene, to_screen, text, &Options::default());
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

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
    measure: fn(&ui::tokens::Metrics) -> f32,
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
        .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Related)
        .with_rhythm(ui::tokens::Rhythm::Related)
        .with_align_items(ui::taffy::AlignItems::Center)
        .push(Box::new(swatch))
        .push(Box::new(text));
    super::layout::width(
        Box::new(
            Container::new().with_no_padding().with_child(Box::new(
                ui::widget::ControlRow::new(content)
                    .choosing(state, value.to_owned())
                    .with_id(id)
                    .with_accessible_name(said),
            )),
        ),
        measure,
    )
}

fn rows(tiles: Vec<Element>, per_row: usize) -> Element {
    let mut grid = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Related);
    let mut tiles = tiles.into_iter().peekable();
    while tiles.peek().is_some() {
        let mut row = Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for tile in tiles.by_ref().take(per_row) {
            row = row.push(tile);
        }
        grid = grid.push(Box::new(row));
    }
    Box::new(grid)
}

/// "1920 × 1080".
fn sides(width: f64, height: f64) -> String {
    format!("{width:.0} by {height:.0}")
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
                |metrics| metrics.panel_width_sheet / 4.0,
                &choices.size,
                size.id,
                &format!("canvas-new-size-{}", size.id),
                Swatch::new(move || turned(), move || ground.ground()),
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
            |metrics| metrics.panel_width_sheet / 4.0,
            &choices.size,
            CUSTOM,
            "canvas-new-size-custom",
            Swatch::new(
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
                |metrics| metrics.panel_width_sheet / 3.0,
                &choices.start,
                start.id,
                &format!("canvas-new-start-{}", start.id),
                Swatch::new(
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
    let choices = choices.clone();
    cx.switch(&choices.size.clone(), move |_, size| {
        if size == CUSTOM {
            return custom_sides(&choices);
        }
        // A square stands the same either way: nothing to choose.
        if document::size(size)
            .is_some_and(|preset| (preset.width - preset.height).abs() < f64::EPSILON)
        {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
        }
        Box::new(
            ui::widget::ToggleGroup::from_state(
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
            .with_id("canvas-new-orientation"),
        )
    })
}

/// A custom size's width and height, typed.
fn custom_sides(choices: &Choices) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let width = TextInput::from_state(&choices.width)
        .with_id("canvas-new-width")
        .with_accessible_name(lookup("canvas-artwork-width"));
    let height = TextInput::from_state(&choices.height)
        .with_id("canvas-new-height")
        .with_accessible_name(lookup("canvas-artwork-height"));
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(caption(&lookup("canvas-artwork-width")))
            .push(super::layout::width(
                Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(Box::new(width)),
                ),
                |metrics| metrics.panel_row_height * 2.0,
            ))
            .push(caption(&lookup("canvas-artwork-height")))
            .push(super::layout::width(
                Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_child(Box::new(height)),
                ),
                |metrics| metrics.panel_row_height * 2.0,
            ))
            .push(caption(&lookup("canvas-units"))),
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
        .with_placeholder(lookup("canvas-untitled"))
        .with_accessible_name(lookup("canvas-artwork-title"));
    let starts = {
        let choices = choices.clone();
        cx.switch(&choices.size.clone(), move |_cx, size| {
            if size.starts_with("pixel-") && choices.start.get() == "paper" {
                choices.start.set("transparent".into());
            }
            rows(start_tiles(&choices), 3)
        })
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .push(super::heading("canvas-new-size"))
            .push(rows(size_tiles(&choices), 4))
            .push(orientation(cx, &choices))
            .push(super::heading("canvas-new-start"))
            .push(starts)
            .push(super::heading("canvas-artwork-title"))
            .push(Box::new(title))
            .push(replacing(cx)),
    )
}

/// The welcome over a blank artwork: what a person can do here, that an
/// agent can paint here too, and the artworks worked on lately.
pub fn welcome(cx: &ViewCx<ArtworkResource>, tools: &Tools, compact: bool) -> Element {
    let blank = cx.value(|art: &Artwork| art.marks.is_empty());
    let tools = tools.clone();
    cx.switch(&blank, move |cx, blank| {
        if !*blank {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
        }
        let tools = tools.clone();
        cx.switch(&tools.welcomed.clone(), move |cx, welcomed| {
            if *welcomed {
                return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
            }
            placed(card(cx, &tools), compact)
        })
    })
}

/// The welcome's card.
fn card(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let lookup = ui::i18n::I18nManager::lookup_plain;
    let button = |id: &str, icon: Icon, then: Box<dyn Fn()>| -> Element {
        let name = lookup(id);
        Box::new(Button::new(id, &name).with_icon(icon).on_view(then))
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
            Button::new("canvas-welcome-close", "")
                .with_icon(Icon::X)
                .role(ui::widget::ButtonRole::Plain)
                .with_accessible_name(&name)
                .tooltip(name.clone())
                .control_size(ui::widget::ControlSize::Small)
                .on_view(move || welcomed.set(true)),
        )
    };
    let tip = |icon: Icon, id: &str| -> Element {
        Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(ui::widget::IconWidget::new(icon).dim()))
                .push(Box::new(Text::new(id))),
        )
    };
    let recents = recent_list(cx, tools);
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .push(Box::new(
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
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
            Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .push(new)
                .push(open),
        ))
        .push(recents);
    let card = Container::new()
        .with_no_padding()
        .with_child(Box::new(Welcome(Box::new(ui::widget::Card::new(
            Box::new(body),
        )))));
    super::layout::themed(Box::new(card), |theme, style| {
        // A cap is a comfortable wide measure, never a minimum wider than
        // the space between the rail and the window's trailing gutter.
        style.size.width = ui::taffy::Dimension::auto();
        style.min_size.width = ui::taffy::Dimension::length(0.0);
        style.max_size.width = ui::taffy::Dimension::length(theme.tokens.metrics.panel_width_list);
        style.flex_basis = ui::taffy::Dimension::length(0.0);
        style.flex_grow = 1.0;
        style.flex_shrink = 1.0;
    })
}

/// The welcome is a child of the actual artwork viewport, so the shell's
/// header, inspector and tools retain their full measured space. Phones get
/// a scrolling page; desktop cards stay centred over the artwork.
fn placed(card: Element, compact: bool) -> Element {
    if compact {
        return super::layout::themed(
            Box::new(
                Container::new().filling().with_child(Box::new(
                    Scrollable::new_vertical()
                        .fill_parent_height()
                        .with_child(card),
                )),
            ),
            |theme, style| {
                style.padding = ui::taffy::Rect {
                    left: ui::taffy::LengthPercentage::length(theme.tokens.spacing.s3),
                    right: ui::taffy::LengthPercentage::length(theme.tokens.spacing.s3),
                    top: ui::taffy::LengthPercentage::length(theme.tokens.spacing.s3),
                    bottom: ui::taffy::LengthPercentage::length(theme.tokens.spacing.s3),
                };
            },
        );
    }
    Box::new(
        Container::new()
            .with_no_padding()
            .filling()
            .with_child(super::layout::themed(
                Box::new(
                    Column::new()
                        .with_no_padding()
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
                ),
                move |theme, style| {
                    let gutter = ui::taffy::LengthPercentage::length(theme.tokens.spacing.s4);
                    style.padding = ui::taffy::Rect {
                        left: gutter,
                        right: gutter,
                        top: gutter,
                        bottom: gutter,
                    };
                },
            )),
    )
}

/// The artworks worked on lately, each opened by a click.
fn recent_list(cx: &ViewCx<ArtworkResource>, tools: &Tools) -> Element {
    let files = tools.files.clone();
    cx.switch(&tools.recents, move |_, recents: &RecentArtworks| {
        let recents = &recents.artworks;
        if recents.is_empty() {
            return Box::new(Spacer::token(ui::tokens::SpacingStep::S0)) as Element;
        }
        let mut list = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
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
                .with_padding_rhythm(ui::tokens::Rhythm::Related, ui::tokens::Rhythm::Tight)
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(ui::widget::IconWidget::new(Icon::Image).dim()))
                .push(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(0.0)
                        .push(Box::new(Text::new(&name)))
                        .push(caption(&sides(recent.width, recent.height)))
                        .push(caption(&place)),
                ));
            // Choosing the row asks Files for it; the file desk takes the
            // request and clears it.
            list = list.push(Box::new(
                ui::widget::ControlRow::new(content)
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
