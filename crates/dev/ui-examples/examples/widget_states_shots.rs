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
//!       `navigation`, `icons`, `content`, `editor` (and `controls-large`,
//!       `inputs-large` and `editor-large` at 200 % text), `overlays`,
//!       `overlays-narrow` (800 × 600), `overlays-opaque`, `modal`,
//!       `modal-question` and `modal-tall` (800 × 600).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_ui::widget::TaffyTreeExt;

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
use tilo_ui::widget::toggle_group::{Segment, SegmentedStyle};
use tilo_ui::widget::toolbar::Orientation;
use tilo_ui::widget::tree_view::TreeNode;
use tilo_ui::widget::{
    Actions, Avatar, AvatarFace, AvatarSize, Badge, Banner, Breadcrumb, Button, ButtonRole, Card,
    CardGrid, Checkbox, Chip, ChipLead, ChipStyle, Column, Container, ContextMenu, ControlSize,
    DatePicker, DropZone, Dropdown, Element, EmptyState, Field, IconSize, IconWidget, Image,
    Interaction, Link, ListRow, ListRowFace, MediaArt, MediaCard, MediaCardFace, MediaGrid,
    MediaSource, MenuItem, ModalDialog, NavItem, NavList, NavSection, NumberInput, ObjectHeader,
    Popover, PopoverSide, ProgressBar, ProgressRing, Property, PropertyList, Row, RowLeading,
    RowStatus, SearchField, Section, SectionHeader, Skeleton, Slider, Spinner, StatGroup, StatTile,
    StatusLine, Stepper, TabBar, TabDescriptor, Text, TextInput, TimePicker, Toggle, ToggleGroup,
    Tone, Toolbar, TreeView, Trend, Widget,
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
            Button::new("button", label)
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
        widget("Menu button", 56.0, |c, _| match c.st {
            St::Selected | St::Error => None,
            st => boxed(
                Button::new("menu", "Editor")
                    .control_size(c.size)
                    .with_enabled(st != St::Disabled)
                    .with_menu(Vec::new()),
            ),
        }),
        widget("Key", 56.0, |c, _| match c.st {
            St::Error => None,
            st => boxed(
                Button::new("key", "7")
                    .role(ButtonRole::Key)
                    .control_size(ControlSize::Large)
                    .toggled(st == St::Selected)
                    .with_enabled(st != St::Disabled),
            ),
        }),
        widget("Star toggle", 56.0, |c, _| match c.st {
            St::Error => None,
            st => boxed(
                Button::icon_only("star", Icon::Star, "Star")
                    .control_size(c.size)
                    .toggled(st == St::Selected)
                    .with_enabled(st != St::Disabled),
            ),
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
        widget("Segmented, none", 56.0, |c, _| match c.st {
            St::Error | St::Selected => None,
            st => boxed(
                ToggleGroup::unchosen(vec![
                    Segment::icon(Icon::List, "List"),
                    Segment::icon(Icon::LayoutGrid, "Grid"),
                ])
                .control_size(c.size)
                .with_enabled(st != St::Disabled),
            ),
        }),
        filling("Weekdays", 112.0, |c, _| match c.st {
            St::Error => None,
            st => boxed(
                ToggleGroup::multiple(
                    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
                        .iter()
                        .map(|day| Segment::new(*day))
                        .collect(),
                    vec![true, true, st == St::Selected, true, true, false, false],
                )
                .control_size(ControlSize::Small)
                .with_enabled(st != St::Disabled),
            ),
        }),
        filling("Tabs", 56.0, |c, _| match c.st {
            St::Error | St::Disabled => None,
            _ => boxed(
                ToggleGroup::new(
                    ["Overview", "Activity", "Members", "Settings"]
                        .iter()
                        .map(|tab| Segment::new(*tab))
                        .collect(),
                    usize::from(c.st == St::Selected),
                )
                .with_style(SegmentedStyle::Tabs)
                .control_size(c.size),
            ),
        }),
        // Past their room, tabs take their short labels before they scroll.
        filling("Tabs, short labels", 56.0, |c, _| match c.st {
            St::Error | St::Disabled => None,
            _ => boxed(
                ToggleGroup::new(
                    [("Privacy & Security", "Privacy"), ("Apps & Files", "Apps")]
                        .iter()
                        .map(|(tab, short)| Segment::new(*tab).short(*short))
                        .collect(),
                    usize::from(c.st == St::Selected),
                )
                .with_style(SegmentedStyle::Tabs)
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
        face("Colour swatch", 48.0, |ctx, rect, col| {
            swatch_states(ctx, rect, col, |r| {
                tilo_ui::widget::ColorSwatchFace::new(
                    r,
                    vello::peniko::Color::from_rgb8(0x3b, 0x6f, 0xd4),
                    "Blue",
                )
            });
        }),
        face("Colour swatch, none", 48.0, |ctx, rect, col| {
            swatch_states(ctx, rect, col, |r| {
                tilo_ui::widget::ColorSwatchFace::none(r, "No colour")
            });
        }),
    ]
}

/// A colour swatch of the default size, at the start of `rect`, in `col`'s
/// state; it has no disabled or error state.
fn swatch_states(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    col: Col,
    face: impl Fn(Rect) -> tilo_ui::widget::ColorSwatchFace,
) {
    if matches!(col.st, St::Disabled | St::Error) {
        return;
    }
    let side = col.size.height(ctx.theme);
    let cell = centred(rect, side);
    let swatch = Rect::from_origin_size(cell.origin(), (side * 1.6, side));
    face(swatch)
        .selected(col.st == St::Selected)
        .hovered(matches!(col.st, St::Hover | St::Pressed))
        .focused(col.st == St::Focused)
        .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
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
                        Button::new("a", "Save")
                            .role(ButtonRole::Primary)
                            .control_size(c.size),
                    ))
                    .push(Box::new(Button::new("b", "Discard").control_size(c.size)))
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
        face("Navigation row, parts", 56.0, |ctx, rect, col| {
            let row = NavRowFace::new(rect, "Orders database")
                .icon(Icon::HardDrive)
                .secondary("db.example.net:5432")
                .status(RowStatus::Active)
                .hovered(matches!(col.st, St::Hover | St::Pressed))
                .focused(col.st == St::Focused)
                .selected(col.st == St::Selected);
            let h = row.height(ctx.theme);
            let row = NavRowFace {
                rect: centred(rect, h),
                ..row
            };
            paint_nav_row(ctx.scene, ctx.transform, ctx.text, ctx.theme, &row);
        }),
        face("Navigation row, level", 64.0, |ctx, rect, col| {
            let row = NavRowFace::new(rect, "Backup disk")
                .icon(Icon::Usb)
                .secondary("Every night at 02:00")
                .meta("1.2 TB")
                .level(0.62)
                .hovered(matches!(col.st, St::Hover | St::Pressed))
                .focused(col.st == St::Focused)
                .selected(col.st == St::Selected);
            let h = row.height(ctx.theme);
            let row = NavRowFace {
                rect: centred(rect, h),
                ..row
            };
            paint_nav_row(ctx.scene, ctx.transform, ctx.text, ctx.theme, &row);
        }),
        face("Navigation row, check", 48.0, |ctx, rect, col| {
            let h = tilo_ui::chrome::nav_row_height(ctx.theme);
            let row = NavRowFace::new(centred(rect, h), "Family")
                .checked(col.st != St::Disabled)
                .swatch(ctx.theme.tokens.colors.presence.series(4))
                .hovered(matches!(col.st, St::Hover | St::Pressed))
                .focused(col.st == St::Focused);
            paint_nav_row(ctx.scene, ctx.transform, ctx.text, ctx.theme, &row);
        }),
        face("Navigation row, badge", 48.0, |ctx, rect, col| {
            let h = tilo_ui::chrome::nav_row_height(ctx.theme);
            let row = NavRowFace::new(centred(rect, h), "Allow SSH")
                .badge("POST")
                .status_label("Active", Tone::Success)
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
        tilo_ui::raster::rgba8(rgba, w, h)
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
    // Rows grow with the text, so a page at 200 % text keeps its rows apart.
    let grow = f64::from(theme.text_scale.max(1.0));
    let cell_w = (PAGE_W - 2.0 * MARGIN - LABEL_W) / cols.len() as f64;
    let height =
        MARGIN * 2.0 + HEADER_H * grow + specs.iter().map(|s| s.height * grow).sum::<f64>();
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
    let mut y = MARGIN + HEADER_H * grow;
    let mut drawn = Vec::new();
    for spec in specs {
        let row_h = spec.height * grow;
        let (_, label_h) = tr.measure_style(spec.name, None, &label_style);
        tr.draw_style(
            &mut scene,
            spec.name,
            theme.tokens.colors.text_dim,
            Affine::translate((MARGIN, y + (row_h - f64::from(label_h)) / 2.0)),
            None,
            &label_style,
        );
        for (i, col) in cols.iter().enumerate() {
            let x0 = MARGIN + LABEL_W + i as f64 * cell_w;
            // Leave a gutter between cells so nothing in one touches the next.
            let cell = Rect::new(x0, y, x0 + cell_w - 16.0, y + row_h);
            let mut canvas = Canvas {
                scene: &mut scene,
                tr: &mut tr,
                doc: &mut doc,
                theme,
                page,
            };
            drawn.extend(draw_cell(&mut canvas, cell, spec, *col));
        }
        y += row_h;
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
                state: tilo_ui::widget::Edits::lend(doc),
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
    block_page_at(theme, blocks, out, PAGE_W);
}

/// [`block_page`] on a page `page_w` wide: blocks wider than the page are
/// narrowed to it, as a narrow window narrows its panes.
fn block_page_at(theme: &Theme, blocks: &[Block], out: &Path, page_w: f64) {
    let gap = 32.0;
    let caption_h = 24.0;
    let mut placed = Vec::new();
    let (mut x, mut y, mut row_h) = (MARGIN, MARGIN, 0.0_f64);
    for b in blocks {
        let width = b.width.min(page_w - 2.0 * MARGIN);
        if x + width > page_w - MARGIN && x > MARGIN {
            x = MARGIN;
            y += row_h + gap;
            row_h = 0.0;
        }
        placed.push((Rect::new(x, y, x + width, y + caption_h + b.height), b));
        x += width + gap;
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
    let mut renderer = OffscreenRenderer::new(page_w as u32, height as u32);
    renderer.render_png(&scene, theme.tokens.colors.surfaces.content, out);
    println!("wrote {}", out.display());
}

/// Lay `widget` out in a window the size of `cell` through the toolkit's
/// own layout pass (`ShotFrame`, as a shot does), and draw it there.
fn stamp_block(scene: &mut Scene, theme: &Theme, cell: Rect, widget: &mut dyn Widget) {
    widget.set_motion_reduced(true);
    let mut frame = ShotFrame::new((cell.width() as u32, cell.height() as u32), theme.clone());
    frame.sync(widget);
    // A widget that learns its room in one pass (a table's sideways bar
    // gutter) asks for another, as a window's next frame would give it.
    for _ in 0..3 {
        if let Err(error) = frame.layout(widget) {
            eprintln!("widget_states: layout failed: {error:?}");
            return;
        }
        if !widget.is_layout_dirty() {
            break;
        }
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

/// The navigation list with every part a row can show, a rail whose
/// captions wrap, and the breadcrumb with its root icon and truncation.
fn navigation_blocks() -> Vec<Block> {
    vec![
        block("Navigation list, every part", 320.0, 560.0, |theme| {
            let presence = &theme.tokens.colors.presence;
            let list = NavList::new(vec![
                NavSection::new(
                    "connections",
                    "Connections",
                    vec![
                        NavItem::new("orders", "Orders")
                            .icon(Icon::HardDrive)
                            .secondary("db.example.net:5432")
                            .status(RowStatus::Active),
                        NavItem::new("staging", "Staging")
                            .icon(Icon::HardDrive)
                            .secondary("10.0.4.12:5432")
                            .status(RowStatus::Warning),
                    ],
                ),
                NavSection::new(
                    "requests",
                    "Requests",
                    vec![
                        NavItem::new("list", "List users").badge("GET"),
                        NavItem::new("create", "Create user").badge("POST"),
                        NavItem::new("remove", "Remove user").badge("DELETE"),
                    ],
                ),
                NavSection::new(
                    "calendars",
                    "Calendars",
                    vec![
                        NavItem::new("home", "Home")
                            .checked(true)
                            .swatch(presence.series(1)),
                        NavItem::new("work", "Work")
                            .checked(false)
                            .swatch(presence.series(6)),
                    ],
                ),
                NavSection::new(
                    "destinations",
                    "Destinations",
                    vec![
                        NavItem::new("disk", "Backup disk")
                            .icon(Icon::Usb)
                            .secondary("Every night")
                            .meta("1.2 TB")
                            .level(0.62),
                    ],
                ),
                NavSection::new(
                    "zones",
                    "Zones",
                    vec![
                        NavItem::new("public", "Public")
                            .icon(Icon::Globe)
                            .status_label("Active", Tone::Success),
                        NavItem::new("notes", "Design notes")
                            .icon(Icon::FileText)
                            .presence("ana")
                            .presence("ben"),
                    ],
                ),
                NavSection::new("drafts", "Drafts", Vec::new()).empty_caption("No drafts"),
                NavSection::new("archive", "Archive", vec![NavItem::new("old", "Old mail")])
                    .collapsed(),
            ])
            .selected("orders");
            Box::new(list)
        }),
        block("Rail", 76.0, 360.0, |_| {
            Box::new(
                NavList::new(vec![NavSection::untitled(
                    "destinations",
                    vec![
                        NavItem::new("home", "Home").icon(Icon::Home),
                        NavItem::new("subs", "Subscriptions").icon(Icon::Rss),
                        NavItem::new("connections", "Connections").icon(Icon::Link),
                        NavItem::new("later", "Watch later").icon(Icon::Clock),
                    ],
                )])
                .rail()
                .selected("subs"),
            )
        }),
        block("Dragging a row", 280.0, 220.0, |_| {
            let mut list = NavList::new(vec![NavSection::new(
                "playlists",
                "Playlists",
                vec![
                    NavItem::new("focus", "Focus").icon(Icon::ListMusic),
                    NavItem::new("running", "Running").icon(Icon::ListMusic),
                    NavItem::new("dinner", "Dinner").icon(Icon::ListMusic),
                    NavItem::new("road", "Road trip").icon(Icon::ListMusic),
                ],
            )])
            .on_move(|_, _| {});
            list.stage_drag("road", "running");
            Box::new(list)
        }),
        block("Breadcrumb, root icon", 480.0, 40.0, |_| {
            Box::new(
                Breadcrumb::new(vec![
                    "Home".into(),
                    "Projects".into(),
                    "Atlas".into(),
                    "Design".into(),
                ])
                .root_icon(Icon::Home),
            )
        }),
        block("Breadcrumb, truncated", 280.0, 40.0, |_| {
            Box::new(
                Breadcrumb::new(vec![
                    "Files".into(),
                    "Projects".into(),
                    "Atlas".into(),
                    "Design".into(),
                    "Exports".into(),
                    "Final".into(),
                ])
                .root_icon(Icon::HardDrive),
            )
        }),
    ]
}

/// Every glyph of the everyday icon set at the three icon sizes.
fn icon_blocks() -> Vec<Block> {
    vec![
        block("Small", 1200.0, 120.0, |_| icon_row(IconSize::Small)),
        block("Regular", 1200.0, 140.0, |_| icon_row(IconSize::Regular)),
        block("Large", 1200.0, 180.0, |_| icon_row(IconSize::Large)),
    ]
}

fn icon_row(size: IconSize) -> Element {
    let mut row = Row::new().with_gap(12.0);
    for icon in EVERYDAY_ICONS {
        row = row.push(Box::new(IconWidget::new(*icon).sized(size)));
    }
    Box::new(row)
}

const EVERYDAY_ICONS: &[Icon] = &[
    Icon::EyeOff,
    Icon::Phone,
    Icon::Link,
    Icon::MapPin,
    Icon::QrCode,
    Icon::ScanLine,
    Icon::SwitchCamera,
    Icon::User,
    Icon::UserPlus,
    Icon::Hash,
    Icon::Smile,
    Icon::SmilePlus,
    Icon::Video,
    Icon::PhoneOff,
    Icon::Lock,
    Icon::MessageSquare,
    Icon::Clock,
    Icon::AlarmClock,
    Icon::Hourglass,
    Icon::Bookmark,
    Icon::Tag,
    Icon::Rss,
    Icon::Upload,
    Icon::Type,
    Icon::Box,
    Icon::Contrast,
    Icon::Columns2,
    Icon::Palette,
    Icon::Crop,
    Icon::Usb,
    Icon::SdCard,
    Icon::CircleCheck,
    Icon::Disc3,
    Icon::Bold,
    Icon::Italic,
    Icon::Underline,
    Icon::Strikethrough,
    Icon::Superscript,
    Icon::Subscript,
    Icon::Heading1,
    Icon::Heading2,
    Icon::ListOrdered,
    Icon::Quote,
    Icon::ReplyAll,
    Icon::Forward,
    Icon::MailOpen,
    Icon::AlignLeft,
    Icon::AlignCenter,
    Icon::AlignRight,
    Icon::AlignJustify,
    Icon::BringToFront,
    Icon::SendToBack,
    Icon::Baseline,
    Icon::Highlighter,
    Icon::Table,
    Icon::ListTree,
    Icon::Shuffle,
    Icon::Repeat,
    Icon::Repeat1,
    Icon::Heart,
    Icon::ListMusic,
    Icon::Speaker,
    Icon::Headphones,
    Icon::Gauge,
    Icon::LocateFixed,
    Icon::Navigation,
    Icon::Flag,
    Icon::CornerUpLeft,
    Icon::CornerUpRight,
    Icon::ArrowUpLeft,
    Icon::ArrowUpRight,
    Icon::ArrowDownLeft,
    Icon::ArrowDownRight,
    Icon::Coffee,
    Icon::Utensils,
    Icon::Beer,
    Icon::Landmark,
    Icon::Trees,
    Icon::TrainFront,
    Icon::BedDouble,
    Icon::ShoppingBag,
    Icon::Hospital,
    Icon::Droplet,
    Icon::Wind,
    Icon::Cloud,
    Icon::CloudRain,
    Icon::CloudSnow,
    Icon::Moon,
    Icon::Hand,
    Icon::PawPrint,
    Icon::Plane,
    Icon::Trophy,
    Icon::Lightbulb,
];

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
                    .with_footer(Actions::new(Button::new("o", "Open"))),
                ),
                Box::new(
                    Card::new(Box::new(Text::new("Synced from your phone at 10:12.")))
                        .with_title("Field notes")
                        .with_footer(Actions::new(Button::new("o2", "Open"))),
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
        block("Media", 600.0, 700.0, |_| {
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
                "The server did not answer. Check the connection and try again in a moment.",
            ))
        }),
        block("Loading", 380.0, 220.0, |_| {
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(16.0)
                    .push(Box::new(Skeleton::row(
                        tilo_ui::widget::RowShape::TITLE
                            .led_by(tilo_ui::widget::RowLeadingShape::Avatar)
                            .with_secondary(1),
                    )))
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

/// The find band and a media control bar, painted as a painted host paints
/// them: the band across the prose, the bar over the photograph.
struct ChromeProbe {
    band: Rect,
    media: Rect,
    find: tilo_ui::widget::FindBar,
    bar: tilo_ui::widget::floating_surface::ControlBar,
}

impl Widget for ChromeProbe {
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
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use tilo_ui::widget::floating_surface::{BarControl, ControlBar};
        // The band stands at the foot of its area, as a find bar under a
        // document does.
        let height = self.find.height(theme);
        let band = Rect::new(
            self.band.x0,
            self.band.y1 - height,
            self.band.x1,
            self.band.y1,
        );
        self.find.layout(band, theme, tr);
        self.find.paint(scene, transform, tr, theme, presence);
        let controls = [
            BarControl::new(Icon::Camera, "Take photo"),
            BarControl::new(Icon::Mic, "Record sound").toggled(true),
            BarControl::new(Icon::Timer, "Timer"),
            BarControl::new(Icon::Settings, "Settings").enabled(false),
        ];
        let rect = ControlBar::bottom_centre(self.media, controls.len(), theme);
        self.bar.paint(scene, transform, tr, theme, rect, &controls);
    }
    fn name(&self) -> &'static str {
        "ChromeProbe"
    }
    fn get_rect(&self) -> Rect {
        self.band
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

fn overlays(theme: &Theme, out: &Path, viewport: (u32, u32)) {
    use tilo_ui::platform::Shortcut;
    let width = f64::from(viewport.0);
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
    context.push(MenuItem::selection("Open").with_shortcut(Shortcut::command("O")));
    context.push(MenuItem::selection("Rename").with_icon(Icon::Pencil));
    context.push(MenuItem::submenu(
        "New",
        vec![
            MenuItem::selection("Text file"),
            MenuItem::selection("Markdown file"),
            MenuItem::selection("Folder").with_shortcut(Shortcut::command("N").shift()),
        ],
    ));
    context.push(MenuItem::selection("Show hidden files").with_checked(true));
    context.push(MenuItem::separator());
    context.push(
        MenuItem::destructive_selection("Move to bin")
            .with_icon(Icon::Trash)
            .with_shortcut(Shortcut::key("Delete")),
    );
    let context_x = (width / 2.0).min(600.0);
    context.open_anchored(
        Rect::new(context_x, 60.0, context_x, 60.0),
        Rect::new(0.0, 0.0, width, f64::from(viewport.1)),
    );
    context.focus_item(2);
    context.open_submenu(2, false);
    let mut prompt = Popover::prompt("Go to offset", "0x1F or 31", "Hex with 0x, or decimal")
        .with_field_chars(16)
        .side(PopoverSide::Above);
    prompt.open_prompt(Rect::new(24.0, 360.0, 140.0, 384.0), "0x0400");
    let dropdown = Dropdown::from_state(
        vec!["Low".into(), "Medium".into(), "High".into()],
        &LocalState::new("Medium".to_owned()),
    )
    .open();
    let toolbar = Toolbar::new(Orientation::Horizontal)
        .push_icon_toggle(Icon::List, "Show outline", true, || {})
        .push_button("Compile", || {})
        .push_button("Reset", || {})
        .with_enabled(false)
        .push_separator()
        .push_icon(Icon::Minus, "Zoom out", || {})
        .push_icon(Icon::Plus, "Zoom in", || {})
        .joined()
        .push_menu(
            "Sort",
            vec![
                MenuItem::selection("By name"),
                MenuItem::selection("By date"),
            ],
        )
        .push_button("Export PDF", || {})
        .push_button("Print", || {});
    let mut find = tilo_ui::widget::FindBar::new().with_hex_mode();
    find.set_query("revenue");
    let chrome = ChromeProbe {
        band: Rect::new(160.0, 560.0, width - 24.0, f64::from(viewport.1) - 8.0),
        media: Rect::new(24.0, 24.0, width - 24.0, 324.0),
        find,
        bar: tilo_ui::widget::floating_surface::ControlBar::new(),
    };
    let mut root = busy_page()
        .push(Box::new(toolbar))
        .push(Box::new(
            Row::new()
                .push(Box::new(dropdown))
                .push(Box::new(Button::new("x", "Export"))),
        ))
        .push(Box::new(chrome))
        .push(Box::new(menu))
        .push(Box::new(detail))
        .push(Box::new(context))
        .push(Box::new(prompt));
    // A narrow window has no free corner for the tooltip.
    if width >= 1000.0 {
        root = root.push(Box::new(TooltipProbe {
            anchor: Rect::new(width - 300.0, 120.0, width - 268.0, 152.0),
            label: "Share this report",
        }));
    }
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(root), viewport, theme.clone());
    harness.settle_for_capture(SETTLE);
    harness.render_png(out);
    println!("wrote {}", out.display());
}

/// A three-way question whose title wraps, and a form taller than a short
/// window allows: it scrolls inside the sheet, under a fixed title and
/// footer.
fn modal_pages(theme: &Theme, out_question: &Path, out_tall: &Path) {
    let question = busy_page().push(Box::new(
        ModalDialog::new(
            "Save the changes you made to Quarterly report before closing it?",
            Text::new("Your changes will be lost if you don't save them."),
        )
        .open(true)
        .primary_action("Save", || {})
        .tertiary_action("Don't save", || {})
        .with_destructive_tertiary(true)
        .secondary_action("Cancel", || {}),
    ));
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(question), (1280, 640), theme.clone());
    harness.settle_for_capture(SETTLE);
    harness.render_png(out_question);
    println!("wrote {}", out_question.display());

    let mut section = tilo_ui::widget::FormSection::new("Server");
    for label in [
        "Name",
        "Host",
        "Port",
        "Database",
        "User",
        "Password",
        "Schema",
        "Timeout",
        "Certificate",
        "Notes",
    ] {
        section = section.field(Field::new(
            label,
            TextInput::local().with_placeholder(label),
        ));
    }
    let form = tilo_ui::widget::Form::new().section(section);

    let tall = busy_page().push(Box::new(
        ModalDialog::new("New connection", form)
            .open(true)
            .primary_action("Connect", || {})
            .with_primary_enabled(false)
            .secondary_action("Cancel", || {}),
    ));
    let mut harness =
        FrameHarness::new_with_theme(State::new(), Box::new(tall), (800, 600), theme.clone());
    harness.settle_for_capture(SETTLE);
    harness.render_png(out_tall);
    println!("wrote {}", out_tall.display());
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
    // An optional second argument names the pages to render, comma
    // separated (`lists,tables`); without it every page renders.
    let only: Option<Vec<String>> = args
        .get(1)
        .map(|pages| pages.split(',').map(str::to_owned).collect());
    let wanted = |page: &str| {
        only.as_ref()
            .is_none_or(|pages| pages.iter().any(|p| p == page))
    };
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let theme = theme.with_reduce_motion(true);
        let file = |page: &str| out.join(format!("{page}-{name}.png"));
        if wanted("lists") {
            block_page(&theme, &list_blocks(), &file("lists"));
            block_page_at(&theme, &list_blocks(), &file("lists-narrow"), 800.0);
        }
        if wanted("tables") {
            block_page(&theme, &table_blocks(), &file("tables"));
            block_page_at(&theme, &table_blocks(), &file("tables-narrow"), 800.0);
        }
        if only.is_some() {
            continue;
        }
        grid_page(&theme, CONTROL_COLS, &control_specs(), &file("controls"));
        grid_page(&theme, SIZE_COLS, &size_specs(), &file("sizes"));
        grid_page(&theme, INPUT_COLS, &input_specs(), &file("inputs"));
        // The controls again at 200 % text: labels, icons and bands grow.
        let large = theme.clone().with_text_scale(2.0);
        grid_page(
            &large,
            CONTROL_COLS,
            &control_specs(),
            &file("controls-large"),
        );
        grid_page(&large, INPUT_COLS, &input_specs(), &file("inputs-large"));
        grid_page(&theme, ROW_COLS, &row_specs(), &file("rows"));
        block_page(&theme, &pane_blocks(), &file("panes"));
        block_page(&theme, &navigation_blocks(), &file("navigation"));
        block_page(&theme, &icon_blocks(), &file("icons"));
        block_page(&theme, &content_blocks(), &file("content"));
        block_page(&theme, &editor_blocks(), &file("editor"));
        block_page(
            &theme.clone().with_text_scale(2.0),
            &editor_blocks(),
            &file("editor-large"),
        );
        overlays(&theme, &file("overlays"), (1280, 640));
        overlays(&theme, &file("overlays-narrow"), (800, 600));
        overlays(
            &theme.clone().with_reduce_transparency(true),
            &file("overlays-opaque"),
            (1280, 640),
        );
        modal(&theme, &file("modal"));
        modal_pages(&theme, &file("modal-question"), &file("modal-tall"));
    }
}

/// A painted host: a fixed-height leaf that paints a face across its width.
struct Painted {
    height: f64,
    paint: fn(&mut PaintCtx<'_>, Rect),
    rect: Rect,
}

impl Widget for Painted {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(self.height as f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    tilo_ui::rect_geometry!();
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        (self.paint)(&mut PaintCtx::new(scene, transform, text, theme), self.rect);
    }
    fn name(&self) -> &'static str {
        "Painted"
    }
}

/// A column with the content gutter a pane gives its lists, so lifts have
/// the room they bleed into.
fn gutter(content: Vec<Element>) -> Element {
    let mut column = Column::new()
        .with_horizontal_padding(16.0, 16.0)
        .with_intrinsic_height()
        .with_gap(16.0);
    for item in content {
        column = column.push(item);
    }
    Box::new(column)
}

fn list_blocks() -> Vec<Block> {
    vec![
        block("One content edge", 380.0, 330.0, |_| {
            gutter(vec![
                Box::new(Text::new("Rows, headers and sentences start on one edge.").dimmed()),
                Box::new(
                    Section::new(vec![
                        Box::new(
                            ListRow::new("a", "Inbox")
                                .icon(Icon::Inbox)
                                .meta("12")
                                .on_view(|| {}),
                        ),
                        Box::new(
                            ListRow::new("b", "Drafts")
                                .icon(Icon::FileText)
                                .selected(true)
                                .on_view(|| {}),
                        ),
                        Box::new(
                            ListRow::new("c", "Sent")
                                .icon(Icon::Send)
                                .selected(true)
                                .on_view(|| {}),
                        ),
                        Box::new(
                            ListRow::new("d", "Archive")
                                .icon(Icon::Archive)
                                .on_view(|| {}),
                        ),
                    ])
                    .with_header(
                        SectionHeader::group("Mailboxes")
                            .count(4)
                            .action(Button::new("clear", "Clear all")),
                    ),
                ),
                Box::new(StatusLine::active("Synced a minute ago")),
            ])
        }),
        block("Row parts", 440.0, 420.0, |_| {
            gutter(vec![Box::new(Section::new(vec![
                Box::new(
                    ListRow::new("wifi", "Wi-Fi")
                        .secondary("Home network")
                        .icon(Icon::Wifi)
                        .trailing(Box::new(Toggle::new("wifi-on", "wifi"))),
                ),
                Box::new(
                    ListRow::new("tokyo", "Tokyo")
                        .secondary("Tomorrow, 7 hours ahead")
                        .title_value("02:41"),
                ),
                Box::new(
                    ListRow::new("plan", "Atlas plan")
                        .mark(Icon::Star)
                        .tags(&[
                            ("Draft", ChipStyle::Neutral),
                            ("Shared", ChipStyle::Tinted(Tone::Info)),
                        ])
                        .meta("Yesterday"),
                ),
                Box::new(
                    ListRow::new("aapl", "Apple")
                        .badge("AAPL")
                        .meta_spans(&[("182.10", Tone::Neutral), ("+2.4%", Tone::Success)]),
                ),
                Box::new(
                    ListRow::new("thread", "Ana Lima")
                        .avatar("Ana Lima", "ana")
                        .subtitle("Launch checklist")
                        .secondary(
                            "The walkthrough is recorded; only the notes are left before we ship.",
                        )
                        .meta("09:12")
                        .count(4)
                        .emphasized(true),
                ),
                Box::new(ListRow::new("serif", "Serif").checked(true).on_view(|| {})),
                Box::new(
                    ListRow::new("mono", "Monospace")
                        .checked(false)
                        .with_enabled(false),
                ),
            ]))])
        }),
        block("Leading column", 380.0, 330.0, |_| {
            gutter(vec![Box::new(Section::new(vec![
                Box::new(
                    ListRow::new("get", "/users")
                        .badge("GET")
                        .leading_tooltip("Reads the resource"),
                ),
                Box::new(ListRow::new("del", "/users/42").badge("DELETE")),
                Box::new(
                    ListRow::new("site", "example.org")
                        .avatar("Example", "example.org")
                        .square(),
                ),
                Box::new(
                    ListRow::new("photo", "Coastline")
                        .thumbnail(art().clone())
                        .secondary("2.4 MB"),
                ),
                Box::new(
                    ListRow::new("clip", "Harbour at dusk")
                        .media(MediaSource::Placeholder, tilo_ui::widget::MediaAspect::Wide)
                        .secondary("4:12"),
                ),
                Box::new(ListRow::new("pick", "Select message").checkbox(true, |_| {})),
                Box::new(
                    ListRow::new("cal", "Work")
                        .swatch(vello::peniko::Color::from_rgb8(0x4a, 0x90, 0xd9)),
                ),
            ]))])
        }),
        block("Folding sections", 380.0, 260.0, |_| {
            gutter(vec![
                Box::new(
                    Section::new(vec![
                        Box::new(ListRow::new("w1", "Notes").icon(Icon::FileText)),
                        Box::new(ListRow::new("w2", "Mail").icon(Icon::Mail)),
                    ])
                    .with_header(SectionHeader::group("This workspace").count(2))
                    .collapsible(),
                ),
                Box::new(
                    Section::new(vec![Box::new(
                        ListRow::new("w3", "Terminal").icon(Icon::Terminal),
                    )])
                    .with_header(SectionHeader::group("Other workspaces").count(5))
                    .collapsed(),
                ),
            ])
        }),
        block("Property lists", 440.0, 420.0, |_| {
            gutter(vec![
                Box::new(
                    ObjectHeader::new("Ada Lovelace")
                        .avatar("Ada Lovelace", "ada@example.org")
                        .caption("ada@example.org"),
                ),
                Box::new(Section::new(vec![
                    Box::new(PropertyList::sections(vec![
                        (
                            "Contact",
                            vec![
                                Property::new("Email", "ada@example.org")
                                    .with_kind(tilo_ui::text_render::TextKind::Identifier)
                                    .copyable(),
                                Property::new("Phone", "+44 20 7946 0958")
                                    .with_kind(tilo_ui::text_render::TextKind::Numeric),
                            ],
                        ),
                        (
                            "Account",
                            vec![
                                Property::new("Key", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl")
                                    .with_kind(tilo_ui::text_render::TextKind::Identifier)
                                    .copyable()
                                    .action(Icon::Eye, "Reveal", || {}),
                                Property::new("Certificate", "Expired 3 days ago").tone(Tone::Danger),
                                Property::new("Notes", "Prefers the morning for calls; the engine draft is in the shared folder."),
                            ],
                        ),
                    ])),
                ])),
            ])
        }),
        block("Inspector, compact", 320.0, 260.0, |_| {
            gutter(vec![Box::new(Section::new(vec![Box::new(
                PropertyList::new(vec![
                    Property::new("Offset", "0x0004_2A10")
                        .with_kind(tilo_ui::text_render::TextKind::Numeric),
                    Property::new("Type", "u32 little endian"),
                    Property::new("Value", "1,048,576")
                        .with_kind(tilo_ui::text_render::TextKind::Numeric),
                    Property::new("Selection", "16 bytes"),
                ])
                .compact()
                .striped(),
            )]))])
        }),
        block("Chips", 380.0, 110.0, |_| {
            gutter(vec![Box::new(
                Row::new()
                    .push(Box::new(
                        Chip::new("design").with_id("tag-design").on_click(|| {}),
                    ))
                    .push(Box::new(Chip::new("Sorted by date").menu().on_click(|| {})))
                    .push(Box::new(Chip::new("Unread").selected(true))),
            )])
        }),
    ]
}

#[derive(Clone)]
struct Proc {
    name: &'static str,
    depth: usize,
    children: bool,
    open: bool,
    cpu: &'static str,
    level: (&'static str, Tone),
    on: bool,
}

fn procs() -> Vec<Proc> {
    let p = |name, depth, children, cpu, level, on| Proc {
        name,
        depth,
        children,
        open: children,
        cpu,
        level,
        on,
    };
    vec![
        p("systemd", 0, true, "0.1", ("Info", Tone::Info), true),
        p("tilo-shell", 1, true, "4.2", ("Info", Tone::Info), true),
        p(
            "compositor",
            2,
            false,
            "2.8",
            ("Warning", Tone::Warning),
            false,
        ),
        p(
            "sync-daemon",
            1,
            false,
            "0.6",
            ("Error", Tone::Danger),
            true,
        ),
        p("journald", 1, false, "0.2", ("Info", Tone::Info), false),
    ]
}

fn table_blocks() -> Vec<Block> {
    vec![
        block("Table", 560.0, 220.0, |_| gutter(vec![table()])),
        block("Tree, badges and boxes", 560.0, 300.0, |_| {
            let mut t = DataTable::new(vec![
                ColumnDef::checkbox("on", "On", |p: &Proc| p.on),
                ColumnDef::new("name", "Process", ColumnWidth::Flex(1.0))
                    .with_text(|p: &Proc| p.name.to_owned()),
                ColumnDef::new("level", "Level", ColumnWidth::Auto)
                    .with_badge(|p: &Proc| (p.level.0.to_owned(), p.level.1)),
                ColumnDef::new("cpu", "CPU %", ColumnWidth::Auto)
                    .with_kind(tilo_ui::text_render::TextKind::Numeric)
                    .with_text(|p: &Proc| p.cpu.to_owned()),
            ])
            .selectable(true)
            .tree(|p: &Proc| p.depth, |p| p.children, |p| p.open, |_, _| {})
            .with_add_row("Add a process", || {});
            t.set_rows(procs());
            t.selection_mut().select_only(1);
            gutter(vec![Box::new(t)])
        }),
        block("Compact, narrow", 320.0, 240.0, |_| {
            let mut t = DataTable::new(vec![
                ColumnDef::new("name", "Process", ColumnWidth::Auto)
                    .with_text(|p: &Proc| p.name.to_owned()),
                ColumnDef::new("level", "Level", ColumnWidth::Fixed(110.0))
                    .with_badge(|p: &Proc| (p.level.0.to_owned(), p.level.1))
                    .priority(1),
                ColumnDef::new("cpu", "CPU %", ColumnWidth::Fixed(90.0))
                    .with_kind(tilo_ui::text_render::TextKind::Numeric)
                    .with_text(|p: &Proc| p.cpu.to_owned()),
                ColumnDef::new("path", "Path", ColumnWidth::Fixed(200.0))
                    .with_text(|p: &Proc| format!("/usr/bin/{}", p.name)),
            ])
            .compact();
            t.set_rows(procs());
            gutter(vec![Box::new(t)])
        }),
        block("Painted table", 560.0, 220.0, |_| {
            gutter(vec![Box::new(Painted {
                height: 200.0,
                rect: Rect::ZERO,
                paint: |ctx, rect| {
                    use tilo_ui::widget::data_table::{DataTableFace, FaceColumn};
                    let columns = [
                        FaceColumn::new("Title", ColumnWidth::Flex(1.0)),
                        FaceColumn::new("Artist", ColumnWidth::Auto),
                        FaceColumn::new("Time", ColumnWidth::Auto)
                            .with_kind(tilo_ui::text_render::TextKind::Numeric),
                    ];
                    let rows: [&[&str]; 4] = [
                        &["So what", "Miles Davis", "9:22"],
                        &["Freddie Freeloader", "Miles Davis", "9:46"],
                        &["Blue in green", "Miles Davis", "5:37"],
                        &["All blues", "Miles Davis", "11:33"],
                    ];
                    DataTableFace::new(&columns, &rows)
                        .selected(Some(2))
                        .hovered(Some(0))
                        .sorted_by(0, true)
                        .paint(ctx, rect);
                },
            })])
        }),
    ]
}
