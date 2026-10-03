//! The editor's chrome: the slide sidebar, the content column's header row
//! and the properties inspector, laid out from the theme's metrics and
//! painted through the toolkit's faces.
//!
//! The geometry is renderer-free ([`SlidesApp::compute_chrome`]) so the
//! painter, the pointer and the accessibility tree read the same rects; the
//! only measured inputs are the label widths [`ChromeMetrics`] caches from
//! the renderer at the top of every frame.

use super::{
    Affine, ArgValue, Background, BlendMode, Element, ElementKind, FILL_ROLES, Fill, HashMap, Hit,
    Icon, Layout, Mode, PaintCtx, Palette, PaletteKind, Point, Rect, SLIDE_H, SLIDE_W,
    STROKE_ROLES, Scene, SidebarDrawer, SlidesApp, SplitLayout, TextAlign, TextRenderer, Theme,
    custom_colour_hex, tr, tr_args,
};

use ui::TextRole;
use ui::chrome::{SelectionState, paint_row_selection};
use ui::surface::Region;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ColorSwatchFace, ControlSize, DropZoneFace, SegmentedFace,
    ToggleGroup,
};

/// What a measured width belongs to: a button face (with or without a
/// leading icon) or a line of text in a role.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Measure {
    /// A `Regular` button face.
    Button { icon: bool },
    /// A line of `Heading` text (the deck title).
    Heading,
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
    /// The one header row across the window.
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
    /// A text field's inset from its edge to its text.
    pub(super) field_pad: f64,
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
            field_pad: s3,
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
                    let face =
                        ButtonFace::new(Rect::ZERO, &label).control_size(ControlSize::Regular);
                    let face = if icon { face.icon(Icon::Plus) } else { face };
                    face.intrinsic_width(tr, theme)
                }
                Measure::Heading => f64::from(
                    tr.measure_style(&label, None, &theme.text(TextRole::Heading))
                        .0,
                ),
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
            Measure::Heading => glyphs * self.lh_heading * 0.5,
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
    /// A labelled on/off command (bold, italic): a lift while on.
    Toggle { label: String, active: bool },
    /// One segment of a segmented strip. Consecutive segments of one
    /// `group` on one row paint as one strip.
    Segment {
        label: String,
        active: bool,
        group: u8,
    },
    /// A fill or stroke colour role.
    Swatch { role: String, selected: bool },
    /// A deck colour theme.
    Theme { kind: PaletteKind, selected: bool },
    /// The speaker-notes field (painted by its text field).
    Notes,
}

#[derive(Clone)]
pub(super) struct UiButton {
    pub(super) rect: Rect,
    pub(super) kind: ButtonKind,
    pub(super) hit: Hit,
}

/// Segment groups of the inspector.
const SEG_ALIGN: u8 = 0;
const SEG_BACKGROUND: u8 = 1;

/// Which column of the window a control or a label belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Zone {
    /// The content column and its header row.
    Content,
    /// The slide sidebar.
    Rail,
    /// The inspector.
    Inspector,
}

/// Where each column's controls and labels sit in [`Chrome::buttons`] and
/// [`Chrome::labels`]: content first, then the sidebar, then the inspector.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ZoneStarts {
    rail_buttons: usize,
    inspector_buttons: usize,
    rail_labels: usize,
    inspector_labels: usize,
}

/// The editor chrome resolved for one frame.
pub(super) struct Chrome {
    /// The content column's header band: the deck title, the insert tools
    /// and Present.
    pub(super) toolbar: Rect,
    /// Click target of the deck title (rename).
    pub(super) title_rect: Rect,
    /// Top-left of the title's line box.
    pub(super) title_pos: Point,
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
    /// Insert tools that did not fit the header row, reached through the
    /// More button's menu (bounds are the More button's).
    pub(super) overflow: Vec<UiButton>,
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
    /// The column the button at `index` belongs to.
    pub(super) fn button_zone(&self, index: usize) -> Zone {
        if index >= self.zones.inspector_buttons {
            Zone::Inspector
        } else if index >= self.zones.rail_buttons {
            Zone::Rail
        } else {
            Zone::Content
        }
    }

