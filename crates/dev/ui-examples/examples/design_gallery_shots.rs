#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    reason = "a capture scenario lays a fixed gallery out in small, bounded pixel math"
)]

//! The design system's gallery (`docs/design-language.md`, "Primitives"):
//! every primitive in every state (rest, hover, pressed, focused, disabled,
//! on, selected), dark and light, and the rows again at compact density.
//!
//! Pages, each `<page>-<theme>.png`:
//!
//! - `surfaces`: the five levels in place (ground, a window with a region,
//!   raised wells and a floating menu) and the colour roles: accent, accent
//!   ink, on state, neutral fill and the four status tones.
//! - `controls`: buttons by role, icon buttons, bar controls, switch,
//!   checkbox, radio, segmented, slider and chips, in every state.
//! - `rows`: list rows (icon 16, avatar 24, avatar 40), text stacks, facts,
//!   key hints, a property list and disclosure rows; `rows-compact` the
//!   same at compact density.
//! - `panels`: a floating panel with an embedded field, rows and a key-hint
//!   footer; a menu; a banner and an empty state.
//!
//! Run: `cargo run -p ui-examples --example ui-shots -- design_gallery [OUT_DIR]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use tilo_ui::icons::Icon;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::paint::PaintCtx;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::{Density, TextRole};
use tilo_ui::widget::toggle_group::SegmentedFace;
use tilo_ui::widget::{
    AvatarFace, BannerContent, BannerKind, BarControlFace, Button, ButtonFace, ButtonRole,
    CheckboxFace, ChipFace, ChipStyle, ControlSize, DisclosureRow, EmptyState, EmptyStateContent,
    Fact, FactsFace, FloatingPanelFace, IconButton, Interaction, KeyHint, KeyHintsFace,
    ListRowFace, PropertyListFace, PropertyRow, RowLeading, SliderFace, SurfaceLevel, Text,
    TextStackFace, Toggle, ToggleFace, Widget, paint_surface,
};

const PAGE_W: f64 = 1280.0;
const MARGIN: f64 = 40.0;
const LABEL_W: f64 = 176.0;

/// The states a grid row shows, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
    Rest,
    Hover,
    Pressed,
    Focused,
    Disabled,
    On,
}

const STATES: [St; 6] = [
    St::Rest,
    St::Hover,
    St::Pressed,
    St::Focused,
    St::Disabled,
    St::On,
];

impl St {
    fn label(self) -> &'static str {
        match self {
            Self::Rest => "Rest",
            Self::Hover => "Hover",
            Self::Pressed => "Pressed",
            Self::Focused => "Focused",
            Self::Disabled => "Disabled",
            Self::On => "On or selected",
        }
    }
}

/// A page being drawn: its scene, text renderer, theme and the next free y.
struct Page {
    scene: Scene,
    tr: TextRenderer,
    theme: Theme,
    y: f64,
    /// Retained widgets drawn on the page, kept alive until it renders.
    kept: Vec<Box<dyn Widget>>,
}

impl Page {
    fn new(theme: &Theme) -> Self {
        Self {
            scene: Scene::new(),
            tr: TextRenderer::new(),
            theme: theme.clone(),
            y: MARGIN,
            kept: Vec::new(),
        }
    }

    fn ctx(&mut self) -> PaintCtx<'_> {
        PaintCtx::new(&mut self.scene, Affine::IDENTITY, &mut self.tr, &self.theme)
    }

    fn text(&mut self, s: &str, role: TextRole, ink: Color, at: Point) -> (f64, f64) {
        let style = self.theme.text(role);
        let (w, h) = self.tr.measure_style(s, None, &style);
        self.tr.draw_style(
            &mut self.scene,
            s,
            ink,
            Affine::translate(at.to_vec2()),
            None,
            &style,
        );
        (f64::from(w), f64::from(h))
    }

    /// A section title in `Heading`, with section space above.
    fn section(&mut self, title: &str) {
        if self.y > MARGIN {
            self.y += 24.0;
        }
        let ink = self.theme.text_primary;
        let (_, h) = self.text(title, TextRole::Heading, ink, Point::new(MARGIN, self.y));
        self.y += h + 16.0;
    }

    /// The state column headers.
    fn state_header(&mut self) {
        let ink = self.theme.text_tertiary;
        for (i, st) in STATES.iter().enumerate() {
            let x = col_x(i, STATES.len());
            self.text(st.label(), TextRole::Caption, ink, Point::new(x, self.y));
        }
        self.y += 28.0;
    }

    /// One grid row: a label, then `paint` once per state column.
    fn grid_row(
        &mut self,
        label: &str,
        height: f64,
        mut paint: impl FnMut(&mut PaintCtx<'_>, St, Rect),
    ) {
        let ink = self.theme.text_dim;
        let style = self.theme.text(TextRole::Label);
        let (_, lh) = self.tr.measure_style(label, None, &style);
        self.text(
            label,
            TextRole::Label,
            ink,
            Point::new(MARGIN, self.y + (height - f64::from(lh)) / 2.0),
        );
        let n = STATES.len();
        for (i, st) in STATES.iter().enumerate() {
            let x = col_x(i, n);
            let cell = Rect::new(x, self.y, x + col_w(n) - 16.0, self.y + height);
            paint(&mut self.ctx(), *st, cell);
        }
        self.y += height + 12.0;
    }

    fn finish(self, out: &Path) {
        let height = self.y + MARGIN;
        let mut renderer = OffscreenRenderer::new(PAGE_W as u32, height.ceil() as u32);
        renderer.render_png(&self.scene, self.theme.tokens.colors.surfaces.content, out);
        println!("wrote {}", out.display());
    }
}

