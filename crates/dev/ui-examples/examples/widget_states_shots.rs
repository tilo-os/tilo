#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "a capture scenario lays a fixed gallery grid out in small, bounded pixel math"
)]

//! The widget library in its states, dark and light: every public control,
//! input, row, navigation and content composite, overlay and status surface
//! of `tilo_ui::widget`, each in rest, hover, pressed, focused, disabled,
//! selected and error where the widget has that state. The review grid for
//! `docs/design-language.md`: type roles, concentric radii, the inverted
//! primary, the lift, your-colour focus, edges, heights per `ControlSize`,
//! spacing and contrast.
//!
//! Hover and press are real pointer events delivered to the retained widget
//! at its centre, focus is `Widget::set_focus`; disabled, selected and error
//! use each widget's own API. Overlays are captured through `FrameHarness`
//! over a busy page, so the floating material shows its blur exactly as a
//! window does.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- widget_states [OUT_DIR]`
//! Out:  `OUT_DIR` (default `/tmp/widget_states`): `<page>-<theme>.png` for
//!       the pages `controls`, `sizes`, `inputs`, `rows`, `panes`,
//!       `content`, `editor` (and `editor-large` at 200 % text), `overlays`,
//!       `overlays-opaque` and `modal`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::AutoCommit;
use chrono::NaiveDate;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

use tilo_ui::chrome::{NavCount, NavRowFace, paint_nav_row};
use tilo_ui::headless::FrameHarness;
use tilo_ui::icons::Icon;
use tilo_ui::launch::ShotFrame;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::paint::PaintCtx;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::time::Duration;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use tilo_ui::widget::toast::ToastKind;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::toolbar::Orientation;
use tilo_ui::widget::tree_view::TreeNode;
use tilo_ui::widget::{
    Actions, Avatar, AvatarFace, AvatarSize, Badge, Banner, Breadcrumb, Button, ButtonRole, Card,
    CardGrid, Checkbox, Chip, ChipLead, ChipStyle, Column, Container, ContextMenu, ControlSize,
    DatePicker, DropZone, Dropdown, Element, EmptyState, Field, Image, Interaction, Link, ListRow,
    ListRowFace, MediaArt, MediaCard, MediaCardFace, MediaGrid, MediaSource, MenuItem, ModalDialog,
    NavItem, NavList, NavSection, NumberInput, Popover, PopoverSide, ProgressBar, ProgressRing,
    Property, PropertyList, Row, RowLeading, RowStatus, SearchField, Section, Skeleton, Slider,
    Spinner, StatGroup, StatTile, Stepper, TabBar, TabDescriptor, Text, TextInput, TimePicker,
    Toggle, ToggleGroup, Tone, Toolbar, TreeView, Trend, Widget,
};
use tilo_ui::{LocalState, State, TextRole};

const PAGE_W: f64 = 1280.0;
const MARGIN: f64 = 24.0;
const LABEL_W: f64 = 176.0;
const HEADER_H: f64 = 36.0;
const SETTLE: Duration = Duration::from_secs(2);

/// One state a cell shows. Hover and press are delivered as pointer
/// events, focus through `set_focus`; the rest are the widget's own API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Rest,
    Hover,
    Pressed,
    Focused,
    Disabled,
    Selected,
    Error,
}

impl St {
    fn label(self) -> &'static str {
        match self {
            Self::Rest => "Rest",
            Self::Hover => "Hover",
            Self::Pressed => "Pressed",
            Self::Focused => "Focused",
            Self::Disabled => "Disabled",
            Self::Selected => "Selected or on",
            Self::Error => "Error",
        }
    }
}

/// One column of a grid page: a state at a control size.
#[derive(Clone, Copy)]
struct Col {
    label: &'static str,
    st: St,
    size: ControlSize,
}

const fn state(st: St) -> Col {
    Col {
        label: "",
        st,
        size: ControlSize::Regular,
    }
}

const CONTROL_COLS: &[Col] = &[
    state(St::Rest),
    state(St::Hover),
    state(St::Pressed),
    state(St::Focused),
    state(St::Disabled),
    state(St::Selected),
];
const INPUT_COLS: &[Col] = &[
    state(St::Rest),
    state(St::Hover),
    state(St::Focused),
    state(St::Disabled),
    state(St::Error),
];
const ROW_COLS: &[Col] = &[
    state(St::Rest),
    state(St::Hover),
    state(St::Pressed),
    state(St::Focused),
    state(St::Selected),
];
const SIZE_COLS: &[Col] = &[
    Col {
        label: "Small",
        st: St::Rest,
        size: ControlSize::Small,
    },
    Col {
        label: "Regular",
        st: St::Rest,
        size: ControlSize::Regular,
    },
    Col {
        label: "Large",
        st: St::Rest,
        size: ControlSize::Large,
    },
];

