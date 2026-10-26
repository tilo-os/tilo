//! The editor's chrome: the slide sidebar and the properties inspector,
//! laid out from the theme's metrics and painted through the toolkit's
//! faces. The window's bar holds the editor's controls
//! ([`super::window_bar`]).
//!
//! The geometry is renderer-free ([`SlidesApp::compute_chrome`]) so the
//! painter, the pointer and the accessibility tree read the same rects; the
//! only measured inputs are the label widths [`ChromeMetrics`] caches from
//! the renderer at the top of every frame.

use super::{
    Affine, ArgValue, Background, BlendMode, DeckAssets, DeckLook, Element, ElementKind,
    FILL_ROLES, Fill, HashMap, Hit, Icon, PaintCtx, Palette, PaletteKind, Point, PresenceCtx, Rect,
    SLIDE_H, SLIDE_W, STROKE_ROLES, Scene, SidebarDrawer, SlideZoom, SlidesApp, SplitLayout,
    TextAlign, TextRenderer, Theme, custom_colour_hex, tr, tr_args,
};

use ui::TextRole;
use ui::surface::Region;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ColorSwatchFace, ControlSize, DropZoneFace, Segment,
    SegmentedFace, ToggleGroup,
};

/// What a measured width belongs to: a button face (with or without a
/// leading icon) or a field.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Measure {
    /// A `Regular` button face.
    Button { icon: bool },
    /// The font-size field.
    FontInput,
    /// One segment of a segmented strip: its strong label and padding.
    Segment,
}

/// Theme-derived measures the renderer-free chrome layout reads, plus the
/// measured widths of every label it reserves room for. Refreshed at the top
/// of every draw from the theme and renderer about to paint, so the rects
/// events hit are the rects that were painted.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChromeMetrics {
    widths: HashMap<(String, Measure), f64>,
    pub(super) s1: f64,
    pub(super) s2: f64,
    pub(super) s3: f64,
    pub(super) s4: f64,
    pub(super) s6: f64,
    /// The inspector's heading row.
    pub(super) header_h: f64,
    /// A `Regular` control.
    pub(super) control_h: f64,
    /// A `Small` control: swatches.
    pub(super) small_h: f64,
    /// The square an icon-only control occupies.
    pub(super) target: f64,
    pub(super) lh_heading: f64,
    pub(super) lh_subheading: f64,
    pub(super) lh_body: f64,
    pub(super) lh_label: f64,
    /// How much text-sized widths grow at the current text scale.
    pub(super) growth: f64,
    /// One wheel notch follows the host's scrolling metric.
    pub(super) wheel_line: f64,
    /// Height of the drop-to-delete zone at the foot of the sidebar.
    pub(super) drop_zone_h: f64,
}

impl ChromeMetrics {
    /// Everything but the measured widths, from `theme`.
    pub(super) fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let control_h = ControlSize::Regular.height(theme);
        let s3 = f64::from(sp.s3);
        let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
        Self {
            widths: HashMap::new(),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3,
            s4: f64::from(sp.s4),
            s6: f64::from(sp.s6),
            header_h: f64::from(metrics.header_height).max(control_h + 2.0 * s3),
            control_h,
            small_h: ControlSize::Small.height(theme),
            target: f64::from(metrics.min_pointer_target).max(control_h),
            lh_heading: line(TextRole::Heading),
            lh_subheading: line(TextRole::Subheading),
            lh_body: line(TextRole::Body),
            lh_label: line(TextRole::Label),
            growth: theme.responsive_scale(),
            wheel_line: f64::from(metrics.wheel_line),
            drop_zone_h: 2.0 * f64::from(sp.s4)
                + f64::from(metrics.icon_lg)
                + f64::from(sp.s2)
                + line(TextRole::Body),
        }
    }

    /// Measure every `(label, measure)` the chrome lays out against `tr`.
    pub(super) fn resolve(
        tr: &mut TextRenderer,
        theme: &Theme,
        labels: impl IntoIterator<Item = (String, Measure)>,
        font_input_w: f64,
    ) -> Self {
        let mut metrics = Self::of(theme);
        for (label, measure) in labels {
            let width = match measure {
                Measure::Button { icon } => {
                    let face = ButtonFace::new(Rect::ZERO, &label);
                    let face = if icon { face.icon(Icon::Plus) } else { face };
                    face.intrinsic_width(tr, theme)
                }
                Measure::FontInput => font_input_w,
                Measure::Segment => {
                    let style = ControlSize::Regular.label_style(theme).strong();
                    f64::from(tr.measure_style(&label, None, &style).0)
                        + 2.0 * f64::from(theme.tokens.spacing.s3)
                }
            };
            metrics.widths.insert((label, measure), width.ceil());
        }
        metrics
    }

    /// The stock measures used before the first draw has a renderer.
    pub(super) fn fallback() -> Self {
        Self::of(&Theme::dark())
    }

    /// The measured width of `label`, or an estimate before it was measured.
    pub(super) fn width(&self, label: &str, measure: Measure) -> f64 {
        if let Some(width) = self.widths.get(&(label.to_owned(), measure)) {
            return *width;
        }
        // A generous per-glyph estimate; replaced by the measured width at
        // the next draw.
        let glyphs = label.chars().count() as f64;
        match measure {
            Measure::Button { icon } => {
                glyphs * self.lh_label * 0.5
                    + 2.0 * self.s3
                    + if icon { self.lh_label + self.s2 } else { 0.0 }
            }
            Measure::FontInput => 2.0 * self.target + 4.0 * self.lh_label,
            Measure::Segment => glyphs * self.lh_label * 0.55 + 2.0 * self.s3,
        }
    }
}

/// A line of chrome text.
#[derive(Clone)]
pub(super) struct Label {
    pub(super) pos: Point,
    pub(super) text: String,
    pub(super) role: TextRole,
    /// Secondary ink.
    pub(super) dim: bool,
    /// When set, the painter elides `text` to this many pixels.
    pub(super) max_w: Option<f64>,
}