fn col_w(n: usize) -> f64 {
    (PAGE_W - 2.0 * MARGIN - LABEL_W) / n as f64
}

fn col_x(i: usize, n: usize) -> f64 {
    MARGIN + LABEL_W + i as f64 * col_w(n)
}

/// A rect of `w` × `h` at the vertical middle of `cell`'s leading edge.
fn lead(cell: Rect, w: f64, h: f64) -> Rect {
    let cy = cell.center().y;
    Rect::new(cell.x0, cy - h / 2.0, cell.x0 + w, cy + h / 2.0)
}

fn interaction(st: St) -> Interaction {
    Interaction {
        hovered: matches!(st, St::Hover | St::Pressed),
        pressed: st == St::Pressed,
        focused: st == St::Focused,
        disabled: st == St::Disabled,
    }
}

fn button_cell(
    ctx: &mut PaintCtx<'_>,
    st: St,
    cell: Rect,
    role: ButtonRole,
    label: &str,
    icon: Option<Icon>,
) {
    if st == St::On {
        return;
    }
    let mut face = ButtonFace::new(cell, label).role(role);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    let w = face.intrinsic_width(ctx.text, ctx.theme);
    let h = ButtonFace::height(ControlSize::Regular, ctx.theme);
    let rect = lead(cell, w, h);
    let mut face = ButtonFace::new(rect, label)
        .role(role)
        .hovered(matches!(st, St::Hover | St::Pressed))
        .pressed(st == St::Pressed)
        .focused(st == St::Focused)
        .disabled(st == St::Disabled);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
}

fn icon_button_cell(ctx: &mut PaintCtx<'_>, st: St, cell: Rect, size: ControlSize) {
    let edge = size.height(ctx.theme);
    let rect = lead(cell, edge, edge);
    let face = IconButton::face(rect, Icon::Pencil, "Rename")
        .control_size(size)
        .toggled(st == St::On)
        .hovered(matches!(st, St::Hover | St::Pressed))
        .pressed(st == St::Pressed)
        .focused(st == St::Focused)
        .disabled(st == St::Disabled);
    Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
}

fn bar_control_cell(ctx: &mut PaintCtx<'_>, st: St, cell: Rect, which: u8) {
    if st == St::Disabled {
        return;
    }
    // A bar control stands on its bar: the desktop ground here.
    let ground = ctx.theme.tokens.colors.surfaces.base;
    let band = Rect::new(cell.x0 - 6.0, cell.y0, cell.x1, cell.y1);
    ctx.scene.fill(
        vello::peniko::Fill::NonZero,
        ctx.transform,
        ground,
        None,
        &vello::kurbo::RoundedRect::from_rect(band, 8.0),
    );
    let on_bar = ctx.theme.on_ground(ground);
    let face = match which {
        0 => BarControlFace::new("Quick settings").icon(Icon::Wifi),
        1 => BarControlFace::new("Workspace")
            .label("Studio")
            .trailing(Icon::ChevronDown),
        _ => BarControlFace::new("Search")
            .icon(Icon::Search)
            .label("Search"),
    }
    .hovered(st == St::Hover)
    .pressed(st == St::Pressed)
    .focused(st == St::Focused)
    .open(st == St::On);
    let w = face.width(ctx.text, &on_bar);
    let rect = BarControlFace::place(cell.x0, cell.center().y, w, &on_bar);
    let mut inner = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &on_bar);
    face.paint(&mut inner, rect, ground);
}