    /// The column the label at `index` belongs to.
    pub(super) fn label_zone(&self, index: usize) -> Zone {
        if index >= self.zones.inspector_labels {
            Zone::Inspector
        } else if index >= self.zones.rail_labels {
            Zone::Rail
        } else {
            Zone::Content
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

/// The insert tools, in toolbar order.
pub(super) fn insert_tools() -> [(String, Hit); 5] {
    [
        (tr("slides-tool-text"), Hit::Insert(ElementKind::Text)),
        (tr("slides-tool-rectangle"), Hit::Insert(ElementKind::Rect)),
        (tr("slides-tool-ellipse"), Hit::Insert(ElementKind::Ellipse)),
        (tr("slides-tool-line"), Hit::Insert(ElementKind::Line)),
        (tr("slides-tool-image"), Hit::Insert(ElementKind::Image)),
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
        let mut set = vec![
            (tr("slides-present"), Measure::Button { icon: true }),
            (self.deck.title.clone(), Measure::Heading),
            (String::new(), Measure::FontInput),
            button(tr("slides-bring-front")),
            button(tr("slides-send-back")),
            button(tr("slides-duplicate")),
            button(tr("slides-delete")),
            button(tr("slides-custom-colour")),
            button(tr("slides-bold")),
            button(tr("slides-italic")),
        ];
        set.extend(insert_tools().into_iter().map(|(label, _)| button(label)));
        set.extend(Layout::ALL.iter().map(|l| button(l.label().to_owned())));
        set.extend(
            TextAlign::ALL
                .iter()
                .map(|a| a.glyph())
                .chain(Background::ALL.iter().map(|b| b.label()))
                .map(|label| (label.to_owned(), Measure::Segment)),
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
        self.chrome_metrics.field_pad = self.title_input.text_padding_x(theme);
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
        let m = &self.chrome_metrics;
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
        let toolbar = Rect::new(content.x0, r.y0, content.x1, r.y0 + m.header_h);
        let canvas = Rect::new(content.x0, toolbar.y1, content.x1, r.y1);
        let (slide_dest, scale) = self.fit_slide(canvas);
        let scrim = if (layout == SplitLayout::Overlay && self.rail_drawer.reveal() > 0.001)
            || (inspector_layout == SplitLayout::Overlay && self.inspector_drawer.reveal() > 0.001)
        {
            r
        } else {
            Rect::ZERO
        };

        let mut buttons = Vec::new();
        let mut labels = Vec::new();
        let (title_rect, title_pos, overflow) = self.build_toolbar(
            toolbar,
            !rail_docked || !self.rail_open(),
            !inspector_docked || !self.inspector_open(),
            &mut buttons,
            &mut labels,
        );

        let mut zones = ZoneStarts {
            rail_buttons: buttons.len(),
            rail_labels: labels.len(),
            ..ZoneStarts::default()
        };
        let rail_viewport = if rail.width() > 1.0 {
            self.build_rail_chrome(rail, &mut buttons, &mut labels)
        } else {
            Rect::ZERO
        };
        let thumbs = self.page_sidebar.thumb_rects();

        zones.inspector_buttons = buttons.len();
        zones.inspector_labels = labels.len();
        let (inspector_body, inspector_content_h, notes_box, font_input_rect) =
            if inspector.width() > 1.0 {
                self.build_inspector(inspector, &mut buttons, &mut labels)
            } else {
                (Rect::ZERO, 0.0, None, None)
            };
        let fill_picker_rect = self.fill_picker_popover_rect(inspector, r);

        Chrome {
            toolbar,
            title_rect,
            title_pos,
            rail,
            inspector,
            inspector_body,
            inspector_content_h,
            slide_dest,
            scale,
            thumbs,
            buttons,
            labels,
            overflow,
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

    /// Fit the 16:9 slide inside `canvas` with the content gutter round it.
    /// Returns the destination rect and the slide→pixel scale factor.
    pub(super) fn fit_slide(&self, canvas: Rect) -> (Rect, f64) {
        let pad = self.chrome_metrics.s6;
        let avail_w = (canvas.width() - 2.0 * pad).max(1.0);
        let avail_h = (canvas.height() - 2.0 * pad).max(1.0);
        let s = (avail_w / SLIDE_W).min(avail_h / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = canvas.x0 + (canvas.width() - sw) / 2.0;
        let sy = canvas.y0 + (canvas.height() - sh) / 2.0;
        (Rect::new(sx, sy, sx + sw, sy + sh), s)
    }

    /// An icon-only square control centred on the band at `x`.
    pub(super) fn chip(&self, band: Rect, x: f64) -> Rect {
        let side = self.chrome_metrics.target;
        let y = band.y0 + (band.height() - side) / 2.0;
        Rect::new(x, y, x + side, y + side)
    }

    /// The content column's header row: the sidebar toggle while the
    /// sidebar is not docked open, the deck title (elided only when nothing
    /// else gives way), the insert tools (the ones that do not fit go behind
    /// a More button), Present, and the inspector toggle while the inspector
    /// is not docked open. Returns the title's click target and origin, and
    /// the tools behind More.
    fn build_toolbar(
        &self,
        toolbar: Rect,
        rail_toggle: bool,
        inspector_toggle: bool,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> (Rect, Point, Vec<UiButton>) {
        let m = &self.chrome_metrics;
        let control_y = toolbar.y0 + (toolbar.height() - m.control_h) / 2.0;
        let mut x = toolbar.x0 + m.s3;
        if rail_toggle {
            let chip = self.chip(toolbar, x);
            buttons.push(UiButton {
                rect: chip,
                kind: ButtonKind::Icon {
                    icon: Icon::PanelLeft,
                    name: tr("slides-show-slides"),
                    active: false,
                },
                hit: Hit::ToggleRail,
            });
            x = chip.x1 + m.s2;
        }
        let mut right = toolbar.x1 - m.s3;
        if inspector_toggle {
            let chip = self.chip(toolbar, right - m.target);
            buttons.push(UiButton {
                rect: chip,
                kind: ButtonKind::Icon {
                    icon: Icon::PanelRight,
                    name: tr("slides-show-properties"),
                    active: false,
                },
                hit: Hit::ToggleInspector,
            });
            right = chip.x0 - m.s2;
        }
        let present_label = tr("slides-present");
        let present_w = m.width(&present_label, Measure::Button { icon: true });
        let present = Rect::new(
            (right - present_w).max(x),
            control_y,
            right,
            control_y + m.control_h,
        );
        buttons.push(UiButton {
            rect: present,
            kind: ButtonKind::Command {
                label: present_label,
                icon: Some(Icon::Play),
                role: ButtonRole::Primary,
            },
            hit: Hit::Present,
        });
        let right = present.x0 - m.s4;

        // The title keeps its measured width while the tools give way; it
        // elides only once every tool is behind the More button.
        let title_w = if self.editing_title {
            m.width(self.title_input.value(), Measure::Heading)
                .max(m.width(&self.deck.title, Measure::Heading))
        } else {
            m.width(&self.deck.title, Measure::Heading)
        };
        let tools = insert_tools();
        let tool_w: Vec<f64> = tools
            .iter()
            .map(|(label, _)| m.width(label, Measure::Button { icon: false }))
            .collect();
        let all_w = tool_w.iter().sum::<f64>() + m.s1 * (tool_w.len() as f64 - 1.0);
        let room = (right - x).max(0.0);
        let tools_x = |title: f64| x + title + if title > 0.0 { m.s4 } else { 0.0 };
        let (title_max, shown) = if title_w + m.s4 + all_w <= room {
            (title_w, tools.len())
        } else {
            // Some tools go behind More: as many as fit beside the title.
            let mut shown = 0;
            let mut used = m.target;
            for w in &tool_w {
                if title_w + m.s4 + used + w + m.s1 <= room {
                    used += w + m.s1;
                    shown += 1;
                } else {
                    break;
                }
            }
            let title_max = if shown == 0 {
                (room - m.s4 - m.target).max(0.0).min(title_w)
            } else {
                title_w
            };
            (title_max, shown)
        };
        let mut tx = tools_x(title_max);
        for ((label, hit), w) in tools.iter().zip(&tool_w).take(shown) {
            buttons.push(UiButton {
                rect: Rect::new(tx, control_y, tx + w, control_y + m.control_h),
                kind: ButtonKind::Command {
                    label: label.clone(),
                    icon: None,
                    role: ButtonRole::Plain,
                },
                hit: hit.clone(),
            });
            tx += w + m.s1;
        }
        let mut overflow = Vec::new();
        if shown < tools.len() {
            let more = self.chip(toolbar, tx.min(right - m.target).max(x));
            buttons.push(UiButton {
                rect: more,
                kind: ButtonKind::Icon {
                    icon: Icon::MoreHorizontal,
                    name: tr("slides-a11y-more-tools"),
                    active: self.ctx_menu.is_open() && self.toolbar_menu_open,
                },
                hit: Hit::ToggleToolbarMenu,
            });
            overflow = tools
                .iter()
                .skip(shown)
                .map(|(label, hit)| UiButton {
                    rect: more,
                    kind: ButtonKind::Command {
                        label: label.clone(),
                        icon: None,
                        role: ButtonRole::Plain,
                    },
                    hit: hit.clone(),
                })
                .collect();
        }

        let title_top = toolbar.y0 + (toolbar.height() - m.lh_heading) / 2.0;
        let title_pos = Point::new(x, title_top);
        labels.push(Label {
            pos: title_pos,
            text: if self.editing_title || title_max < 1.0 {
                String::new()
            } else {
                self.deck.title.clone()
            },
            role: TextRole::Heading,
            dim: false,
            max_w: Some(title_max),
        });
        let title_rect = Rect::new(
            x - m.s1,
            control_y,
            x + title_max + m.s1,
            control_y + m.control_h,
        );
        (title_rect, title_pos, overflow)
    }

    /// The slide sidebar: its header (the sidebar toggle, the "Slides"
    /// title and the overview), the footer actions and, between them, the
    /// thumbnail list's viewport, which the shared [`PageSidebar`] owns.
    fn build_rail_chrome(
        &self,
        rail: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> Rect {
        let m = &self.chrome_metrics;
        let header = Rect::new(rail.x0, rail.y0, rail.x1, rail.y0 + m.header_h);
        let toggle = self.chip(header, rail.x0 + m.s3);
        buttons.push(UiButton {
            rect: toggle,
            kind: ButtonKind::Icon {
                icon: Icon::PanelLeft,
                name: tr("slides-hide-slides"),
                active: false,
            },
            hit: Hit::ToggleRail,
        });
        let overview = self.chip(header, rail.x1 - m.s3 - m.target);
        buttons.push(UiButton {
            rect: overview,
            kind: ButtonKind::Icon {
                icon: Icon::LayoutGrid,
                name: tr("slides-a11y-slide-overview"),
                active: self.mode == Mode::Overview,
            },
            hit: Hit::Overview,
        });
        let title_x = toggle.x1 + m.s2;
        labels.push(Label {
            pos: Point::new(title_x, header.y0 + (header.height() - m.lh_heading) / 2.0),
            text: tr("slides-slides"),
            role: TextRole::Heading,
            dim: false,
            max_w: Some((overview.x0 - m.s2 - title_x).max(0.0)),
        });

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
        Rect::new(
            rail.x0,
            header.y1,
            rail.x1,
            (footer_y - m.s3).max(header.y1),
        )
    }

    /// The drop-to-delete zone that takes the place of the sidebar's
    /// footer while a thumbnail is dragged.
    pub(super) fn slide_delete_zone(&self, rail: Rect) -> Rect {
        let m = &self.chrome_metrics;
        let top = (rail.y1 - m.s3 - m.drop_zone_h).max(rail.y0 + m.header_h);
        Rect::new(rail.x0 + m.s3, top, rail.x1 - m.s3, rail.y1 - m.s3)
    }

    /// The thumbnail list's viewport inside `rail` (see
    /// [`Self::build_rail_chrome`]): above the footer, or above the
    /// drop-to-delete zone while a thumbnail is dragged.
    pub(super) fn rail_viewport_of(&self, rail: Rect) -> Rect {
        let m = &self.chrome_metrics;
        let top = rail.y0 + m.header_h;
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
        let y = (r.y0 + m.header_h + top + m.s2)
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

    /// The inspector: its header (what is selected, and the inspector
    /// toggle) and its scrolling body of sections, ending in the speaker
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
        let toggle = self.chip(header, inspector.x1 - m.s3 - m.target);
        buttons.push(UiButton {
            rect: toggle,
            kind: ButtonKind::Icon {
                icon: Icon::PanelRight,
                name: tr("slides-hide-properties"),
                active: false,
            },
            hit: Hit::ToggleInspector,
        });
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
            max_w: Some((toggle.x0 - m.s2 - title_x).max(0.0)),
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
            .filter(|b| matches!(b.kind, ButtonKind::Notes) || inside(b.rect.center().y))
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
        y = self.swatch_grid(buttons, x0, x1, y, &FILL_ROLES, &el.fill, true);
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
            let bold_w = m.width(&tr("slides-bold"), Measure::Button { icon: false });
            let italic_w = m.width(&tr("slides-italic"), Measure::Button { icon: false });
            let toggles_w = bold_w + m.s1 + italic_w;
            let toggles_y = if field_w + m.s2 + toggles_w <= x1 - x0 + 0.5 {
                y
            } else {
                y + m.control_h + m.s2
            };
            let bold = Rect::new(
                x1 - toggles_w,
                toggles_y,
                x1 - italic_w - m.s1,
                toggles_y + m.control_h,
            );
            buttons.push(UiButton {
                rect: bold,
                kind: ButtonKind::Toggle {
                    label: tr("slides-bold"),
                    active: el.bold,
                },
                hit: Hit::ToggleBold,
            });
            buttons.push(UiButton {
                rect: Rect::new(x1 - italic_w, toggles_y, x1, toggles_y + m.control_h),
                kind: ButtonKind::Toggle {
                    label: tr("slides-italic"),
                    active: el.italic,
                },
                hit: Hit::ToggleItalic,
            });
            y = toggles_y + m.control_h + m.s6;

            Self::push_section_label(m, labels, x0, &mut y, &tr("slides-alignment"));
            let items: Vec<(String, bool, Hit)> = TextAlign::ALL
                .iter()
                .map(|a| (a.glyph().to_owned(), *a == el.align, Hit::SetAlign(*a)))
                .collect();
            y = self.segment_rows(buttons, x0, x1, y, SEG_ALIGN, items);
            y += m.s6;
        }

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-arrange"));
        let actions = [
            (
                tr("slides-bring-front"),
                ButtonRole::Secondary,
                Hit::BringFront,
            ),
            (tr("slides-send-back"), ButtonRole::Secondary, Hit::SendBack),
            (
                tr("slides-duplicate"),
                ButtonRole::Secondary,
                Hit::DuplicateElement,
            ),
            (
                tr("slides-delete"),
                ButtonRole::Destructive,
                Hit::DeleteElement,
            ),
        ];
        let half = (x1 - x0 - m.s2) / 2.0;
        let widest = actions
            .iter()
            .map(|(label, ..)| m.width(label, Measure::Button { icon: false }))
            .fold(0.0_f64, f64::max);
        let cols = if widest <= half + 0.5 { 2 } else { 1 };
        let cell = if cols == 2 { half } else { x1 - x0 };
        for (i, (label, role, hit)) in actions.into_iter().enumerate() {
            let col = (i % cols) as f64;
            let row = (i / cols) as f64;
            let x = x0 + col * (cell + m.s2);
            let top = y + row * (m.control_h + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(x, top, x + cell, top + m.control_h),
                kind: ButtonKind::Command {
                    label,
                    icon: None,
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
        let current = self.slide().map_or(Layout::TitleContent, |s| s.layout);
        buttons.push(UiButton {
            rect: Rect::new(x0, y, x1, y + m.control_h),
            kind: ButtonKind::Command {
                label: current.label().to_owned(),
                icon: None,
                role: ButtonRole::Secondary,
            },
            hit: Hit::OpenLayoutMenu,
        });
        y += m.control_h + m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-background"));
        let background = self.slide().map_or(Background::Surface, |s| s.background);
        let items: Vec<(String, bool, Hit)> = Background::ALL
            .iter()
            .map(|b| {
                (
                    b.label().to_owned(),
                    *b == background,
                    Hit::SetBackground(*b),
                )
            })
            .collect();
        y = self.segment_rows(buttons, x0, x1, y, SEG_BACKGROUND, items);
        y += m.s6;

        Self::push_section_label(m, labels, x0, &mut y, &tr("slides-deck-theme"));
        let n = PaletteKind::ALL.len() as f64;
        let w = (x1 - x0 - (n - 1.0) * m.s2) / n;
        for (i, kind) in PaletteKind::ALL.iter().enumerate() {
            let x = x0 + i as f64 * (w + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + m.small_h),
                kind: ButtonKind::Theme {
                    kind: *kind,
                    selected: *kind == self.deck.palette,
                },
                hit: Hit::SetPalette(*kind),
            });
        }
        y + m.small_h + m.s6
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
        items: Vec<(String, bool, Hit)>,
    ) -> f64 {
        let m = &self.chrome_metrics;
        let n = items.len().max(1);
        let widest = items
            .iter()
            .map(|(label, ..)| m.width(label, Measure::Segment))
            .fold(0.0_f64, f64::max);
        let cols = if widest <= (x1 - x0) / n as f64 + 0.5 {
            n
        } else {
            2
        };
        let seg_w = (x1 - x0) / cols as f64;
        for (i, (label, active, hit)) in items.into_iter().enumerate() {
            let col = i % cols;
            let row = (i / cols) as f64;
            let sx = x0 + col as f64 * seg_w;
            let ex = if col + 1 == cols { x1 } else { sx + seg_w };
            let sy = y + row * (m.control_h + m.s2);
            buttons.push(UiButton {
                rect: Rect::new(sx, sy, ex, sy + m.control_h),
                kind: ButtonKind::Segment {
                    label,
                    active,
                    group,
                },
                hit,
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

    // --- painting -----------------------------------------------------------

    /// Whether a point falls in the slide sidebar.
    pub(super) fn in_rail(chrome: &Chrome, p: Point) -> bool {
        chrome.rail.width() > 1.0 && chrome.rail.contains(p)
    }

    /// Whether a point falls in the inspector.
    pub(super) fn in_inspector(chrome: &Chrome, p: Point) -> bool {
        chrome.inspector.width() > 1.0 && chrome.inspector.contains(p)
    }

    pub(super) fn in_panel(chrome: &Chrome, p: Point) -> bool {
        Self::in_rail(chrome, p) || Self::in_inspector(chrome, p)
    }

    /// The whole editor: the docked panels' regions, the canvas, the
    /// content column's header, the panels' contents, and any open drawer
    /// over its scrim.
    pub(super) fn draw_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let pal = self.palette(theme);
        let rail_docked = chrome.layout == SplitLayout::Docked;
        let inspector_docked = chrome.inspector_layout == SplitLayout::Docked;

        // The canvas stands on the window ground the framework painted. A
        // slide's type is sized in slide units, whatever the reader's text
        // scale.
        let reader_scale = tr.text_scale();
        tr.set_text_scale(1.0);
        self.draw_editor_canvas(scene, t, tr, theme, chrome, &pal);
        self.draw_text_editor(scene, t, tr, theme, chrome, &pal);
        tr.set_text_scale(reader_scale);

        // The content column's header: a hairline under it, its controls.
        ui::surface::separator(
            scene,
            t,
            chrome.toolbar.x0,
            chrome.toolbar.x1,
            chrome.toolbar.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        Self::draw_labels(scene, t, tr, theme, chrome, Zone::Content, |_| true);
        self.draw_buttons(
            &mut PaintCtx::new(scene, t, tr, theme),
            &pal,
            chrome,
            Zone::Content,
            |_| true,
        );

        if rail_docked && chrome.rail.width() > 1.0 {
            let local = self.rail_drawer.paint_panel(scene, t, theme);
            self.draw_rail_contents(scene, t, tr, &local, chrome, &pal);
        }
        if inspector_docked && chrome.inspector.width() > 1.0 {
            let local = self.inspector_drawer.paint_panel(scene, t, theme);
            self.draw_inspector_contents(scene, t, tr, &local, chrome, &pal);
        }

        // An open drawer floats over everything, behind its scrim.
        let rail_floating = !rail_docked && chrome.rail.width() > 1.0;
        let inspector_floating = !inspector_docked && chrome.inspector.width() > 1.0;
        if rail_floating {
            self.rail_drawer.draw_scrim(scene, t, theme);
        } else if inspector_floating {
            self.inspector_drawer.draw_scrim(scene, t, theme);
        }
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
        Self::draw_labels(scene, t, tr, theme, chrome, Zone::Rail, |_| true);
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
        let labels: Vec<&str> = row
            .iter()
            .map(|b| match &b.kind {
                ButtonKind::Segment { label, .. } => label.as_str(),
                _ => "",
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
            &SegmentedFace::new(&rects, &labels, selected).hovered(hovered),
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
        let ground = theme.tokens.colors.surfaces.content;
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
                if *active {
                    paint_row_selection(
                        scene,
                        t,
                        b.rect,
                        SelectionState::active(true),
                        ground,
                        theme,
                    );
                }
                let face = ButtonFace::icon_only(b.rect, *icon, name).hovered(hovered && !active);
                Button::paint(scene, t, tr, theme, &face);
            }
            ButtonKind::Toggle { label, active } => {
                if *active {
                    paint_row_selection(
                        scene,
                        t,
                        b.rect,
                        SelectionState::active(true),
                        ground,
                        theme,
                    );
                }
                let face = ButtonFace::new(b.rect, label)
                    .role(ButtonRole::Plain)
                    .hovered(hovered && !active);
                Button::paint(scene, t, tr, theme, &face);
            }
            ButtonKind::Segment { .. } => {
                self.draw_segments(scene, t, tr, theme, &[b]);
            }
            ButtonKind::Swatch { role, selected } => {
                let mut on_slide = *pal;
                if let Some(slide) = self.slide() {
                    on_slide = on_slide.on(slide.background);
                }
                // "No stroke" shows the ground it would leave visible.
                let color = on_slide.resolve_role(role).unwrap_or(ground);
                ColorSwatchFace::new(b.rect, color, role_name(role))
                    .selected(*selected)
                    .hovered(hovered)
                    .paint(scene, t, tr, theme);
            }
            ButtonKind::Theme { kind, selected } => {
                ColorSwatchFace::new(b.rect, Palette::resolve(*kind, theme).accent, kind.label())
                    .selected(*selected)
                    .hovered(hovered)
                    .paint(scene, t, tr, theme);
            }
            ButtonKind::Notes => {}
        }
    }
}