type Build = fn(Col, &Theme) -> Option<Element>;
type Paint = fn(&mut PaintCtx<'_>, Rect, Col);

/// How a row of the grid draws a cell.
enum Draw {
    /// A retained widget, laid out at its own size and centred in the cell.
    Widget(Build),
    /// A retained widget stretched to the cell's width.
    Filling(Build),
    /// An immediate-mode face painted into the cell.
    Face(Paint),
}

struct Spec {
    name: &'static str,
    height: f64,
    draw: Draw,
}

const fn widget(name: &'static str, height: f64, build: Build) -> Spec {
    Spec {
        name,
        height,
        draw: Draw::Widget(build),
    }
}

const fn filling(name: &'static str, height: f64, build: Build) -> Spec {
    Spec {
        name,
        height,
        draw: Draw::Filling(build),
    }
}

const fn face(name: &'static str, height: f64, paint: Paint) -> Spec {
    Spec {
        name,
        height,
        draw: Draw::Face(paint),
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every cell builder answers `Option`; a state the widget lacks is `None`"
)]
fn boxed(w: impl Widget + 'static) -> Option<Element> {
    Some(Box::new(w))
}

/// A button of `role` in the cell's state and size.
fn button(role: ButtonRole, label: &str, col: Col) -> Option<Element> {
    match col.st {
        St::Selected | St::Error => None,
        st => boxed(
            Button::new("button", label, None)
                .role(role)
                .control_size(col.size)
                .with_enabled(st != St::Disabled),
        ),
    }
}

fn control_specs() -> Vec<Spec> {
    vec![
        widget("Button, primary", 56.0, |c, _| {
            button(ButtonRole::Primary, "Save draft", c)
        }),
        widget("Button, secondary", 56.0, |c, _| {
            button(ButtonRole::Secondary, "Discard", c)
        }),
        widget("Button, destructive", 56.0, |c, _| {
            button(ButtonRole::Destructive, "Delete", c)
        }),
        widget("Button, plain", 56.0, |c, _| {
            button(ButtonRole::Plain, "Details", c)
        }),
        widget("Icon button", 56.0, |c, _| match c.st {
            St::Selected | St::Error => None,
            st => boxed(
                Button::icon_only("share", Icon::Pencil, "Edit")
                    .control_size(c.size)
                    .with_enabled(st != St::Disabled),
            ),
        }),
        widget("Switch", 56.0, |c, _| {
            boxed(
                Toggle::from_state(&LocalState::new(c.st == St::Selected))
                    .with_accessible_name("Wi-Fi")
                    .with_enabled(c.st != St::Disabled),
            )
        }),
        widget("Checkbox", 56.0, |c, _| {
            boxed(
                Checkbox::from_state(&LocalState::new(c.st == St::Selected))
                    .with_label("Sync")
                    .with_enabled(c.st != St::Disabled),
            )
        }),
        filling("Slider", 56.0, |c, _| {
            let value = if c.st == St::Selected { 1.0 } else { 0.4 };
            boxed(
                Slider::from_state(&LocalState::new(value))
                    .with_accessible_name("Volume")
                    .with_enabled(c.st != St::Disabled),
            )
        }),
        widget("Segmented", 56.0, |c, _| match c.st {
            St::Disabled | St::Error => None,
            _ => boxed(
                ToggleGroup::new(
                    vec![
                        Segment::new("Day"),
                        Segment::new("Week"),
                        Segment::new("Year"),
                    ],
                    usize::from(c.st == St::Selected),
                )
                .control_size(c.size),
            ),
        }),
        widget("Chip", 56.0, |c, _| match c.st {
            St::Disabled | St::Error => None,
            st => boxed(
                Chip::new("Design")
                    .selectable()
                    .selected(st == St::Selected),
            ),
        }),
        widget("Dropdown", 56.0, |c, _| match c.st {
            St::Selected | St::Error => None,
            st => boxed(
                Dropdown::from_state(
                    vec!["Low".into(), "Medium".into(), "High".into()],
                    &LocalState::new("Medium".to_owned()),
                )
                .with_accessible_name("Quality")
                .control_size(c.size)
                .with_enabled(st != St::Disabled),
            ),
        }),
        widget("Number field", 56.0, |c, _| match c.st {
            St::Selected | St::Error => None,
            st => boxed(
                NumberInput::new(12.0)
                    .with_range(0.0, 99.0)
                    .with_suffix(" px")
                    .with_accessible_name("Margin")
                    .control_size(c.size)
                    .with_enabled(st != St::Disabled),
            ),
        }),
        widget("Link", 48.0, |c, _| match c.st {
            St::Rest | St::Hover | St::Pressed | St::Focused => {
                boxed(Link::new("https://tilo.so/privacy").with_label("Privacy policy"))
            }
            _ => None,
        }),
    ]
}

fn size_specs() -> Vec<Spec> {
    vec![
        widget("Button, primary", 56.0, |c, _| {
            button(ButtonRole::Primary, "Save draft", c)
        }),
        widget("Button, secondary", 56.0, |c, _| {
            button(ButtonRole::Secondary, "Discard", c)
        }),
        widget("Icon button", 56.0, |c, _| {
            boxed(Button::icon_only("share", Icon::Pencil, "Edit").control_size(c.size))
        }),
        widget("Dropdown", 56.0, |c, _| {
            boxed(
                Dropdown::from_state(
                    vec!["Low".into(), "Medium".into()],
                    &LocalState::new("Medium".to_owned()),
                )
                .control_size(c.size),
            )
        }),
        widget("Number field", 56.0, |c, _| {
            boxed(NumberInput::new(12.0).control_size(c.size))
        }),
        widget("Segmented", 56.0, |c, _| {
            boxed(
                ToggleGroup::new(vec![Segment::new("List"), Segment::new("Grid")], 0)
                    .control_size(c.size),
            )
        }),
        filling("Search field", 56.0, |c, _| {
            boxed(
                SearchField::filter()
                    .with_placeholder("Filter")
                    .control_size(c.size),
            )
        }),
        filling("Side by side", 64.0, |c, _| {
            boxed(
                Row::new()
                    .with_gap(8.0)
                    .push(Box::new(
                        Button::new("a", "Save", None)
                            .role(ButtonRole::Primary)
                            .control_size(c.size),
                    ))
                    .push(Box::new(
                        Button::new("b", "Discard", None).control_size(c.size),
                    ))
                    .push(Box::new(NumberInput::new(4.0).control_size(c.size)))
                    .push(Box::new(
                        ToggleGroup::new(vec![Segment::new("A"), Segment::new("B")], 0)
                            .control_size(c.size),
                    )),
            )
        }),
    ]
}

fn input_specs() -> Vec<Spec> {
    fn field(label: &str, control: TextInput, col: Col) -> Option<Element> {
        match col.st {
            St::Error => {
                boxed(Field::new(label, control).with_error("Use letters, numbers and spaces"))
            }
            St::Selected => None,
            st => boxed(control.disabled(st == St::Disabled)),
        }
    }
    vec![
        filling("Text field, empty", 96.0, |c, _| {
            field("Name", TextInput::local().with_placeholder("Name"), c)
        }),
        filling("Text field, filled", 96.0, |c, _| {
            field(
                "Name",
                TextInput::local().with_initial_value("Ada Lovelace"),
                c,
            )
        }),
        filling("Search field", 72.0, |c, _| match c.st {
            St::Error | St::Disabled | St::Selected => None,
            _ => boxed(SearchField::filter().with_placeholder("Filter messages")),
        }),
        widget("Number field", 72.0, |c, _| match c.st {
            St::Error | St::Selected => None,
            st => boxed(
                NumberInput::new(1.5)
                    .with_precision(1)
                    .with_suffix(" m")
                    .with_enabled(st != St::Disabled),
            ),
        }),
        widget("Time", 148.0, |c, _| match c.st {
            St::Error | St::Disabled | St::Selected => None,
            _ => boxed(TimePicker::new(10, 42).with_accessible_name("Start")),
        }),
    ]
}

fn row_specs() -> Vec<Spec> {
    fn interaction(col: Col) -> Interaction {
        Interaction {
            hovered: matches!(col.st, St::Hover | St::Pressed),
            pressed: col.st == St::Pressed,
            focused: col.st == St::Focused,
            disabled: col.st == St::Disabled,
        }
    }
    vec![
        face("List row", 64.0, |ctx, rect, col| {
            let row = ListRowFace::new("Quarterly report")
                .secondary("Edited by Ana")
                .meta("10:42")
                .leading(RowLeading::Icon(Icon::FileText))
                .selected(col.st == St::Selected)
                .interaction(interaction(col));
            let h = row.height(ctx.text, ctx.theme);
            row.paint(ctx, centred(rect, h));
        }),
        face("List row, person", 64.0, |ctx, rect, col| {
            let row = ListRowFace::new("Ana Lima")
                .secondary("ana@tilo.so")
                .leading(RowLeading::Avatar(AvatarFace::new("Ana Lima", "ana")))
                .status(RowStatus::Active)
                .selected(col.st == St::Selected)
                .interaction(interaction(col));
            let h = row.height(ctx.text, ctx.theme);
            row.paint(ctx, centred(rect, h));
        }),
        face("Navigation row", 48.0, |ctx, rect, col| {
            let h = tilo_ui::chrome::nav_row_height(ctx.theme);
            let row = NavRowFace::new(centred(rect, h), "Inbox")
                .icon(Icon::Inbox)
                .count(NavCount::Neutral(12))
                .hovered(matches!(col.st, St::Hover | St::Pressed))
                .focused(col.st == St::Focused)
                .selected(col.st == St::Selected);
            paint_nav_row(ctx.scene, ctx.transform, ctx.text, ctx.theme, &row);
        }),
        face("Navigation row, unread", 48.0, |ctx, rect, col| {
            let h = tilo_ui::chrome::nav_row_height(ctx.theme);
            let row = NavRowFace::new(centred(rect, h), "Notifications")
                .icon(Icon::Bell)
                .count(NavCount::Live(3))
                .hovered(matches!(col.st, St::Hover | St::Pressed))
                .focused(col.st == St::Focused)
                .selected(col.st == St::Selected);
            paint_nav_row(ctx.scene, ctx.transform, ctx.text, ctx.theme, &row);
        }),
        face("Media card", 200.0, |ctx, rect, col| {
            let card = MediaCardFace::new(MediaArt::Image(art()))
                .title("Coastline")
                .meta("Ana Lima")
                .selected(col.st == St::Selected)
                .interaction(interaction(col));
            let width = rect.width().min(160.0);
            let h = card.height(ctx.text, ctx.theme, width);
            card.paint(ctx, Rect::from_origin_size(rect.origin(), (width, h)));
        }),
    ]
}

/// `height` tall, vertically centred in `rect`, full width.
fn centred(rect: Rect, height: f64) -> Rect {
    let y0 = rect.y0 + ((rect.height() - height) / 2.0).max(0.0);
    Rect::new(rect.x0, y0, rect.x1, y0 + height)
}

/// A photograph-like image: a warm-to-cool sky over a dark band, with fine
/// detail a blur has to smooth away.
fn art() -> &'static vello::peniko::ImageData {
    static ART: std::sync::OnceLock<vello::peniko::ImageData> = std::sync::OnceLock::new();
    ART.get_or_init(|| {
        let (w, h) = (240_u32, 160_u32);
        let rgba = photo_rgba(w, h);
        vello::peniko::ImageData {
            data: vello::peniko::Blob::new(Arc::new(rgba)),
            format: vello::peniko::ImageFormat::Rgba8,
            alpha_type: vello::peniko::ImageAlphaType::Alpha,
            width: w,
            height: h,
        }
    })
}