fn controls(theme: &Theme, out: &Path) {
    let mut p = Page::new(theme);
    p.section("Buttons");
    p.state_header();
    p.grid_row("Primary", 40.0, |c, st, r| {
        button_cell(c, st, r, ButtonRole::Primary, "Save", None);
    });
    p.grid_row("Secondary", 40.0, |c, st, r| {
        button_cell(c, st, r, ButtonRole::Secondary, "Discard", None);
    });
    p.grid_row("Quiet", 40.0, |c, st, r| {
        button_cell(c, st, r, ButtonRole::Plain, "Details", None);
    });
    p.grid_row("Destructive", 40.0, |c, st, r| {
        button_cell(c, st, r, ButtonRole::Destructive, "Delete", None);
    });
    p.grid_row("With icon", 40.0, |c, st, r| {
        button_cell(c, st, r, ButtonRole::Secondary, "Share", Some(Icon::Users));
    });
    p.grid_row("Icon button", 40.0, |c, st, r| {
        icon_button_cell(c, st, r, ControlSize::Regular);
    });
    p.grid_row("Icon button, small", 36.0, |c, st, r| {
        icon_button_cell(c, st, r, ControlSize::Small);
    });

    p.section("Bar controls");
    p.state_header();
    for (label, which) in [("Icon", 0), ("Label and menu", 1), ("Icon and label", 2)] {
        p.grid_row(label, 40.0, |c, st, r| bar_control_cell(c, st, r, which));
    }

    p.section("Choices");
    p.state_header();
    p.grid_row("Switch, off", 36.0, |c, st, r| {
        let (w, h) = ToggleFace::size(c.theme);
        let face = ToggleFace::new(lead(r, w, h), st == St::On)
            .hovered(matches!(st, St::Hover | St::Pressed))
            .focused(st == St::Focused)
            .disabled(st == St::Disabled);
        Toggle::paint(c.scene, c.transform, c.theme, &face);
    });
    p.grid_row("Switch, on", 36.0, |c, st, r| {
        let (w, h) = ToggleFace::size(c.theme);
        let face = ToggleFace::new(lead(r, w, h), true)
            .hovered(matches!(st, St::Hover | St::Pressed))
            .focused(st == St::Focused)
            .disabled(st == St::Disabled);
        Toggle::paint(c.scene, c.transform, c.theme, &face);
    });
    for (label, radio, on) in [
        ("Checkbox", false, false),
        ("Checkbox, checked", false, true),
        ("Radio", true, false),
        ("Radio, chosen", true, true),
    ] {
        p.grid_row(label, 32.0, |c, st, r| {
            let face = CheckboxFace::new(lead(r, 18.0, 18.0), on || st == St::On)
                .hovered(matches!(st, St::Hover | St::Pressed))
                .focused(st == St::Focused)
                .disabled(st == St::Disabled);
            let face = if radio { face.radio() } else { face };
            tilo_ui::widget::Checkbox::paint(c.scene, c.transform, c.theme, &face);
            let ink = c.theme.text_primary;
            let style = c.theme.text(TextRole::Body);
            let (_, h) = c.text.measure_style("Sync", None, &style);
            c.text.draw_style(
                c.scene,
                if radio { "Weekly" } else { "Sync" },
                ink,
                Affine::translate((r.x0 + 28.0, r.center().y - f64::from(h) / 2.0)),
                None,
                &style,
            );
        });
    }
    p.grid_row("Segmented", 40.0, |c, st, r| {
        let labels = ["List", "Grid"];
        let w = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, c.text, c.theme);
        let h = SegmentedFace::height(ControlSize::Regular, c.theme);
        let strip = lead(r, w.min(r.width()), h);
        let segs = SegmentedFace::partition(strip, &labels, ControlSize::Regular, c.text, c.theme);
        let face = SegmentedFace::new(&segs, &labels, usize::from(st == St::On))
            .hovered(matches!(st, St::Hover | St::Pressed).then_some(2))
            .focused(st == St::Focused)
            .disabled(st == St::Disabled);
        tilo_ui::widget::ToggleGroup::paint(c.scene, c.transform, c.text, c.theme, &face);
    });
    p.grid_row("Slider", 32.0, |c, st, r| {
        let face = SliderFace::new(
            lead(r, r.width() - 8.0, 24.0),
            if st == St::On { 0.8 } else { 0.4 },
        )
        .hovered(st == St::Hover)
        .dragging(st == St::Pressed)
        .focused(st == St::Focused)
        .disabled(st == St::Disabled);
        tilo_ui::widget::Slider::paint(c.scene, c.transform, c.theme, &face);
    });
    p.grid_row("Filter chip", 36.0, |c, st, r| {
        if st == St::Disabled || st == St::Pressed {
            return;
        }
        let lead_kind = tilo_ui::widget::ChipLead::None;
        let w = ChipFace::intrinsic_width("Images", lead_kind, false, c.text, c.theme);
        let face = ChipFace::new(lead(r, w, ChipFace::height(c.theme)), "Images")
            .selected(st == St::On)
            .hovered(st == St::Hover)
            .focused(st == St::Focused);
        face.paint(c.scene, c.transform, c.text, c.theme);
    });
    p.grid_row("Tag, person", 36.0, |c, st, r| {
        if st != St::Rest {
            return;
        }
        let lead_kind = tilo_ui::widget::ChipLead::None;
        let w = ChipFace::intrinsic_width("Design", lead_kind, false, c.text, c.theme);
        let face = ChipFace::new(lead(r, w, ChipFace::height(c.theme)), "Design").style(
            ChipStyle::Tinted(tilo_ui::widget::Tone::Presence(
                tilo_ui::tokens::PresenceHue::Teal,
            )),
        );
        face.paint(c.scene, c.transform, c.text, c.theme);
    });
    p.finish(out);
}