/// What a chrome control is; the toolkit face that paints it follows.
#[derive(Clone)]
pub(super) enum ButtonKind {
    /// A labelled command, optionally with a leading icon.
    Command {
        label: String,
        icon: Option<Icon>,
        role: ButtonRole,
    },
    /// An icon-only command; `name` is its accessible name and tooltip.
    /// `active` paints the lift of a toggle that is on.
    Icon {
        icon: Icon,
        name: String,
        active: bool,
    },
    /// One segment of a segmented strip. Consecutive segments of one
    /// `group` on one row paint as one strip. An icon segment shows its
    /// icon, and `label` is its accessible name.
    Segment {
        label: String,
        icon: Option<Icon>,
        active: bool,
        group: u8,
    },
    /// A fill or stroke colour role.
    Swatch { role: String, selected: bool },
    /// A deck colour theme.
    Theme { kind: PaletteKind, selected: bool },
    /// The speaker-notes field (painted by its text field).
    Notes,
    /// The slide layout picker (painted by its dropdown).
    LayoutPicker,
}

#[derive(Clone)]
pub(super) struct UiButton {
    pub(super) rect: Rect,
    pub(super) kind: ButtonKind,
    pub(super) hit: Hit,
}

/// Segment groups of the inspector.
/// One choice of a segmented strip in the inspector.
struct SegmentItem {
    label: String,
    icon: Option<Icon>,
    active: bool,
    hit: Hit,
}

/// The glyph of a text alignment.
fn align_icon(align: TextAlign) -> Icon {
    match align {
        TextAlign::Left => Icon::AlignLeft,
        TextAlign::Center => Icon::AlignCenter,
        TextAlign::Right => Icon::AlignRight,
        TextAlign::Justify => Icon::AlignJustify,
    }
}

const SEG_ALIGN: u8 = 0;
const SEG_BACKGROUND: u8 = 1;
const SEG_LOOK: u8 = 2;

/// Which column of the window a control or a label belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Zone {
    /// The slide sidebar.
    Rail,
    Inspector,
    /// Under the slide: its speaker notes, while the inspector is hidden
    /// and the column has room below the slide.
    Content,
}

/// Where each column's controls and labels start in [`Chrome::buttons`]
/// and [`Chrome::labels`]: the sidebar's, then the inspector's, then those
/// under the slide.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ZoneStarts {
    inspector_buttons: usize,
    inspector_labels: usize,
    content_buttons: usize,
    content_labels: usize,
}

/// The editor chrome resolved for one frame.
pub(super) struct Chrome {
    /// The slide sidebar: a docked column or a floating drawer.
    pub(super) rail: Rect,
    /// The inspector: a docked column or a floating drawer.
    pub(super) inspector: Rect,
    /// The inspector's scrolling body, under its header.
    pub(super) inspector_body: Rect,
    /// Unscrolled height of the inspector body's content.
    pub(super) inspector_content_h: f64,
    pub(super) slide_dest: Rect,
    pub(super) scale: f64,
    pub(super) thumbs: Vec<(usize, Rect)>,
    pub(super) buttons: Vec<UiButton>,
    pub(super) labels: Vec<Label>,
    /// The thumbnail list, between the sidebar's header and its footer.
    pub(super) rail_viewport: Rect,
    pub(super) notes_box: Option<Rect>,
    pub(super) font_input_rect: Option<Rect>,
    pub(super) fill_picker_rect: Option<Rect>,
    pub(super) layout: SplitLayout,
    pub(super) inspector_layout: SplitLayout,
    /// Dims the window behind an open floating drawer.
    pub(super) scrim: Rect,
    zones: ZoneStarts,
}

impl Chrome {
    /// Where the layout picker sits this frame, if the inspector shows it.
    pub(super) fn layout_picker_rect(&self) -> Option<Rect> {
        self.zone_buttons(Zone::Inspector)
            .find(|b| matches!(b.kind, ButtonKind::LayoutPicker))
            .map(|b| b.rect)
    }

    /// The column the button at `index` belongs to.
    pub(super) fn button_zone(&self, index: usize) -> Zone {
        if index >= self.zones.content_buttons {
            Zone::Content
        } else if index >= self.zones.inspector_buttons {
            Zone::Inspector
        } else {
            Zone::Rail
        }
    }

    /// The column the label at `index` belongs to.
    pub(super) fn label_zone(&self, index: usize) -> Zone {
        if index >= self.zones.content_labels {
            Zone::Content
        } else if index >= self.zones.inspector_labels {
            Zone::Inspector
        } else {
            Zone::Rail
        }
    }

    /// The buttons of one column, with their indices.
    pub(super) fn zone_buttons(&self, zone: Zone) -> impl Iterator<Item = &UiButton> {
        self.buttons
            .iter()
            .enumerate()
            .filter(move |(i, _)| self.button_zone(*i) == zone)
            .map(|(_, b)| b)
    }
}

/// The insert tools, in the order the window bar's Insert menu lists them,
/// each with its icon.
pub(super) fn insert_tools() -> [(String, Icon, ElementKind); 5] {
    [
        (tr("slides-tool-text"), Icon::Type, ElementKind::Text),
        (tr("slides-tool-rectangle"), Icon::Square, ElementKind::Rect),
        (
            tr("slides-tool-ellipse"),
            Icon::Circle,
            ElementKind::Ellipse,
        ),
        (tr("slides-tool-line"), Icon::Minus, ElementKind::Line),
        (tr("slides-tool-image"), Icon::Image, ElementKind::Image),
    ]
}

/// The display name of a colour role, for its swatch's tooltip and
/// accessible name.
pub(super) fn role_name(role: &str) -> String {
    match role {
        "none" => tr("slides-colour-none"),
        "accent" => tr("slides-colour-accent"),
        "accent-soft" => tr("slides-colour-accent-soft"),
        "surface" => tr("slides-colour-surface"),
        "deep" => tr("slides-colour-deep"),
        "text" => tr("slides-colour-text"),
        "white" => tr("slides-colour-white"),
        "success" => tr("slides-colour-green"),
        "warning" => tr("slides-colour-amber"),
        "danger" => tr("slides-colour-red"),
        "info" => tr("slides-colour-violet"),
        other => other.to_owned(),
    }
}