fn photo_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        for column in 0..width {
            let fx = f64::from(column) / f64::from(width);
            let fy = f64::from(row) / f64::from(height);
            let horizon = 0.62 + 0.05 * (fx * 9.0).sin();
            let (red, green, blue) = if fy < horizon {
                let t = fy / horizon;
                (
                    0.95 - 0.55 * t + 0.1 * fx,
                    0.62 - 0.2 * t + 0.15 * fx,
                    0.35 + 0.45 * t,
                )
            } else {
                let ripple = if (row / 3 + column / 7) % 2 == 0 {
                    0.08
                } else {
                    0.0
                };
                (0.08 + ripple, 0.22 + ripple, 0.32 + ripple * 2.0)
            };
            for channel in [red, green, blue] {
                rgba.push((channel.clamp(0.0, 1.0) * 255.0) as u8);
            }
            rgba.push(255);
        }
    }
    rgba
}

/// Draw one grid page: the state header, then each row's label and cells.
fn grid_page(theme: &Theme, cols: &[Col], specs: &[Spec], out: &Path) {
    let cell_w = (PAGE_W - 2.0 * MARGIN - LABEL_W) / cols.len() as f64;
    let height = MARGIN * 2.0 + HEADER_H + specs.iter().map(|s| s.height).sum::<f64>();
    let page = Rect::new(0.0, 0.0, PAGE_W, height);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let mut doc = AutoCommit::new();
    let label_style = theme.text(TextRole::Label);
    let caption = theme.text(TextRole::Caption);
    for (i, col) in cols.iter().enumerate() {
        let label = if col.label.is_empty() {
            col.st.label()
        } else {
            col.label
        };
        tr.draw_style(
            &mut scene,
            label,
            theme.tokens.colors.text_dim,
            Affine::translate((MARGIN + LABEL_W + i as f64 * cell_w, MARGIN)),
            None,
            &caption,
        );
    }
    let mut y = MARGIN + HEADER_H;
    let mut drawn = Vec::new();
    for spec in specs {
        let (_, label_h) = tr.measure_style(spec.name, None, &label_style);
        tr.draw_style(
            &mut scene,
            spec.name,
            theme.tokens.colors.text_dim,
            Affine::translate((MARGIN, y + (spec.height - f64::from(label_h)) / 2.0)),
            None,
            &label_style,
        );
        for (i, col) in cols.iter().enumerate() {
            let x0 = MARGIN + LABEL_W + i as f64 * cell_w;
            // Leave a gutter between cells so nothing in one touches the next.
            let cell = Rect::new(x0, y, x0 + cell_w - 16.0, y + spec.height);
            let mut canvas = Canvas {
                scene: &mut scene,
                tr: &mut tr,
                doc: &mut doc,
                theme,
                page,
            };
            drawn.extend(draw_cell(&mut canvas, cell, spec, *col));
        }
        y += spec.height;
    }
    // The top layer goes over every cell, as a window presents it.
    let presence = PresenceCtx::default();
    let previous = presence.push_viewport(page);
    for widget in &mut drawn {
        widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }
    presence.pop_viewport(previous);
    let mut renderer = OffscreenRenderer::new(PAGE_W as u32, height as u32);
    renderer.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

fn draw_cell(canvas: &mut Canvas<'_>, cell: Rect, spec: &Spec, col: Col) -> Option<Element> {
    let (build, fill) = match spec.draw {
        Draw::Face(paint) => {
            paint(
                &mut PaintCtx::new(canvas.scene, Affine::IDENTITY, canvas.tr, canvas.theme),
                cell,
                col,
            );
            return None;
        }
        Draw::Widget(build) => (build, false),
        Draw::Filling(build) => (build, true),
    };
    let mut widget = build(col, canvas.theme)?;
    stamp(canvas, cell, widget.as_mut(), col.st, fill);
    Some(widget)
}

/// Where a grid page draws: its scene, text, document, theme and bounds.
struct Canvas<'a> {
    scene: &'a mut Scene,
    tr: &'a mut TextRenderer,
    doc: &'a mut AutoCommit,
    theme: &'a Theme,
    page: Rect,
}