fn row_face(kind: u8, title: &str) -> ListRowFace<'_> {
    match kind {
        0 => ListRowFace::new(title)
            .leading(RowLeading::Icon(Icon::Wifi))
            .value("Home"),
        1 => ListRowFace::new(title)
            .leading(RowLeading::Icon(Icon::Folder))
            .secondary("Edited yesterday by Ana")
            .disclosure(),
        2 => {
            ListRowFace::new(title).leading(RowLeading::Avatar(AvatarFace::new("Ana Lima", "ana")))
        }
        _ => ListRowFace::new(title)
            .leading(RowLeading::Avatar(AvatarFace::new("Ben Okafor", "ben")))
            .secondary("Editing Budget 2027"),
    }
}

fn rows(theme: &Theme, out: &Path) {
    let mut p = Page::new(theme);
    p.section("List rows");
    p.state_header();
    let states = |st: St| (st == St::On, interaction(st));
    for (label, kind, title) in [
        ("Icon, value", 0, "Wi-Fi"),
        ("Icon, two lines", 1, "Quarterly report"),
        ("Avatar 24", 2, "Ana Lima"),
        ("Avatar 40, two lines", 3, "Ben Okafor"),
    ] {
        let height = row_face(kind, title).height(&mut p.tr, &p.theme);
        p.grid_row(label, height, |c, st, r| {
            if st == St::Disabled {
                return;
            }
            let (selected, inter) = states(st);
            let face = row_face(kind, title).selected(selected).interaction(inter);
            face.paint(c, r);
        });
    }

    p.section("Text and facts");
    let x = MARGIN + LABEL_W;
    let w = 420.0;
    let examples: [(&str, TextStackFace<'_>); 3] = [
        (
            "Text stack",
            TextStackFace::new("Budget 2027").secondary("Spreadsheet in Finance"),
        ),
        (
            "With tertiary",
            TextStackFace::new("Budget 2027")
                .secondary("Spreadsheet in Finance")
                .tertiary("Ana is editing"),
        ),
        (
            "Dense",
            TextStackFace::new("Budget 2027")
                .secondary("Spreadsheet")
                .dense(),
        ),
    ];
    for (label, stack) in examples {
        let h = stack.height(&mut p.tr, &p.theme);
        let ink = p.theme.text_dim;
        p.text(label, TextRole::Label, ink, Point::new(MARGIN, p.y));
        let rect = Rect::new(x, p.y, x + w, p.y + h);
        stack.paint(&mut p.ctx(), rect);
        p.y += h + 16.0;
    }
    let facts = [
        Fact::with_icon(Icon::File, "12 files").priority(2),
        Fact::text("3.4 MB"),
        Fact::with_icon(Icon::Clock, "Edited today").priority(1),
    ];
    for (label, width) in [("Facts", 420.0), ("Facts, narrow", 150.0)] {
        let ink = p.theme.text_dim;
        p.text(label, TextRole::Label, ink, Point::new(MARGIN, p.y));
        let rect = Rect::new(x, p.y, x + width, p.y + 20.0);
        FactsFace::new(&facts).paint(&mut p.ctx(), rect);
        p.y += 36.0;
    }
    let hints = [
        KeyHint::new(["Enter"], "Open"),
        KeyHint::new(["Ctrl", "Enter"], "Open in new tab"),
        KeyHint::new(["Esc"], "Close"),
    ];
    let ink = p.theme.text_dim;
    p.text("Key hints", TextRole::Label, ink, Point::new(MARGIN, p.y));
    let h = KeyHintsFace::height(&p.theme);
    let rect = Rect::new(x, p.y, x + 700.0, p.y + h);
    KeyHintsFace::new(&hints).paint(&mut p.ctx(), rect);
    p.y += h + 24.0;

    let props = [
        PropertyRow::new("Kind", "Spreadsheet"),
        PropertyRow::new("Size", "3.4 MB"),
        PropertyRow::new("Owner", "Ana Lima"),
        PropertyRow::new("Modified", "Today, 14:02"),
    ];
    let ink = p.theme.text_dim;
    p.text(
        "Property list",
        TextRole::Label,
        ink,
        Point::new(MARGIN, p.y),
    );
    let face = PropertyListFace::new(&props);
    let ph = face.height(&mut p.tr, &p.theme, 480.0);
    let rect = Rect::new(x, p.y, x + 480.0, p.y + ph);
    face.paint(&mut p.ctx(), rect);
    p.y += ph + 16.0;

    p.section("Disclosure rows");
    let ink = p.theme.text_dim;
    let body = || -> Box<dyn Widget> {
        Box::new(Text::new(
            "Networks you joined appear here, strongest first.",
        ))
    };
    let closed = DisclosureRow::new("Known networks", body()).icon(Icon::Wifi);
    let open = DisclosureRow::new("Saved devices", body())
        .icon(Icon::Bluetooth)
        .secondary("3 devices")
        .opened();
    for (label, row) in [("Closed", closed), ("Open", open)] {
        p.text(label, TextRole::Label, ink, Point::new(MARGIN, p.y + 8.0));
        let area = Rect::new(x, p.y, x + 520.0, p.y + 200.0);
        let h = stamp(&mut p, Box::new(row), area);
        p.y += h + 12.0;
    }
    p.finish(out);
}

/// Lay a retained widget out in `area` at its own height and draw it.
/// Returns its height.
fn stamp(p: &mut Page, mut widget: Box<dyn Widget>, area: Rect) -> f64 {
    widget.set_motion_reduced(true);
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = widget.build_layout(&mut taffy, &p.theme, &mut nodes);
    if let Ok(style) = taffy.style(root) {
        let mut style = style.clone();
        style.size.width = taffy::style_helpers::length(area.width() as f32);
        let _ = taffy.set_style(root, style);
    }
    let tr = &mut p.tr;
    let _ = taffy.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(area.width() as f32),
            height: AvailableSpace::MaxContent,
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(Size::ZERO, |&w| {
                // SAFETY: the node map was filled by `build_layout` above
                // from widgets `widget` owns, and nothing else touches them
                // while layout runs.
                unsafe { (*w).measure(known, available, tr) }
            })
        },
    );
    widget.apply_layout(&taffy, root, area.origin());
    // A disclosure learns its body's height from its first layout.
    let root = widget.build_layout(&mut taffy, &p.theme, &mut nodes);
    if let Ok(style) = taffy.style(root) {
        let mut style = style.clone();
        style.size.width = taffy::style_helpers::length(area.width() as f32);
        let _ = taffy.set_style(root, style);
    }
    let _ = taffy.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(area.width() as f32),
            height: AvailableSpace::MaxContent,
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(Size::ZERO, |&w| {
                // SAFETY: as above.
                unsafe { (*w).measure(known, available, tr) }
            })
        },
    );
    widget.apply_layout(&taffy, root, area.origin());
    let presence = PresenceCtx::default();
    widget.draw(
        &mut p.scene,
        Affine::IDENTITY,
        &mut p.tr,
        &p.theme,
        &presence,
    );
    let h = widget.get_rect().height();
    p.kept.push(widget);
    h
}