impl SlidesApp {
    /// Every label the chrome reserves a measured slot for this frame.
    pub(super) fn chrome_label_set(&self) -> Vec<(String, Measure)> {
        let button = |s: String| (s, Measure::Button { icon: false });
        let command = |s: String| (s, Measure::Button { icon: true });
        let mut set = vec![
            (self.font_input.value().to_string(), Measure::FontInput),
            command(tr("slides-bring-front")),
            command(tr("slides-send-back")),
            command(tr("slides-duplicate")),
            command(tr("slides-delete")),
            button(tr("slides-custom-colour")),
        ];
        set.extend(
            TextAlign::ALL
                .iter()
                .map(|a| a.glyph())
                .chain(Background::ALL.iter().map(|b| b.label()))
                .map(|label| (label.to_owned(), Measure::Segment)),
        );
        set.extend(
            PaletteKind::ALL
                .iter()
                .map(|kind| command(kind.label().to_owned())),
        );
        if let Some(el) = self.selected_element()
            && el.fill.starts_with('#')
        {
            set.push(button(custom_colour_hex(&el.fill)));
        }
        set
    }

    /// Re-resolve the chrome metrics against the theme and renderer about
    /// to paint.
    pub(super) fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let font_input_w = self.font_input.preferred_width(theme, tr);
        let set = self.chrome_label_set();
        self.chrome_metrics = ChromeMetrics::resolve(tr, theme, set, font_input_w);
        let zone_w =
            f64::from(theme.tokens.metrics.sidebar_min_width) - 2.0 * self.chrome_metrics.s3;
        self.chrome_metrics.drop_zone_h =
            DropZoneFace::new(&crate::strings::tr("slides-drop-to-delete"))
                .icon(Icon::Trash)
                .height(tr, theme, zone_w, 0.0);
        self.drawer_theme_scale(theme);
    }

    /// The breakpoint below which both side panels become drawers: a slide
    /// sidebar, an inspector and a content column that still reads.
    fn panel_breakpoint() -> f64 {
        let base = ui::tokens::Metrics::BASE;
        f64::from(base.sidebar_min_width + base.inspector_width + base.content_min_width)
    }

    /// Fresh slide sidebar and inspector drawers at the shell's metrics.
    pub(super) fn new_drawers() -> (SidebarDrawer, SidebarDrawer) {
        let base = ui::tokens::Metrics::BASE;
        let breakpoint = Self::panel_breakpoint();
        (
            SidebarDrawer::new()
                .docked_width(f64::from(base.sidebar_min_width))
                .breakpoint(breakpoint),
            SidebarDrawer::new()
                .right()
                .docked_width(f64::from(base.inspector_width))
                .breakpoint(breakpoint),
        )
    }

    /// Hand both drawers the theme's scale.
    fn drawer_theme_scale(&mut self, theme: &Theme) {
        self.rail_drawer.scale_for(theme);
        self.inspector_drawer.scale_for(theme);
    }

    /// Resolve both drawers against the window.
    pub(super) fn resolve_drawers(&mut self) {
        let r = self.rect;
        self.rail_drawer.resolve(r);
        self.inspector_drawer.resolve(r);
    }

    /// The editor chrome for this frame.
    pub(super) fn compute_chrome(&self) -> Chrome {
        let r = self.rect;
        let layout = self.rail_layout();
        let inspector_layout = self.inspector_layout();
        // A drawer slid fully out of the window shows nothing.
        let shown = |panel: Rect| {
            if r.intersect(panel).width() > 1.0 {
                panel
            } else {
                Rect::ZERO
            }
        };
        let rail = shown(self.rail_drawer.sidebar_rect());
        let inspector = shown(self.inspector_drawer.sidebar_rect());
        let rail_docked = layout == SplitLayout::Docked && rail.width() > 1.0;
        let inspector_docked = inspector_layout == SplitLayout::Docked && inspector.width() > 1.0;
        let content = Rect::new(
            if rail_docked { rail.x1 } else { r.x0 },
            r.y0,
            if inspector_docked { inspector.x0 } else { r.x1 },
            r.y1,
        );
        let stacked = if inspector.width() > 1.0 {
            None
        } else {
            self.stacked_notes(content)
        };
        let (slide_dest, scale) = self.fit_slide(stacked.map_or(content, |(canvas, _)| canvas));
        let scrim = if (layout == SplitLayout::Overlay && self.rail_drawer.reveal() > 0.001)
            || (inspector_layout == SplitLayout::Overlay && self.inspector_drawer.reveal() > 0.001)
        {
            r
        } else {
            Rect::ZERO
        };

        let mut buttons = Vec::new();
        let mut labels = Vec::new();
        let rail_viewport = if rail.width() > 1.0 {
            self.build_rail_chrome(rail, &mut buttons)
        } else {
            Rect::ZERO
        };
        let thumbs = self.page_sidebar.thumb_rects();

        let mut zones = ZoneStarts {
            inspector_buttons: buttons.len(),
            inspector_labels: labels.len(),
            ..ZoneStarts::default()
        };
        let (inspector_body, inspector_content_h, mut notes_box, font_input_rect) =
            if inspector.width() > 1.0 {
                self.build_inspector(inspector, &mut buttons, &mut labels)
            } else {
                (Rect::ZERO, 0.0, None, None)
            };
        zones.content_buttons = buttons.len();
        zones.content_labels = labels.len();
        if let Some((_, area)) = stacked {
            let mut y = area.y0;
            Self::push_section_label(
                &self.chrome_metrics,
                &mut labels,
                area.x0,
                &mut y,
                &tr("slides-speaker-notes"),
            );
            let notes = Rect::new(area.x0, y, area.x1, area.y1);
            buttons.push(UiButton {
                rect: notes,
                kind: ButtonKind::Notes,
                hit: Hit::EditNotes,
            });
            notes_box = Some(notes);
        }
        let fill_picker_rect = self.fill_picker_popover_rect(inspector, r);

        Chrome {
            rail,
            inspector,
            inspector_body,
            inspector_content_h,
            slide_dest,
            scale,
            thumbs,
            buttons,
            labels,
            rail_viewport,
            notes_box,
            font_input_rect,
            fill_picker_rect,
            layout,
            inspector_layout,
            scrim,
            zones,
        }
    }

    /// The swatch for `role` in `rect`, in its colour as it reads on the
    /// current slide; "no colour" is the toolkit's none swatch.
    pub(super) fn swatch_face(&self, pal: &Palette, rect: Rect, role: &str) -> ColorSwatchFace {
        let on_slide = self.slide().map_or(*pal, |slide| pal.on(slide.background));
        match on_slide.resolve_role(role) {
            Some(color) => ColorSwatchFace::new(rect, color, role_name(role)),
            None => ColorSwatchFace::none(rect, role_name(role)),
        }
    }

    /// With the inspector hidden, a column taller than the slide needs puts
    /// the slide at its top and the speaker notes under it, rather than
    /// leaving empty bands above and below the slide. Returns the slide's
    /// canvas and the notes' area (their caption, then the field).
    fn stacked_notes(&self, content: Rect) -> Option<(Rect, Rect)> {
        let m = &self.chrome_metrics;
        let slide_h = (content.width() - 2.0 * m.s6).max(1.0) * SLIDE_H / SLIDE_W + 2.0 * m.s6;
        let notes_h = m.lh_subheading + m.s2 + 4.0 * m.lh_body + 2.0 * m.s3 + m.s6;
        if content.height() - slide_h < notes_h {
            return None;
        }
        let canvas = Rect::new(content.x0, content.y0, content.x1, content.y0 + slide_h);
        let area = Rect::new(
            content.x0 + m.s6,
            canvas.y1,
            content.x1 - m.s6,
            content.y1 - m.s6,
        );
        Some((canvas, area))
    }

    /// Fit the 16:9 slide inside `canvas` with the content gutter round it,
    /// or place it at the zoom the person chose, moved by however far they
    /// have panned. Returns the destination rect and the slide→pixel scale
    /// factor.
    ///
    /// A zoomed slide is centred on the pane and may be larger than it: the
    /// pane clips it, and the wheel moves it, so every part stays reachable.
    pub(super) fn fit_slide(&self, canvas: Rect) -> (Rect, f64) {
        let pad = self.chrome_metrics.s6;
        let avail_w = (canvas.width() - 2.0 * pad).max(1.0);
        let avail_h = (canvas.height() - 2.0 * pad).max(1.0);
        // What the pan is clamped to, for the keys and the wheel, which run
        // between frames.
        self.slide_pane.set((avail_w, avail_h));
        let fit = (avail_w / SLIDE_W).min(avail_h / SLIDE_H);
        let s = match self.slide_zoom {
            SlideZoom::Fit => fit,
            SlideZoom::Pinned(pinned) => pinned,
        };
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let (pan_x, pan_y) = self.slide_pan_within(canvas);
        let sx = canvas.x0 + (canvas.width() - sw) / 2.0 + pan_x;
        let sy = canvas.y0 + (canvas.height() - sh) / 2.0 + pan_y;
        (Rect::new(sx, sy, sx + sw, sy + sh), s)
    }

    /// How far the slide can be moved from the pane's centre: half of what
    /// does not fit, along each axis, and nothing while it fits.
    pub(super) fn slide_pan_within(&self, canvas: Rect) -> (f64, f64) {
        let pad = self.chrome_metrics.s6;
        let avail_w = (canvas.width() - 2.0 * pad).max(1.0);
        let avail_h = (canvas.height() - 2.0 * pad).max(1.0);
        let s = match self.slide_zoom {
            SlideZoom::Fit => return (0.0, 0.0),
            SlideZoom::Pinned(pinned) => pinned,
        };
        let reach_x = ((SLIDE_W * s - avail_w) / 2.0).max(0.0);
        let reach_y = ((SLIDE_H * s - avail_h) / 2.0).max(0.0);
        (
            self.slide_pan.0.clamp(-reach_x, reach_x),
            self.slide_pan.1.clamp(-reach_y, reach_y),
        )
    }

    /// The slide sidebar: the footer actions and, above them, the
    /// thumbnail list's viewport, which the shared [`PageSidebar`] owns.
    fn build_rail_chrome(&self, rail: Rect, buttons: &mut Vec<UiButton>) -> Rect {
        let m = &self.chrome_metrics;
        // Footer: add, duplicate and delete the current slide.
        let footer_y = rail.y1 - m.s3 - m.target;
        let mut x = rail.x0 + m.s3;
        for (icon, name, hit) in [
            (Icon::Plus, tr("slides-a11y-new-slide"), Hit::NewSlide),
            (
                Icon::Copy,
                tr("slides-a11y-duplicate-slide"),
                Hit::DuplicateSlide,
            ),
            (
                Icon::Trash,
                tr("slides-a11y-delete-slide"),
                Hit::DeleteSlide,
            ),
        ] {
            buttons.push(UiButton {
                rect: Rect::new(x, footer_y, x + m.target, footer_y + m.target),
                kind: ButtonKind::Icon {
                    icon,
                    name,
                    active: false,
                },
                hit,
            });
            x += m.target + m.s1;
        }
        Rect::new(rail.x0, rail.y0, rail.x1, (footer_y - m.s3).max(rail.y0))
    }

    /// The drop-to-delete zone that takes the place of the sidebar's
    /// footer while a thumbnail is dragged.
    pub(super) fn slide_delete_zone(&self, rail: Rect) -> Rect {
        let m = &self.chrome_metrics;
        let top = (rail.y1 - m.s3 - m.drop_zone_h).max(rail.y0);
        Rect::new(rail.x0 + m.s3, top, rail.x1 - m.s3, rail.y1 - m.s3)
    }

    /// The thumbnail list's viewport inside `rail` (see
    /// [`Self::build_rail_chrome`]): above the footer, or above the
    /// drop-to-delete zone while a thumbnail is dragged.
    pub(super) fn rail_viewport_of(&self, rail: Rect) -> Rect {
        let m = &self.chrome_metrics;
        let top = rail.y0;
        let foot = if self.page_sidebar.is_dragging() {
            self.slide_delete_zone(rail).y0 - m.s2
        } else {
            rail.y1 - m.s3 - m.target - m.s3
        };
        Rect::new(rail.x0, top, rail.x1, foot.max(top))
    }

    /// The fill colour popover: beside the inspector's fill section,
    /// towards the canvas, and kept inside the window. `None` while it is
    /// closed.
    fn fill_picker_popover_rect(&self, inspector: Rect, r: Rect) -> Option<Rect> {
        if !self.fill_picker_open {
            return None;
        }
        let m = &self.chrome_metrics;
        let (w, h) = self.fill_picker.preferred_size();
        let (top, bottom) = self.fill_picker_panel_insets();
        let x = if inspector.width() > 1.0 {
            inspector.x0 - m.s2 - m.s4 - w
        } else {
            r.x1 - m.s4 - m.s2 - w
        }
        .max(r.x0 + m.s4 + m.s2);
        let y = (r.y0 + top + m.s2)
            .min(r.y1 - h - bottom - m.s2)
            .max(r.y0 + top + m.s2);
        Some(Rect::new(x, y, x + w, y + h))
    }

    /// How far the popover panel extends beyond the picker above (its
    /// title row) and below.
    pub(super) fn fill_picker_panel_insets(&self) -> (f64, f64) {
        let m = &self.chrome_metrics;
        (m.s4 + m.lh_subheading + m.s3, m.s4)
    }

    /// The popover panel round the fill picker at `picker`.
    pub(super) fn fill_picker_panel_rect(&self, picker: Rect) -> Rect {
        let (top, bottom) = self.fill_picker_panel_insets();
        let side = self.chrome_metrics.s4;
        Rect::new(
            picker.x0 - side,
            picker.y0 - top,
            picker.x1 + side,
            picker.y1 + bottom,
        )
    }

    /// The inspector: its heading (what is selected) and its scrolling body of sections, ending in the speaker
    /// notes. Returns the body viewport, the body's unscrolled content
    /// height, the notes field and the font-size field.
    fn build_inspector(
        &self,
        inspector: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> (Rect, f64, Option<Rect>, Option<Rect>) {
        let m = &self.chrome_metrics;
        let header = Rect::new(
            inspector.x0,
            inspector.y0,
            inspector.x1,
            inspector.y0 + m.header_h,
        );
        let heading = match self.selected_element() {
            Some(el) => el.kind.label().to_owned(),
            None => tr_args(
                "slides-slide-n",
                &[("n", ArgValue::from(self.cur_slide + 1))],
            ),
        };
        let title_x = inspector.x0 + m.s3;
        labels.push(Label {
            pos: Point::new(title_x, header.y0 + (header.height() - m.lh_heading) / 2.0),
            text: heading,
            role: TextRole::Heading,
            dim: false,
            max_w: Some((inspector.x1 - m.s3 - title_x).max(0.0)),
        });

        let body = Rect::new(inspector.x0, header.y1, inspector.x1, inspector.y1);
        let x0 = body.x0 + m.s3;
        let x1 = body.x1 - m.s3;
        let first_button = buttons.len();
        let first_label = labels.len();
        let mut y = body.y0 + m.s4;
        let mut font_input = None;
        if let Some(el) = self.selected_element() {
            y = self.build_inspector_element(el, x0, x1, y, buttons, labels, &mut font_input);
        } else {
            y = self.build_inspector_slide(x0, x1, y, buttons, labels);
        }

        // The speaker notes follow the last section and fill the rest of
        // the panel; a short panel scrolls rather than squeezing them.
        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-speaker-notes"));
        let notes_min = 4.0 * m.lh_body + 2.0 * m.s3;
        let mut notes = Rect::new(x0, y, x1, (body.y1 - m.s4).max(y + notes_min));
        buttons.push(UiButton {
            rect: notes,
            kind: ButtonKind::Notes,
            hit: Hit::EditNotes,
        });

        // Scroll the body once its sections outgrow it: every body item is
        // shifted by the scroll model's offset, and what then lies outside
        // the viewport is dropped, so paint, pointer and accessibility agree.
        let content_h = notes.y1 + m.s4 - body.y0;
        let max_offset = (content_h - body.height()).max(0.0);
        let offset = f64::from(self.inspector_scroll.offset_y()).clamp(0.0, max_offset);
        if offset > 0.0 {
            let shift = vello::kurbo::Vec2::new(0.0, -offset);
            for b in buttons.get_mut(first_button..).unwrap_or_default() {
                b.rect = b.rect + shift;
            }
            for l in labels.get_mut(first_label..).unwrap_or_default() {
                l.pos += shift;
            }
            font_input = font_input.map(|r| r + shift);
            notes = notes + shift;
        }
        let inside = |y: f64| y >= body.y0 && y <= body.y1;
        let kept: Vec<UiButton> = buttons
            .drain(first_button..)
            .filter(|b| inside(b.rect.center().y))
            .collect();
        buttons.extend(kept);
        let kept: Vec<Label> = labels
            .drain(first_label..)
            .filter(|l| inside(l.pos.y + m.lh_subheading / 2.0))
            .collect();
        labels.extend(kept);
        font_input = font_input.filter(|r| inside(r.center().y));
        (body, content_h, Some(notes), font_input)
    }

    /// A section label ("Fill") at `x0, *y`, advancing `*y` past it.
    fn push_section_label(
        m: &ChromeMetrics,
        labels: &mut Vec<Label>,
        x0: f64,
        y: &mut f64,
        t: &str,
    ) {
        labels.push(Label {
            pos: Point::new(x0, *y),
            text: t.to_owned(),
            role: TextRole::Subheading,
            dim: false,
            max_w: None,
        });
        *y += m.lh_subheading + m.s2;
    }

    /// The sections for a selected element: fill, stroke, (for text) font
    /// and alignment, and arrange. Returns the y under the last section.
    fn build_inspector_element(
        &self,
        el: &Element,
        x0: f64,
        x1: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
        font_input: &mut Option<Rect>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let mut y = y;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-fill"));
        // A shape may be left unfilled; text always has its colour.
        let mut fills: Vec<&str> = Vec::with_capacity(FILL_ROLES.len() + 1);
        if el.kind != ElementKind::Text {
            fills.push("none");
        }
        fills.extend(FILL_ROLES);
        y = self.swatch_grid(buttons, x0, x1, y, &fills, &el.fill, true);
        y += m.s2;
        let custom = el.fill.starts_with('#');
        buttons.push(UiButton {
            rect: Rect::new(x0, y, x1, y + m.control_h),
            kind: ButtonKind::Command {
                label: if custom {
                    custom_colour_hex(&el.fill)
                } else {
                    tr("slides-custom-colour")
                },
                icon: None,
                role: ButtonRole::Secondary,
            },
            hit: Hit::OpenFillPicker,
        });
        y += m.control_h + m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-stroke"));
        y = self.swatch_grid(buttons, x0, x1, y, &STROKE_ROLES, &el.stroke, false);
        y += m.s6;

        if el.kind == ElementKind::Text {
            Self::push_section_label(m, labels, x0, &mut y, &tr("slides-font"));
            let field_w = m.width("", Measure::FontInput).min(x1 - x0);
            *font_input = Some(Rect::new(x0, y, x0 + field_w, y + m.control_h));
            // Bold, italic and the two lists are square glyph toggles,
            // beside the size field when they fit, under it otherwise.
            let toggles = [
                (
                    Icon::Bold,
                    tr("slides-bold"),
                    self.mark_on(&ui::widget::marks::Mark::Bold),
                    Hit::ToggleBold,
                ),
                (
                    Icon::Italic,
                    tr("slides-italic"),
                    self.mark_on(&ui::widget::marks::Mark::Italic),
                    Hit::ToggleItalic,
                ),
                (
                    Icon::List,
                    tr("slides-bulleted-list"),
                    self.list_on(false),
                    Hit::ToggleList(false),
                ),
                (
                    Icon::ListOrdered,
                    tr("slides-numbered-list"),
                    self.list_on(true),
                    Hit::ToggleList(true),
                ),
            ];
            // Beside the field they are squares at the row's end; under it
            // they share the whole row, as the alignment strip does.
            let square = m.control_h;
            let count = toggles.len() as f64;
            let toggles_w = square * count + m.s1 * (count - 1.0);
            let (toggles_y, mut left, cell) = if field_w + m.s2 + toggles_w <= x1 - x0 + 0.5 {
                (y, x1 - toggles_w, square)
            } else {
                let cell = ((x1 - x0 - m.s1 * (count - 1.0)) / count).max(square);
                (y + m.control_h + m.s2, x0, cell)
            };
            for (icon, name, active, hit) in toggles {
                buttons.push(UiButton {
                    rect: Rect::new(left, toggles_y, left + cell, toggles_y + m.control_h),
                    kind: ButtonKind::Icon { icon, name, active },
                    hit,
                });
                left += cell + m.s1;
            }
            y = toggles_y + m.control_h + m.s6;

            Self::push_section_label(m, labels, x0, &mut y, &tr("slides-alignment"));
            let items: Vec<SegmentItem> = TextAlign::ALL
                .iter()
                .map(|a| SegmentItem {
                    label: a.glyph().to_owned(),
                    icon: Some(align_icon(*a)),
                    active: *a == el.align,
                    hit: Hit::SetAlign(*a),
                })
                .collect();
            y = self.segment_rows(buttons, x0, x1, y, SEG_ALIGN, items);
            y += m.s6;
        }

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-arrange"));
        let actions = [
            (
                tr("slides-bring-front"),
                Icon::BringToFront,
                ButtonRole::Secondary,
                Hit::BringFront,
            ),
            (
                tr("slides-send-back"),
                Icon::SendToBack,
                ButtonRole::Secondary,
                Hit::SendBack,
            ),
            (
                tr("slides-duplicate"),
                Icon::Copy,
                ButtonRole::Secondary,
                Hit::DuplicateElement,
            ),
            (
                tr("slides-delete"),
                Icon::Trash2,
                ButtonRole::Destructive,
                Hit::DeleteElement,
            ),
        ];
        let half = (x1 - x0 - m.s2) / 2.0;
        let widest = actions
            .iter()
            .map(|(label, ..)| m.width(label, Measure::Button { icon: true }))
            .fold(0.0_f64, f64::max);
        let cols = if widest <= half + 0.5 { 2 } else { 1 };
        let cell = if cols == 2 { half } else { x1 - x0 };
        for (i, (label, icon, role, hit)) in actions.into_iter().enumerate() {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            let x = x0 + col * (cell + m.s2);
            let top = y + row * (m.control_h + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(x, top, x + cell, top + m.control_h),
                kind: ButtonKind::Command {
                    label,
                    icon: Some(icon),
                    role,
                },
                hit,
            });
        }
        let rows = 4_usize.div_ceil(cols) as f64;
        y + rows * m.control_h + (rows - 1.0) * m.s2 + m.s6
    }

    /// The sections for the current slide when nothing is selected: its
    /// layout, its background and the deck's colour theme. Returns the y
    /// under the last section.
    fn build_inspector_slide(
        &self,
        x0: f64,
        x1: f64,
        y: f64,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let mut y = y;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-layout"));
        buttons.push(UiButton {
            rect: Rect::new(x0, y, x1, y + m.control_h),
            kind: ButtonKind::LayoutPicker,
            hit: Hit::OpenLayoutMenu,
        });
        y += m.control_h + m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-background"));
        let background = self.slide().map_or(Background::Surface, |s| s.background);
        let items: Vec<SegmentItem> = Background::ALL
            .iter()
            .map(|b| SegmentItem {
                label: b.label().to_owned(),
                icon: None,
                active: *b == background,
                hit: Hit::SetBackground(*b),
            })
            .collect();
        y = self.segment_rows(buttons, x0, x1, y, SEG_BACKGROUND, items);
        y += m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-look"));
        let items: Vec<SegmentItem> = DeckLook::ALL
            .iter()
            .map(|look| SegmentItem {
                label: look.label(),
                icon: None,
                active: *look == self.deck.look,
                hit: Hit::SetLook(*look),
            })
            .collect();
        y = self.segment_rows(buttons, x0, x1, y, SEG_LOOK, items);
        y += m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-deck-theme"));
        let widest = PaletteKind::ALL
            .iter()
            .map(|kind| m.width(kind.label(), Measure::Button { icon: true }))
            .fold(0.0_f64, f64::max);
        let columns = (((x1 - x0 + m.s2) / (widest + m.s2)).floor() as usize)
            .clamp(1, PaletteKind::ALL.len());
        let w = (x1 - x0 - (columns - 1) as f64 * m.s2) / columns as f64;
        for (i, kind) in PaletteKind::ALL.iter().enumerate() {
            let x = x0 + (i % columns) as f64 * (w + m.s2);
            let top = y + (i / columns) as f64 * (m.control_h + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(x, top, x + w, top + m.control_h),
                kind: ButtonKind::Theme {
                    kind: *kind,
                    selected: *kind == self.deck.palette,
                },
                hit: Hit::SetPalette(*kind),
            });
        }
        let rows = PaletteKind::ALL.len().div_ceil(columns);
        y + rows as f64 * (m.control_h + m.s2) - m.s2 + m.s6
    }

    /// A segmented strip across `x0..x1`: one row while every label fits
    /// its share, otherwise rows of two. Returns the y under it.
    fn segment_rows(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        x1: f64,
        y: f64,
        group: u8,
        items: Vec<SegmentItem>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let n = items.len().max(1);
        // An icon segment wants a pointer target; a labelled one its label.
        let widest = items
            .iter()
            .map(|item| {
                if item.icon.is_some() {
                    m.control_h
                } else {
                    m.width(&item.label, Measure::Segment)
                }
            })
            .fold(0.0_f64, f64::max);
        let cols = if widest <= (x1 - x0) / n as f64 + 0.5 {
            n
        } else {
            2
        };
        let seg_w = (x1 - x0) / cols as f64;
        for (i, item) in items.into_iter().enumerate() {
            let col = i % cols;
            let row = (i / cols) as f64;
            let sx = x0 + col as f64 * seg_w;
            let ex = if col + 1 == cols { x1 } else { sx + seg_w };
            let sy = y + row * (m.control_h + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(sx, sy, ex, sy + m.control_h),
                kind: ButtonKind::Segment {
                    label: item.label,
                    icon: item.icon,
                    active: item.active,
                    group,
                },
                hit: item.hit,
            });
        }
        let rows = n.div_ceil(cols) as f64;
        y + rows * m.control_h + (rows - 1.0) * m.s2
    }

    /// The fill or stroke swatches, four to a row. Returns the y under the
    /// last row.
    fn swatch_grid(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        x1: f64,
        y: f64,
        roles: &[&str],
        current: &str,
        is_fill: bool,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let cols = 4.0;
        let sw = (x1 - x0 - (cols - 1.0) * m.s2) / cols;
        let sh = m.small_h;
        for (i, role) in roles.iter().enumerate() {
            let col = (i as f64) % cols;
            let row = ((i as f64) / cols).floor();
            let x = x0 + col * (sw + m.s2);
            let top = y + row * (sh + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(x, top, x + sw, top + sh),
                kind: ButtonKind::Swatch {
                    role: (*role).to_owned(),
                    selected: *role == current,
                },
                hit: if is_fill {
                    Hit::SetFill((*role).to_owned())
                } else {
                    Hit::SetStroke((*role).to_owned())
                },
            });
        }
        let rows = ((roles.len() as f64) / cols).ceil();
        y + rows * (sh + m.s2) - m.s2
    }

    #[cfg(test)]
    pub(super) fn in_inspector(chrome: &Chrome, p: Point) -> bool {
        chrome.inspector.width() > 1.0 && chrome.inspector.contains(p)
    }

    /// The whole editor: the docked panels' regions, the canvas, the
    /// panels' contents, and any open drawer over its scrim.
    pub(super) fn draw_editor(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        chrome: &Chrome,
        presence: &PresenceCtx,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let pal = self.palette();
        let rail_docked = chrome.layout == SplitLayout::Docked;
        let inspector_docked = chrome.inspector_layout == SplitLayout::Docked;

        // The canvas stands on the window ground the framework painted. A
        // slide's type is sized in slide units, whatever the reader's text
        // scale.
        let reader_scale = tr.text_scale();
        tr.set_text_scale(1.0);
        self.layout_text_editor(tr, theme, chrome, &pal);
        let assets = DeckAssets {
            palette: &pal,
            images: &self.images,
            presence: Some(presence),
        };
        self.draw_editor_canvas(&mut PaintCtx::new(scene, t, tr, theme), chrome, assets);
        self.draw_text_editor(&mut PaintCtx::new(scene, t, tr, theme), presence);
        tr.set_text_scale(reader_scale);

        if let Some(notes) = chrome.notes_box
            && chrome.inspector.width() <= 1.0
        {
            Self::draw_labels(scene, t, tr, theme, chrome, Zone::Content, |_| true);
            self.draw_notes(scene, t, tr, theme, notes);
        }

        if rail_docked && chrome.rail.width() > 1.0 {
            let local = self.rail_drawer.paint_panel(scene, t, theme);
            self.draw_rail_contents(scene, t, tr, &local, chrome, &pal);
        }
        if inspector_docked && chrome.inspector.width() > 1.0 {
            let local = self.inspector_drawer.paint_panel(scene, t, theme);
            self.draw_inspector_contents(scene, t, tr, &local, chrome, &pal);
        }

        // An open drawer floats over everything.
        let rail_floating = !rail_docked && chrome.rail.width() > 1.0;
        let inspector_floating = !inspector_docked && chrome.inspector.width() > 1.0;
        if rail_floating {
            let local = self.rail_drawer.paint_panel(scene, t, theme);
            self.draw_rail_contents(scene, t, tr, &local, chrome, &pal);
        }
        if inspector_floating {
            let local = self.inspector_drawer.paint_panel(scene, t, theme);
            self.draw_inspector_contents(scene, t, tr, &local, chrome, &pal);
        }
    }

    /// The theme the inspector's contents are drawn with: its region's, or
    /// the floating drawer's.
    pub(super) fn inspector_theme(&self, theme: &Theme) -> Theme {
        if self.inspector_drawer.is_overlay() {
            theme.on_ground(ui::chrome::panel_ground(theme))
        } else {
            theme.in_region(Region::Inspector)
        }
    }

    fn draw_rail_contents(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let dragging = self.page_sidebar.is_dragging();
        let zone = dragging.then(|| self.slide_delete_zone(chrome.rail));
        self.draw_buttons(
            &mut PaintCtx::new(scene, t, tr, theme),
            pal,
            chrome,
            Zone::Rail,
            |p| !zone.is_some_and(|z| z.contains(p)),
        );
        if let Some(zone) = zone {
            let title = crate::strings::tr("slides-drop-to-delete");
            DropZoneFace::new(&title)
                .icon(Icon::Trash)
                .drag_over(self.over_delete_zone())
                .paint(&mut PaintCtx::new(scene, t, tr, theme), zone, 0.0);
        }
        self.draw_editor_rail_thumbnails(scene, t, tr, theme, chrome, pal);
    }

    fn draw_inspector_contents(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        pal: &Palette,
    ) {
        let header = Rect::new(
            chrome.inspector.x0,
            chrome.inspector.y0,
            chrome.inspector.x1,
            chrome.inspector_body.y0,
        );
        ui::surface::separator(
            scene,
            t,
            header.x0,
            header.x1,
            header.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let in_header = |p: Point| header.contains(p);
        Self::draw_labels(scene, t, tr, theme, chrome, Zone::Inspector, in_header);
        self.draw_buttons(
            &mut PaintCtx::new(scene, t, tr, theme),
            pal,
            chrome,
            Zone::Inspector,
            in_header,
        );
        // The body is clipped to its viewport: a scrolled section is cut at
        // the header rather than running under it.
        let body = chrome.inspector_body;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        let in_body = |p: Point| body.contains(p);
        self.draw_buttons(
            &mut PaintCtx::new(scene, t, tr, theme),
            pal,
            chrome,
            Zone::Inspector,
            in_body,
        );
        Self::draw_labels(scene, t, tr, theme, chrome, Zone::Inspector, in_body);
        if let Some(notes) = chrome.notes_box {
            self.draw_notes(scene, t, tr, theme, notes);
        }
        scene.pop_layer();
        if body.width() > 1.0 {
            self.inspector_scroll.draw_bar_overlay(scene, t, theme);
        }
    }

    fn draw_labels(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
        zone: Zone,
        keep: impl Fn(Point) -> bool,
    ) {
        for (_, l) in chrome
            .labels
            .iter()
            .enumerate()
            .filter(|(i, l)| chrome.label_zone(*i) == zone && keep(l.pos))
        {
            let style = theme.text(l.role);
            let text = match l.max_w {
                Some(w) if w < 1.0 => continue,
                Some(w) => tr.elide_to_width_style(&l.text, w as f32, &style),
                None => l.text.clone(),
            };
            let ink = if l.dim {
                theme.tokens.colors.text_dim
            } else {
                theme.tokens.colors.text_primary
            };
            tr.draw_style(
                scene,
                &text,
                ink,
                t * Affine::translate((l.pos.x, l.pos.y)),
                None,
                &style,
            );
        }
    }

    /// Paint every button whose centre `keep` accepts, with `theme` (the
    /// theme of the ground it stands on). Consecutive segments of one group
    /// paint as one strip.
    fn draw_buttons(
        &self,
        ctx: &mut PaintCtx<'_>,
        pal: &Palette,
        chrome: &Chrome,
        zone: Zone,
        keep: impl Fn(Point) -> bool,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let kept: Vec<&UiButton> = chrome
            .zone_buttons(zone)
            .filter(|b| keep(b.rect.center()))
            .collect();
        let mut i = 0;
        while let Some(b) = kept.get(i) {
            if let ButtonKind::Segment { group, .. } = b.kind {
                let row: Vec<&UiButton> = kept
                    .iter()
                    .skip(i)
                    .take_while(|s| {
                        matches!(s.kind, ButtonKind::Segment { group: g, .. } if g == group)
                            && (s.rect.y0 - b.rect.y0).abs() < 0.5
                    })
                    .copied()
                    .collect();
                self.draw_segments(scene, t, tr, theme, &row);
                i += row.len().max(1);
                continue;
            }
            self.draw_button(scene, t, tr, theme, pal, b);
            i += 1;
        }
    }

    fn hovered(&self, rect: Rect) -> bool {
        rect.contains(self.last_mouse) && !self.drag.active()
    }

    fn draw_segments(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: &[&UiButton],
    ) {
        let rects: Vec<Rect> = row.iter().map(|b| b.rect).collect();
        let segments: Vec<Segment> = row
            .iter()
            .map(|b| match &b.kind {
                ButtonKind::Segment {
                    label,
                    icon: Some(icon),
                    ..
                } => Segment::icon(*icon, label.as_str()),
                ButtonKind::Segment { label, .. } => Segment::new(label.as_str()),
                _ => Segment::new(""),
            })
            .collect();
        let selected = row
            .iter()
            .position(|b| matches!(b.kind, ButtonKind::Segment { active: true, .. }))
            .unwrap_or(row.len());
        let hovered = row.iter().position(|b| self.hovered(b.rect));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::of_segments(&rects, &segments, selected).hovered(hovered),
        );
    }

    pub(super) fn draw_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        b: &UiButton,
    ) {
        let hovered = self.hovered(b.rect);
        match &b.kind {
            ButtonKind::Command { label, icon, role } => {
                let face = ButtonFace::new(b.rect, label).role(*role).hovered(hovered);
                let face = match icon {
                    Some(icon) => face.icon(*icon),
                    None => face,
                };
                Button::paint(scene, t, tr, theme, &face);
            }
            ButtonKind::Icon { icon, name, active } => {
                let face = ButtonFace::icon_only(b.rect, *icon, name)
                    .toggled(*active)
                    .hovered(hovered);
                Button::paint(scene, t, tr, theme, &face);
            }
            ButtonKind::Segment { .. } => {
                self.draw_segments(scene, t, tr, theme, &[b]);
            }
            ButtonKind::Swatch { role, selected } => {
                self.swatch_face(pal, b.rect, role)
                    .selected(*selected)
                    .hovered(hovered)
                    .paint(scene, t, tr, theme);
            }
            ButtonKind::Theme { kind, selected } => {
                let face = ButtonFace::new(b.rect, kind.label())
                    .icon(Icon::Circle)
                    .toggled(*selected)
                    .hovered(hovered);
                Button::paint(scene, t, tr, theme, &face);
                // The swatch is the chip's icon: painted over the icon's
                // own place, so the chip shows one mark, not a ring beside
                // a dot.
                let swatch = face.icon_rect(tr, theme).unwrap_or(b.rect);
                ColorSwatchFace::new(
                    swatch,
                    Palette::resolve(*kind, self.deck.look).accent,
                    kind.label(),
                )
                .target_rect(b.rect)
                .selected(*selected)
                .hovered(hovered)
                .paint(scene, t, tr, theme);
            }
            ButtonKind::Notes | ButtonKind::LayoutPicker => {}
        }
    }
}