/// Lay `widget` out in `cell`, put it in `st` and draw its main pass; the
/// grid draws every top layer after all cells.
fn stamp(canvas: &mut Canvas<'_>, cell: Rect, widget: &mut dyn Widget, st: St, fill: bool) {
    let (scene, tr, doc): (&mut Scene, &mut TextRenderer, &mut AutoCommit) =
        (&mut *canvas.scene, &mut *canvas.tr, &mut *canvas.doc);
    let (theme, page) = (canvas.theme, canvas.page);
    widget.set_motion_reduced(true);
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = widget.build_layout(&mut taffy, theme, &mut nodes);
    if fill && let Ok(style) = taffy.style(root) {
        let mut style = style.clone();
        style.size.width = taffy::style_helpers::length(cell.width() as f32);
        let _ = taffy.set_style(root, style);
    }
    let _ = taffy.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(cell.width() as f32),
            height: AvailableSpace::Definite(cell.height() as f32),
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(Size::ZERO, |&widget| {
                // SAFETY: the node map was filled by `build_layout` above
                // from widgets `widget` owns, and nothing else touches them
                // while layout runs.
                unsafe { (*widget).measure(known, available, tr) }
            })
        },
    );
    let size = taffy.layout(root).map(|l| l.size).unwrap_or_default();
    let origin = Point::new(
        cell.x0,
        cell.y0 + ((cell.height() - f64::from(size.height)) / 2.0).max(0.0),
    );
    widget.apply_layout(&taffy, root, origin);
    widget.sync(doc, &automerge::ROOT);
    let centre = widget.get_rect().center();
    let modifiers = Modifiers::default();
    let mut deliver = |widget: &mut dyn Widget, event: WindowEvent| {
        let mut handled = false;
        let mut focus_target = None;
        widget.handle_event(
            &event,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root,
                taffy: &taffy,
            },
        );
    };
    let moved = WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: PhysicalPosition::new(centre.x, centre.y),
    };
    match st {
        St::Hover => deliver(widget, moved),
        St::Pressed => {
            deliver(widget, moved);
            deliver(
                widget,
                WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
        }
        St::Focused => widget.set_focus(true),
        _ => {}
    }
    let presence = PresenceCtx::default();
    let previous = presence.push_viewport(page);
    widget.draw(scene, Affine::IDENTITY, tr, theme, &presence);
    presence.pop_viewport(previous);
}