fn surfaces(theme: &Theme, out: &Path) {
    let mut p = Page::new(theme);
    p.section("Surfaces");
    // The desktop ground, a window on it with a region and raised wells,
    // and a floating menu over the window.
    let ground = Rect::new(MARGIN, p.y, PAGE_W - MARGIN, p.y + 560.0);
    let _ = paint_surface(
        &mut p.scene,
        Affine::IDENTITY,
        ground,
        SurfaceLevel::Ground,
        &p.theme,
    );
    let window = Rect::new(
        ground.x0 + 40.0,
        ground.y0 + 40.0,
        ground.x0 + 860.0,
        ground.y1 - 40.0,
    );
    let in_window = paint_surface(
        &mut p.scene,
        Affine::IDENTITY,
        window,
        SurfaceLevel::Window,
        &p.theme,
    );
    // The region: the window's leading column, clipped to the window.
    let region = Rect::new(window.x0, window.y0, window.x0 + 240.0, window.y1);
    p.scene.push_layer(
        vello::peniko::Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        Affine::IDENTITY,
        &vello::kurbo::RoundedRect::from_rect(window, f64::from(p.theme.tokens.radii.tile)),
    );
    let in_region = paint_surface(
        &mut p.scene,
        Affine::IDENTITY,
        region,
        SurfaceLevel::Region,
        &p.theme,
    );
    p.scene.pop_layer();
    {
        let mut c = PaintCtx::new(&mut p.scene, Affine::IDENTITY, &mut p.tr, &in_region);
        let bleed = tilo_ui::chrome::row_bleed(&in_region);
        let mut y = region.y0 + 20.0;
        for (i, (icon, name)) in [
            (Icon::Calendar, "Today"),
            (Icon::Folder, "Projects"),
            (Icon::Users, "People"),
            (Icon::Bell, "Activity"),
        ]
        .into_iter()
        .enumerate()
        {
            let face = ListRowFace::new(name)
                .leading(RowLeading::Icon(icon))
                .selected(i == 1)
                .interaction(Interaction {
                    hovered: i == 3,
                    ..Interaction::default()
                });
            let h = face.height(c.text, c.theme);
            face.paint(
                &mut c,
                Rect::new(region.x0 + 16.0 - bleed, y, region.x1 - 16.0 + bleed, y + h),
            );
            y += h;
        }
    }
    {
        let content = Rect::new(
            region.x1 + 24.0,
            window.y0 + 24.0,
            window.x1 - 24.0,
            window.y1 - 24.0,
        );
        let c = PaintCtx::new(&mut p.scene, Affine::IDENTITY, &mut p.tr, &in_window);
        let style = in_window.text(TextRole::Title);
        c.text.draw_style(
            c.scene,
            "Projects",
            in_window.text_primary,
            Affine::translate((content.x0, content.y0)),
            None,
            &style,
        );
        // A raised field well and a raised object.
        let field = Rect::new(content.x0, content.y0 + 48.0, content.x1, content.y0 + 80.0);
        tilo_ui::widget::FieldFace::new(field, "")
            .placeholder("Name the project")
            .paint(c.scene, c.transform, c.text, &in_window);
        let card = Rect::new(
            content.x0,
            content.y0 + 100.0,
            content.x0 + 260.0,
            content.y0 + 196.0,
        );
        let on_card = paint_surface(c.scene, c.transform, card, SurfaceLevel::Raised, &in_window);
        TextStackFace::new("Budget 2027")
            .secondary("Spreadsheet")
            .tertiary("Ana is editing")
            .paint(
                &mut PaintCtx::new(c.scene, c.transform, c.text, &on_card),
                card.inset(-16.0),
            );
        let save = Rect::new(
            content.x0,
            content.y0 + 220.0,
            content.x0 + 80.0,
            content.y0 + 252.0,
        );
        let face = ButtonFace::new(save, "Create").role(ButtonRole::Primary);
        let w = face.intrinsic_width(c.text, &in_window);
        let face = ButtonFace::new(Rect::new(save.x0, save.y0, save.x0 + w, save.y1), "Create")
            .role(ButtonRole::Primary);
        Button::paint(c.scene, c.transform, c.text, &in_window, &face);
        let cancel_x = save.x0 + w + 8.0;
        let face = ButtonFace::new(
            Rect::new(cancel_x, save.y0, cancel_x + 90.0, save.y1),
            "Cancel",
        );
        let w2 = face.intrinsic_width(c.text, &in_window);
        let face = ButtonFace::new(
            Rect::new(cancel_x, save.y0, cancel_x + w2, save.y1),
            "Cancel",
        )
        .role(ButtonRole::Secondary);
        Button::paint(c.scene, c.transform, c.text, &in_window, &face);
    }
    // A floating menu over the window's edge.
    let menu_rows = ["Rename", "Duplicate", "Share", "Delete"];
    let row_h = f64::from(p.theme.tokens.metrics.menu_row_height);
    let face = FloatingPanelFace::new();
    let mh = face.height_for(row_h * menu_rows.len() as f64, &p.theme);
    let menu = Rect::new(
        window.x1 - 120.0,
        window.y0 + 140.0,
        window.x1 + 120.0,
        window.y0 + 140.0 + mh,
    );
    let (layout, on_panel) = face.paint(&mut p.scene, Affine::IDENTITY, menu, 1.0, &p.theme);
    {
        let c = PaintCtx::new(&mut p.scene, Affine::IDENTITY, &mut p.tr, &on_panel);
        for (i, name) in menu_rows.iter().enumerate() {
            let row = Rect::new(
                layout.body.x0,
                layout.body.y0 + i as f64 * row_h,
                layout.body.x1,
                layout.body.y0 + (i as f64 + 1.0) * row_h,
            );
            let ink = if i == 1 {
                tilo_ui::widget::floating_surface::paint_armed_row(
                    c.scene,
                    c.transform,
                    row,
                    8.0,
                    &on_panel,
                )
            } else if i == 3 {
                on_panel.tokens.colors.semantic.danger
            } else {
                on_panel.text_primary
            };
            let style = on_panel.text(TextRole::Body);
            let (_, h) = c.text.measure_style(name, None, &style);
            c.text.draw_style(
                c.scene,
                name,
                ink,
                Affine::translate((row.x0 + 12.0, row.center().y - f64::from(h) / 2.0)),
                None,
                &style,
            );
        }
    }
    p.y = ground.y1 + 8.0;

    p.section("Colour roles");
    let c = &p.theme.tokens.colors;
    let roles = [
        ("Accent", c.intent.accent),
        ("Accent ink", c.intent.accent_ink),
        ("On", c.intent.on_state),
        ("Neutral fill", c.intent.neutral_fill),
        ("Danger", c.semantic.danger),
        ("Warning", c.semantic.warning),
        ("Success", c.semantic.success),
        ("Info", c.semantic.info),
    ];
    let mut x = MARGIN;
    let y = p.y;
    for (name, colour) in roles {
        p.scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            colour,
            None,
            &vello::kurbo::RoundedRect::new(x, y, x + 120.0, y + 56.0, 8.0),
        );
        let ink = p.theme.text_dim;
        p.text(name, TextRole::Caption, ink, Point::new(x, y + 64.0));
        x += 140.0;
    }
    p.y = y + 96.0;
    p.finish(out);
}