/// One block of a composite page: a title and a widget at a fixed size.
struct Block {
    title: &'static str,
    width: f64,
    height: f64,
    build: fn(&Theme) -> Element,
}

const fn block(
    title: &'static str,
    width: f64,
    height: f64,
    build: fn(&Theme) -> Element,
) -> Block {
    Block {
        title,
        width,
        height,
        build,
    }
}

/// Lay blocks out left to right, wrapping, each under its caption.
fn block_page(theme: &Theme, blocks: &[Block], out: &Path) {
    let gap = 32.0;
    let caption_h = 24.0;
    let mut placed = Vec::new();
    let (mut x, mut y, mut row_h) = (MARGIN, MARGIN, 0.0_f64);
    for b in blocks {
        if x + b.width > PAGE_W - MARGIN && x > MARGIN {
            x = MARGIN;
            y += row_h + gap;
            row_h = 0.0;
        }
        placed.push((Rect::new(x, y, x + b.width, y + caption_h + b.height), b));
        x += b.width + gap;
        row_h = row_h.max(caption_h + b.height);
    }
    let height = y + row_h + MARGIN;
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let caption = theme.text(TextRole::Caption);
    for (rect, b) in placed {
        tr.draw_style(
            &mut scene,
            b.title,
            theme.tokens.colors.text_dim,
            Affine::translate((rect.x0, rect.y0)),
            None,
            &caption,
        );
        let cell = Rect::new(rect.x0, rect.y0 + caption_h, rect.x1, rect.y1);
        // In a column, as a pane holds it, so it takes the pane's width.
        let mut widget = Column::new().with_no_padding().push((b.build)(theme));
        stamp_block(&mut scene, theme, cell, &mut widget);
    }
    let mut renderer = OffscreenRenderer::new(PAGE_W as u32, height as u32);
    renderer.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

/// Lay `widget` out in a window the size of `cell` through the toolkit's
/// own layout pass (`ShotFrame`, as a shot does), and draw it there.
fn stamp_block(scene: &mut Scene, theme: &Theme, cell: Rect, widget: &mut dyn Widget) {
    widget.set_motion_reduced(true);
    let mut frame = ShotFrame::new((cell.width() as u32, cell.height() as u32), theme.clone());
    frame.sync(widget);
    if let Err(error) = frame.layout(widget) {
        eprintln!("widget_states: layout failed: {error:?}");
        return;
    }
    let mut drawn = frame.draw(widget);
    frame.draw_overlay(widget, &mut drawn);
    scene.append(&drawn, Some(Affine::translate((cell.x0, cell.y0))));
}

#[derive(Clone)]
struct Task {
    name: &'static str,
    owner: &'static str,
    hours: Option<u32>,
}

fn table() -> Element {
    let mut tasks = DataTable::new(vec![
        ColumnDef::new("task", "Task", ColumnWidth::Flex(2.0))
            .with_text(|t: &Task| t.name.to_owned()),
        ColumnDef::new("owner", "Owner", ColumnWidth::Auto)
            .with_text(|t: &Task| t.owner.to_owned()),
        ColumnDef::new("hours", "Hours", ColumnWidth::Flex(1.0))
            .with_kind(tilo_ui::text_render::TextKind::Numeric)
            .with_optional_text(|t: &Task| t.hours.map(|h| h.to_string()), "Not estimated"),
    ]);
    tasks.set_rows(vec![
        Task {
            name: "Review the sync design",
            owner: "Ana Lima",
            hours: Some(12),
        },
        Task {
            name: "Ship the beta",
            owner: "Ben Okafor",
            hours: Some(140),
        },
        Task {
            name: "Write the release notes",
            owner: "Chloé Martin",
            hours: None,
        },
        Task {
            name: "Plan the launch",
            owner: "Dara Park",
            hours: Some(24),
        },
        Task {
            name: "Record the walkthrough",
            owner: "Ana Lima",
            hours: Some(6),
        },
        Task {
            name: "Archive the old site",
            owner: "Ben Okafor",
            hours: Some(3),
        },
    ]);
    // A selected row between two striped ones, and more rows than the body
    // shows, so the lift, the stripes and the scroll bar meet in one cell.
    let mut tasks = tasks.selectable(true);
    tasks.selection_mut().select_only(2);
    tasks.set_viewport(4.0 * 32.0, 32.0);
    Box::new(tasks)
}

fn pane_blocks() -> Vec<Block> {
    vec![
        block("Navigation list", 280.0, 300.0, |_| {
            Box::new(
                NavList::new(vec![
                    NavSection::untitled(
                        "main",
                        vec![
                            NavItem::new("inbox", "Inbox").icon(Icon::Inbox).count(12),
                            NavItem::new("notifications", "Notifications")
                                .icon(Icon::Bell)
                                .live_count(3),
                            NavItem::new("drafts", "Drafts").icon(Icon::FileText),
                        ],
                    ),
                    NavSection::new(
                        "projects",
                        "Projects",
                        vec![
                            NavItem::new("atlas", "Atlas").icon(Icon::Folder),
                            NavItem::new("harbour", "Harbour").icon(Icon::Folder),
                        ],
                    ),
                ])
                .selected("inbox"),
            )
        }),
        block("Tree", 280.0, 300.0, |_| {
            let root = TreeNode::branch(
                0,
                "Workspace",
                (),
                vec![
                    TreeNode::branch(
                        1,
                        "Design",
                        (),
                        vec![
                            TreeNode::leaf(2, "Colour.md", ()),
                            TreeNode::leaf(3, "Type.md", ()),
                        ],
                    ),
                    TreeNode::leaf(4, "Roadmap.md", ()),
                    TreeNode::leaf(5, "Notes.md", ()),
                ],
            );
            let mut tree = TreeView::new(root).hide_root().with_selected(Some(3));
            tree.toggle_expand(1);
            Box::new(tree)
        }),
        block("Section of rows", 560.0, 300.0, |_| {
            Box::new(Section::titled(
                "Recent",
                vec![
                    Box::new(
                        ListRow::new("q", "Quarterly report")
                            .secondary("Edited by Ana")
                            .meta("10:42")
                            .icon(Icon::FileText),
                    ),
                    Box::new(
                        ListRow::new("s", "Sync service")
                            .secondary("Running on 3 devices")
                            .icon(Icon::Refresh)
                            .active()
                            .selected(true),
                    ),
                    Box::new(
                        ListRow::new("b", "Backup")
                            .secondary("Last run failed")
                            .icon(Icon::HardDrive)
                            .error(),
                    ),
                ],
            ))
        }),
        block("Table", 600.0, 200.0, |_| table()),
        block("Properties", 560.0, 200.0, |_| {
            Box::new(PropertyList::new(vec![
                Property::new("Kind", "Markdown document"),
                Property::new("Size", "14 KB"),
                Property::new("Created", "12 June 2026"),
                Property::new("Shared with", "Ana Lima, Ben Okafor"),
            ]))
        }),
        block("Tabs", 600.0, 56.0, |_| {
            Box::new(TabBar::new(
                "tabs",
                vec![
                    TabDescriptor::new(1, "Overview"),
                    TabDescriptor::new(2, "Activity"),
                    TabDescriptor::new(3, "Settings"),
                ],
                1,
            ))
        }),
        block("Toolbar", 560.0, 56.0, |_| {
            Box::new(
                Toolbar::new(Orientation::Horizontal)
                    .push_button("Bold", || {})
                    .push_button("Italic", || {})
                    .push_separator()
                    .push_button("Link", || {})
                    .push_button("Code", || {}),
            )
        }),
        block("Breadcrumb", 600.0, 40.0, |_| {
            Box::new(Breadcrumb::new(vec![
                "Home".into(),
                "Projects".into(),
                "Atlas".into(),
                "Design".into(),
            ]))
        }),
        block("Stepper", 560.0, 72.0, |_| {
            Box::new(
                Stepper::new(vec![
                    "Account".into(),
                    "Devices".into(),
                    "Invite".into(),
                    "Done".into(),
                ])
                .current(1),
            )
        }),
        block("Date", 320.0, 320.0, |_| {
            Box::new(DatePicker::new(
                NaiveDate::from_ymd_opt(2026, 6, 30).unwrap_or_default(),
            ))
        }),
        block("Avatars and badges", 560.0, 120.0, |theme| {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(16.0)
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(
                                Avatar::person("Ana Lima", "ana").size(AvatarSize::Small),
                            ))
                            .push(Box::new(Avatar::person("Ben Okafor", "ben")))
                            .push(Box::new(
                                Avatar::person("Chloé Martin", "chloe").size(AvatarSize::Large),
                            ))
                            .push(Box::new(
                                Avatar::person("Dev Patel", "dev")
                                    .with_presence(theme.tokens.colors.presence.series(3)),
                            )),
                    ))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(Badge::text("Beta")))
                            .push(Box::new(
                                Badge::text("New").style(ChipStyle::Tinted(Tone::Info)),
                            ))
                            .push(Box::new(
                                Badge::text("Failed").style(ChipStyle::Tinted(Tone::Danger)),
                            ))
                            .push(Box::new(Badge::text("Live").style(ChipStyle::Live)))
                            .push(Box::new(Badge::count(3)))
                            .push(Box::new(Badge::count(128)))
                            .push(Box::new(Badge::dot()))
                            .push(Box::new(Chip::new("Ana Lima").with_lead(ChipLead::Dot(
                                theme.tokens.colors.presence.series(0),
                            ))))
                            .push(Box::new(Chip::new("rust").removable())),
                    )),
            )
        }),
    ]
}

/// A note in the block editor: every block kind on the one content edge,
/// with its drag handles, checklist ticks and an embedded note.
fn editor_blocks() -> Vec<Block> {
    vec![block("Block editor", 1100.0, 760.0, |_| {
        let mut editor = tilo_ui::widget::BlockEditor::from_markdown(
            "What has to land before we ship.\n\n\
             - [x] Finalise the onboarding copy\n\
             - [ ] Write the migration guide\n\n\
             - Owners sign off\n\n\
             1. Cut the release branch\n\n\
             > Good notes keep the reason behind a decision.\n\n\
             ```sh\ngit tag -a v1.0\n```\n\n\
             | Stage | Owner |\n| --- | --- |\n| Beta | Ana |\n\n\
             Reading list",
        )
        .with_wrap_lines(true);
        let page = tilo_ui::widget::marks::PageId::from_u128(1);
        if let Some(last) = editor.blocks_mut().last_mut() {
            let end = last.line.value().len();
            last.line.add_mark(tilo_ui::widget::MarkSpan::new(
                0..end,
                tilo_ui::widget::Mark::Transclusion(page),
            ));
        }
        editor.set_transclusion_previews(
            [(
                page,
                tilo_ui::widget::TransclusionPreview {
                    title: "Reading list".to_owned(),
                    excerpt: "Things worth coming back to.".to_owned(),
                },
            )]
            .into_iter()
            .collect(),
        );
        Box::new(editor)
    })]
}