fn panels(theme: &Theme, out: &Path) {
    let mut p = Page::new(theme);
    p.section("Floating panel");
    let ground = Rect::new(MARGIN, p.y, PAGE_W - MARGIN, p.y + 520.0);
    let _ = paint_surface(
        &mut p.scene,
        Affine::IDENTITY,
        ground,
        SurfaceLevel::Ground,
        &p.theme,
    );
    let results = [
        (Icon::File, "Budget 2027", "Spreadsheet in Finance"),
        (Icon::Folder, "Design reviews", "Folder in Work"),
        (Icon::Calendar, "Planning", "Tomorrow, 10:00"),
        (Icon::Users, "Ana Lima", "In this workspace"),
    ];
    let field_h = ControlSize::Large.height(&p.theme);
    let footer_h = KeyHintsFace::height(&p.theme);
    let face = FloatingPanelFace::new().header(field_h).footer(footer_h);
    let row_h = f64::from(p.theme.tokens.metrics.row_double);
    let height = face.height_for(row_h * results.len() as f64, &p.theme);
    let panel = Rect::new(
        ground.x0 + 60.0,
        ground.y0 + 40.0,
        ground.x0 + 620.0,
        ground.y0 + 40.0 + height,
    );
    let (layout, on_panel) = face.paint(&mut p.scene, Affine::IDENTITY, panel, 1.0, &p.theme);
    {
        let mut c = PaintCtx::new(&mut p.scene, Affine::IDENTITY, &mut p.tr, &on_panel);
        // The embedded field: the panel is its surface.
        let icon = f64::from(on_panel.tokens.metrics.icon_bar);
        let cy = layout.header.center().y;
        Icon::Search.draw(
            c.scene,
            c.transform,
            Rect::new(
                layout.header.x0 + 8.0,
                cy - icon / 2.0,
                layout.header.x0 + 8.0 + icon,
                cy + icon / 2.0,
            ),
            on_panel.text_dim,
        );
        let style = on_panel
            .text(TextRole::Title)
            .with_weight(tilo_ui::text_render::TextWeight::Regular);
        let (_, h) = c.text.measure_style("bud", None, &style);
        c.text.draw_style(
            c.scene,
            "bud",
            on_panel.text_primary,
            Affine::translate((layout.header.x0 + 16.0 + icon, cy - f64::from(h) / 2.0)),
            None,
            &style,
        );
        let bleed = 0.0;
        for (i, (icon, title, secondary)) in results.iter().enumerate() {
            let row = Rect::new(
                layout.body.x0 - bleed,
                layout.body.y0 + i as f64 * row_h,
                layout.body.x1 + bleed,
                layout.body.y0 + (i as f64 + 1.0) * row_h,
            );
            if i == 0 {
                tilo_ui::widget::floating_surface::paint_armed_row(
                    c.scene,
                    c.transform,
                    row,
                    8.0,
                    &on_panel,
                );
            }
            let face = ListRowFace::new(title)
                .leading(RowLeading::Icon(*icon))
                .secondary(secondary);
            face.paint(&mut c, row.inset((-8.0, 0.0)));
        }
        let hints = [
            KeyHint::new(["Enter"], "Open"),
            KeyHint::new(["Tab"], "Actions"),
            KeyHint::new(["Esc"], "Close"),
        ];
        KeyHintsFace::new(&hints).paint(&mut c, layout.footer.inset((-8.0, 0.0)));
    }
    // An empty palette: the field alone, with no rule under it.
    let lone = FloatingPanelFace::new().header(field_h).body_empty(true);
    let lh = lone.height_for(0.0, &p.theme);
    let lone_rect = Rect::new(
        ground.x0 + 680.0,
        ground.y0 + 40.0,
        ground.x1 - 60.0,
        ground.y0 + 40.0 + lh,
    );
    let (layout, on_panel) = lone.paint(&mut p.scene, Affine::IDENTITY, lone_rect, 1.0, &p.theme);
    {
        let c = PaintCtx::new(&mut p.scene, Affine::IDENTITY, &mut p.tr, &on_panel);
        let icon = f64::from(on_panel.tokens.metrics.icon_bar);
        let cy = layout.header.center().y;
        Icon::Search.draw(
            c.scene,
            c.transform,
            Rect::new(
                layout.header.x0 + 8.0,
                cy - icon / 2.0,
                layout.header.x0 + 8.0 + icon,
                cy + icon / 2.0,
            ),
            on_panel.text_dim,
        );
        let style = on_panel
            .text(TextRole::Title)
            .with_weight(tilo_ui::text_render::TextWeight::Regular);
        let (_, h) = c.text.measure_style("Search", None, &style);
        c.text.draw_style(
            c.scene,
            "Search apps, files and people",
            on_panel.text_tertiary,
            Affine::translate((layout.header.x0 + 16.0 + icon, cy - f64::from(h) / 2.0)),
            None,
            &style,
        );
    }
    p.y = ground.y1 + 8.0;

    p.section("Banner and empty state");
    let banner = BannerContent::new(BannerKind::Warning, "Ana is offline")
        .with_message(Some("Her changes will sync when she is back."))
        .action("Notify me");
    let bw = 720.0;
    let bh = tilo_ui::widget::Banner::height(&banner, bw, &mut p.tr, &p.theme);
    let rect = Rect::new(MARGIN, p.y, MARGIN + bw, p.y + bh);
    let _ = tilo_ui::widget::Banner::paint(&mut p.ctx(), rect, &banner, Point::new(-100.0, -100.0));
    p.y += bh + 24.0;
    let empty = EmptyStateContent {
        icon: Some(Icon::Folder),
        illustration: None,
        title: "No projects yet",
        message: None,
        action_label: Some("New project"),
        error: false,
    };
    let rect = Rect::new(MARGIN, p.y, MARGIN + 720.0, p.y + 220.0);
    let _ = EmptyState::paint(&mut p.ctx(), rect, &empty, Point::new(-100.0, -100.0));
    p.y += 220.0;
    p.finish(out);
}

#[allow(
    clippy::exit,
    reason = "the gallery capture is a command-line scenario that must fail with a status when it cannot write its output"
)]
pub fn run(args: &[String]) {
    tilo_ui::i18n::I18nManager::init("en-US");
    let out = args
        .first()
        .map_or_else(|| PathBuf::from("/tmp/design_gallery"), PathBuf::from);
    if let Err(error) = std::fs::create_dir_all(&out) {
        eprintln!("design_gallery: cannot create {}: {error}", out.display());
        std::process::exit(2);
    }
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let theme = theme.with_reduce_motion(true);
        let file = |page: &str| out.join(format!("{page}-{name}.png"));
        surfaces(&theme, &file("surfaces"));
        controls(&theme, &file("controls"));
        rows(&theme, &file("rows"));
        rows(
            &theme.clone().with_density(Density::Compact),
            &file("rows-compact"),
        );
        panels(&theme, &file("panels"));
    }
}