fn content_blocks() -> Vec<Block> {
    vec![
        block("Cards", 600.0, 210.0, |_| {
            Box::new(CardGrid::new(vec![
                Box::new(
                    Card::new(Box::new(Text::new(
                        "Shared with 4 people. Last opened today.",
                    )))
                    .with_title("Atlas plan")
                    .with_footer(Actions::new(Button::new("o", "Open", None))),
                ),
                Box::new(
                    Card::new(Box::new(Text::new("Synced from your phone at 10:12.")))
                        .with_title("Field notes")
                        .with_footer(Actions::new(Button::new("o2", "Open", None))),
                ),
            ]))
        }),
        block("Statistics", 600.0, 120.0, |_| {
            Box::new(StatGroup::new(vec![
                Box::new(StatTile::new("Requests", "1,284").change("12% this hour", Trend::Up)),
                Box::new(
                    StatTile::new("Errors", "3")
                        .tone(Tone::Danger)
                        .change("2 fewer", Trend::Down),
                ),
                Box::new(StatTile::new("Latency", "48").unit("ms")),
                Box::new(StatTile::new("Uptime", "99.9").unit("%")),
            ]))
        }),
        block("Media", 600.0, 340.0, |_| {
            Box::new(MediaGrid::new(vec![
                MediaCard::new("a", MediaSource::Image(art().clone()))
                    .title("Coastline")
                    .meta("Ana Lima"),
                MediaCard::new("b", MediaSource::Image(art().clone()))
                    .title("Harbour at dusk")
                    .meta("Ben Okafor")
                    .selected(true),
                MediaCard::new("c", MediaSource::Placeholder)
                    .title("Still importing")
                    .meta("Chloé Martin"),
            ]))
        }),
        block("Banners", 600.0, 230.0, |_| {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(8.0)
                    .push(Box::new(
                        Banner::info("A new version is ready").with_message("Restart to update."),
                    ))
                    .push(Box::new(Banner::success("Changes saved")))
                    .push(Box::new(
                        Banner::warning("You are offline")
                            .with_message("Edits sync when you reconnect.")
                            .dismissible(),
                    ))
                    .push(Box::new(Banner::error("Upload failed").dismissible())),
            )
        }),
        block("Empty", 380.0, 220.0, |_| {
            Box::new(
                EmptyState::new("No messages")
                    .with_icon(Icon::Inbox)
                    .with_action("New message"),
            )
        }),
        block("Error", 380.0, 220.0, |_| {
            Box::new(EmptyState::error(
                "Could not load messages",
                "The server did not answer.",
            ))
        }),
        block("Loading", 380.0, 220.0, |_| {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(16.0)
                    .push(Box::new(Skeleton::row()))
                    .push(Box::new(Skeleton::text(3)))
                    .push(Box::new(Spinner::new())),
            )
        }),
        block("Progress", 600.0, 150.0, |_| {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(16.0)
                    .push(Box::new(
                        ProgressBar::new(0.62).with_label("Uploading 3 photos"),
                    ))
                    .push(Box::new(ProgressBar::indeterminate()))
                    .push(Box::new(
                        Row::new()
                            .push(Box::new(
                                ProgressRing::new(0.72).value("72%").caption("Storage"),
                            ))
                            .push(Box::new(ProgressRing::new(0.3).value("3 of 10"))),
                    )),
            )
        }),
        block("Drop zone", 600.0, 150.0, |_| {
            Box::new(DropZone::new("drop", "Drop photos here").message("PNG, JPEG or HEIC"))
        }),
    ]
}

/// Draws a hover tooltip in the top layer, as the host does.
struct TooltipProbe {
    anchor: Rect,
    label: &'static str,
}

impl Widget for TooltipProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy
            .new_leaf(taffy::Style::default())
            .unwrap_or_else(|_| NodeId::new(0))
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let viewport = presence.viewport.get().unwrap_or(Rect::ZERO);
        tilo_ui::widget::tooltip::draw_hover_tooltip(
            scene,
            transform,
            tr,
            theme,
            self.anchor,
            self.label,
            viewport,
        );
    }
    fn name(&self) -> &'static str {
        "TooltipProbe"
    }
    fn get_rect(&self) -> Rect {
        self.anchor
    }
}

/// A transient toast painted in the main pass, inside the page's cached
/// columns, as a painted app paints one: its material blurs the photograph
/// through the subtree caches.
struct ToastProbe {
    area: Rect,
}

impl Widget for ToastProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        _nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy
            .new_leaf(taffy::Style::default())
            .unwrap_or_else(|_| NodeId::new(0))
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        tilo_ui::widget::toast::paint_transient(
            scene,
            transform,
            tr,
            theme,
            self.area,
            tilo_ui::widget::toast::ToastAnchor::BottomRight,
            &tilo_ui::widget::toast::ToastCard::new(
                "Copied link to Quarterly report",
                ToastKind::Success,
            )
            .closable(true),
        );
    }
    fn name(&self) -> &'static str {
        "ToastProbe"
    }
    fn get_rect(&self) -> Rect {
        self.area
    }
}

/// A busy page: a photograph across the top and prose under it, so the
/// floating material over it has something to blur.
fn busy_page() -> Column {
    let (w, h) = (1232_u32, 300_u32);
    Column::new()
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_height(300.0)
                .with_child(Box::new(
                    Image::from_rgba(Arc::from(photo_rgba(w, h)), w, h)
                        .with_alt("Coastline")
                        .filling(),
                )),
        ))
        .push(Box::new(
            Text::new("Quarterly report").with_role(TextRole::LargeTitle),
        ))
        .push(Box::new(Text::new(
            "Revenue grew in every region this quarter. The detail below breaks it down by \
             product line and by month, with the forecast for the next two quarters. \
             Subscriptions carried most of the growth; hardware held steady.",
        )))
}

fn overlays(theme: &Theme, out: &Path) {
    let mut menu =
        Popover::new()
            .with_label("Sort by")
            .with_items(["Name", "Date modified", "Size", "Kind"]);
    menu.open_at(Rect::new(24.0, 60.0, 180.0, 92.0), Some(1));
    let detail = Popover::with_content(Text::new("Shared with Ana, Ben and Chloé"))
        .anchored(Rect::new(300.0, 40.0, 420.0, 72.0))
        .side(PopoverSide::Below)
        .open(true);
    let mut context = ContextMenu::new();
    context.push(MenuItem::selection("Open"));
    context.push(MenuItem::selection("Rename").with_icon(Icon::Pencil));
    context.push(MenuItem::selection("Show hidden files").with_checked(true));
    context.push(MenuItem::separator());
    context.push(MenuItem::destructive_selection("Move to bin").with_icon(Icon::Trash));
    context.open_at([600.0, 60.0]);
    context.focus_item(1);
    let dropdown = Dropdown::from_state(
        vec!["Low".into(), "Medium".into(), "High".into()],
        &LocalState::new("Medium".to_owned()),
    )
    .open();
    let root = busy_page()
        .push(Box::new(
            Row::new()
                .push(Box::new(dropdown))
                .push(Box::new(Button::new("x", "Export", None))),
        ))
        .push(Box::new(menu))
        .push(Box::new(detail))
        .push(Box::new(context))
        .push(Box::new(TooltipProbe {
            anchor: Rect::new(980.0, 120.0, 1012.0, 152.0),
            label: "Share this report",
        }));
    let root = root.push(Box::new(ToastProbe {
        area: Rect::new(24.0, 24.0, 1256.0, 324.0),
    }));
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(root), (1280, 640), theme.clone());
    harness.settle_for_capture(SETTLE);
    harness.render_png(out);
    println!("wrote {}", out.display());
}

fn modal(theme: &Theme, out: &Path) {
    let root = busy_page().push(Box::new(
        ModalDialog::new(
            "Delete 3 photos?",
            Text::new("They move to the bin on every device. You can restore them for 30 days."),
        )
        .open(true)
        .secondary_action("Cancel", || {})
        .with_destructive(true)
        .primary_action("Delete photos", || {}),
    ));
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(root), (1280, 640), theme.clone());
    harness.settle_for_capture(SETTLE);
    harness.render_png(out);
    println!("wrote {}", out.display());
}

#[allow(
    clippy::exit,
    reason = "a capture scenario reports an unusable output directory as its exit status"
)]
pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let out = args
        .first()
        .map_or_else(|| PathBuf::from("/tmp/widget_states"), PathBuf::from);
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("widget_states: cannot create {}: {error}", out.display());
        std::process::exit(2);
    }
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let theme = theme.with_reduce_motion(true);
        let file = |page: &str| out.join(format!("{page}-{name}.png"));
        grid_page(&theme, CONTROL_COLS, &control_specs(), &file("controls"));
        grid_page(&theme, SIZE_COLS, &size_specs(), &file("sizes"));
        grid_page(&theme, INPUT_COLS, &input_specs(), &file("inputs"));
        grid_page(&theme, ROW_COLS, &row_specs(), &file("rows"));
        block_page(&theme, &pane_blocks(), &file("panes"));
        block_page(&theme, &content_blocks(), &file("content"));
        block_page(&theme, &editor_blocks(), &file("editor"));
        block_page(
            &theme.clone().with_text_scale(2.0),
            &editor_blocks(),
            &file("editor-large"),
        );
        overlays(&theme, &file("overlays"));
        overlays(
            &theme.clone().with_reduce_transparency(true),
            &file("overlays-opaque"),
        );
        modal(&theme, &file("modal"));
    }
}
