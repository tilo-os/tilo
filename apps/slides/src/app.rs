//! Root widget for the Slides app.
//!
//! `SlidesApp` is a single full-window widget that lays out and paints the
//! whole editor itself (toolbar, thumbnail rail, slide canvas, inspector)
//! plus a full-screen present mode. The deck lives in the shared CRDT
//! ([`crate::doc::SlidesDoc`]); this widget keeps an in-memory mirror that
//! is reloaded from the document every `sync`, so undo/redo and live
//! collaboration work without extra plumbing. View state — current slide,
//! selection, mode — is per-peer and stays local.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::icons::{draw_icon, Icon};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget, DEFAULT_BREAKPOINT};
use ui::ColorCompat;

use crate::chrome;
use crate::doc::SlidesDoc;
use crate::model::{
    new_id, seed_deck, Background, Deck, Element, ElementKind, Id, Layout, Palette, PaletteKind,
    Slide, TextAlign, FILL_ROLES, SLIDE_H, SLIDE_W, STROKE_ROLES,
};
use crate::render;

const TOOLBAR_H: f64 = 56.0;
const RAIL_W: f64 = 212.0;
const INSPECTOR_W: f64 = 296.0;
const HANDLE: f64 = 9.0;

/// Below this allocated width (logical px) the editor switches from the
/// docked side-by-side layout to a narrow layout where the slide rail
/// collapses into an overlay flap. The slide canvas plus both side panels
/// (rail + inspector) only fit comfortably above this width.
const NARROW_BREAKPOINT: f64 = DEFAULT_BREAKPOINT;
/// Size of the toolbar hamburger toggle that summons the collapsed rail.
const HAMBURGER: f64 = 34.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Edit,
    /// Slide-sorter: a full-window grid of every slide thumbnail.
    Overview,
    Present,
}

/// An on-canvas alignment guide produced by the snapping logic while a
/// drag is live. Coordinates are in slide space; the renderer maps them
/// to the destination rect.
#[derive(Clone, Copy)]
enum Guide {
    Vertical(f64),
    Horizontal(f64),
}

/// How long a present-mode slide cross-fade lasts.
const TRANSITION_MS: u128 = 240;
/// Snap threshold in *pixels*; converted to slide units per-drag.
const SNAP_PX: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    Tl,
    T,
    Tr,
    R,
    Br,
    B,
    Bl,
    L,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::Tl,
        Handle::T,
        Handle::Tr,
        Handle::R,
        Handle::Br,
        Handle::B,
        Handle::Bl,
        Handle::L,
    ];
}

#[derive(Clone)]
enum Drag {
    None,
    Move {
        id: Id,
        start: Point,
        orig_x: f64,
        orig_y: f64,
    },
    Resize {
        id: Id,
        handle: Handle,
        start: Point,
        orig: (f64, f64, f64, f64),
    },
}

impl Drag {
    fn active(&self) -> bool {
        !matches!(self, Drag::None)
    }
}

#[derive(Clone)]
enum Hit {
    NewSlide,
    DuplicateSlide,
    DeleteSlide,
    Present,
    Overview,
    OpenSlide(usize),
    CyclePalette,
    Insert(ElementKind),
    SelectSlide(usize),
    MoveSlideUp,
    MoveSlideDown,
    SetFill(String),
    SetStroke(String),
    FontDelta(f64),
    ToggleBold,
    ToggleItalic,
    CycleAlign,
    BringFront,
    SendBack,
    DuplicateElement,
    DeleteElement,
    SetLayout(Layout),
    SetBackground(Background),
    SetPalette(PaletteKind),
    EditNotes,
    /// Toggle the collapsible slide rail (narrow-window hamburger).
    ToggleRail,
}

#[derive(Clone, Copy)]
enum LabelColor {
    Primary,
    Dim,
    Accent,
}

#[derive(Clone)]
struct Label {
    pos: Point,
    text: String,
    font: f32,
    color: LabelColor,
    weight: TextWeight,
}

#[derive(Clone)]
enum ButtonKind {
    Text {
        label: String,
        active: bool,
        danger: bool,
    },
    IconText {
        icon: Icon,
        label: String,
        accent: bool,
    },
    IconOnly {
        icon: Icon,
        active: bool,
    },
    Swatch {
        role: String,
        selected: bool,
    },
    Palette {
        kind: PaletteKind,
        selected: bool,
    },
    /// Three-line "hamburger" that summons the collapsed slide rail.
    Hamburger,
}

#[derive(Clone)]
struct UiButton {
    rect: Rect,
    kind: ButtonKind,
    hit: Hit,
}

struct Chrome {
    toolbar: Rect,
    /// The rail's painted rect. When the rail is an overlay flap this slides
    /// in from the left edge (off-screen when closed); when docked it is the
    /// permanent left column.
    rail: Rect,
    inspector: Rect,
    slide_dest: Rect,
    scale: f64,
    thumbs: Vec<(usize, Rect)>,
    buttons: Vec<UiButton>,
    labels: Vec<Label>,
    notes_box: Option<Rect>,
    /// How the rail resolves at the current width (docked beside content, or
    /// an overlay flap over it).
    layout: SplitLayout,
    /// Dimming scrim behind an open overlay flap (empty when docked/closed).
    scrim: Rect,
    /// Eased reveal `0..=1` of the rail, for the scrim opacity.
    reveal: f64,
}

pub struct SlidesApp {
    deck: Deck,
    rect: Rect,
    node: Option<NodeId>,
    seeded: bool,
    bootstrapped: bool,

    mode: Mode,
    cur_slide: usize,
    sel: Option<Id>,
    editing_text: bool,
    caret: usize,
    editing_notes: bool,
    notes_caret: usize,
    show_notes: bool,

    /// Keyboard-shortcuts overlay visibility (toggled with `?`).
    show_help: bool,
    /// Snap-to-guides toggle (G in edit mode would clash with overview, so
    /// snapping is always on, but the active guide lines computed during the
    /// current drag live here so `draw` can paint them).
    active_guides: Vec<Guide>,
    /// Wall-clock start of the current presentation, for the speaker timer.
    present_started: Option<std::time::Instant>,
    /// In-flight present-mode cross-fade: `(from_slide_idx, started_at)`.
    transition: Option<(usize, std::time::Instant)>,
    /// Present-mode black-out (press `B`).
    blackout: bool,

    last_mouse: Point,
    drag: Drag,
    needs_commit: bool,

    /// Responsive slide-rail flap. On a wide window the rail is docked beside
    /// the canvas; on a narrow window it collapses to an overlay flap behind a
    /// scrim, closed by default and summoned by the toolbar hamburger. This is
    /// the shared toolkit easing kernel ([`FlapState`]).
    rail_flap: FlapState,
    /// Last instant the flap animation advanced, for frame-rate-independent
    /// easing across `draw` calls.
    last_flap_tick: std::time::Instant,
    /// Cleared after the first layout is resolved; until then the flap's eased
    /// reveal is snapped to the resolved default so the very first frame at a
    /// narrow width shows the (closed) flap without a one-frame flash.
    flap_uninitialised: bool,
    /// Width breakpoint between the docked and overlay rail layouts. A field
    /// (not a constant) so `--shots` can force a narrow layout deterministically.
    breakpoint: f64,
}

impl Default for SlidesApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SlidesApp {
    pub fn new() -> Self {
        let mut app = SlidesApp {
            deck: seed_deck(),
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            bootstrapped: false,
            mode: Mode::Edit,
            cur_slide: 1,
            sel: None,
            editing_text: false,
            caret: 0,
            editing_notes: false,
            notes_caret: 0,
            show_notes: false,
            show_help: false,
            active_guides: Vec::new(),
            present_started: None,
            transition: None,
            blackout: false,
            last_mouse: Point::ZERO,
            drag: Drag::None,
            needs_commit: false,
            rail_flap: FlapState::open_when_docked(),
            last_flap_tick: std::time::Instant::now(),
            flap_uninitialised: true,
            breakpoint: NARROW_BREAKPOINT,
        };
        // Optional headless-screenshot hook: pick an initial screen so
        // `--shots` can capture each mode. No effect on normal launch.
        if let Ok(shot) = std::env::var("SLIDES_SHOT") {
            match shot.as_str() {
                "overview" => app.mode = Mode::Overview,
                "present" => {
                    app.mode = Mode::Present;
                    app.present_started = Some(std::time::Instant::now());
                }
                "present_notes" => {
                    app.mode = Mode::Present;
                    app.show_notes = true;
                    app.present_started = Some(std::time::Instant::now());
                }
                "help" => app.show_help = true,
                // Narrow-layout capture (rendered at a phone-ish viewport, so
                // the rail resolves to an overlay flap from the width). `narrow`
                // snaps the flap open to show the summoned rail + scrim;
                // `narrow_closed` leaves it collapsed behind the hamburger. The
                // explicit snap is an intentional preference, so suppress the
                // first-frame default snap.
                "narrow" => {
                    app.rail_flap.snap(true);
                    app.flap_uninitialised = false;
                }
                "narrow_closed" => {
                    app.rail_flap.snap(false);
                    app.flap_uninitialised = false;
                }
                _ => {}
            }
        }
        app
    }

    /// The rail layout resolved from the current allocated width. Overview and
    /// present modes are full-window and never show the rail, but the editor
    /// (`Mode::Edit`) consults this every frame.
    fn rail_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), self.breakpoint)
    }

    /// Whether the rail is currently revealed (resolving the user's toggle
    /// against the layout default: docked ⇒ open, overlay ⇒ closed).
    fn rail_open(&self) -> bool {
        self.rail_flap.is_open(self.rail_layout())
    }

    /// Reconcile the rail flap with the current width and advance its eased
    /// reveal. On the very first resolved layout the reveal is snapped to the
    /// resolved default so a narrow first frame shows the (closed) flap with no
    /// one-frame flash. Returns the resolved layout. Called once per frame
    /// before `compute_chrome`.
    fn sync_rail_flap(&mut self, dt: f64) -> SplitLayout {
        let layout = self.rail_layout();
        self.rail_flap.reconcile(layout);
        if self.flap_uninitialised {
            self.rail_flap.snap(self.rail_flap.is_open(layout));
            // `snap` records a user preference; clear it so width changes still
            // fall back to the layout default. Re-resolving via reconcile after
            // a fresh open_when_docked would re-snap, so instead just mark done.
            self.flap_uninitialised = false;
        } else {
            self.rail_flap.tick(layout, dt);
        }
        layout
    }

    // --- CRDT mirror --------------------------------------------------------

    fn reload(&mut self, doc: &AutoCommit) {
        if self.drag.active() {
            return; // cache is authoritative mid-drag
        }
        if let Some(deck) = SlidesDoc::load(doc) {
            self.deck = deck;
        }
        if self.deck.slides.is_empty() {
            self.cur_slide = 0;
            self.sel = None;
            return;
        }
        if self.cur_slide >= self.deck.slides.len() {
            self.cur_slide = self.deck.slides.len() - 1;
        }
        // Drop selection if the element vanished (e.g. remote delete / undo).
        if let Some(id) = self.sel.clone() {
            let alive = self
                .deck
                .slides
                .get(self.cur_slide)
                .map(|s| s.element(&id).is_some())
                .unwrap_or(false);
            if !alive {
                self.sel = None;
                self.editing_text = false;
            }
        }
    }

    fn slide(&self) -> Option<&Slide> {
        self.deck.slides.get(self.cur_slide)
    }

    fn selected_element(&self) -> Option<&Element> {
        let id = self.sel.as_ref()?;
        self.slide()?.element(id)
    }

    fn palette(&self, theme: &Theme) -> Palette {
        Palette::resolve(self.deck.palette, theme)
    }

    fn mark(&mut self) {
        self.needs_commit = true;
    }

    // --- geometry -----------------------------------------------------------

    fn compute_chrome(&self) -> Chrome {
        let r = self.rect;
        let toolbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);

        // Resolve the responsive rail layout from the allocated width. When
        // docked the rail is a permanent left column that insets the canvas;
        // when narrow it becomes an overlay flap, so the canvas + inspector
        // span the full width and the rail slides over them behind a scrim.
        let layout = self.rail_layout();
        let reveal = self.rail_flap.reveal();
        let body_top = toolbar.y1;
        let (rail, canvas_left, scrim) = match layout {
            SplitLayout::Docked => {
                // Eased inset so the rail reveals/collapses smoothly when the
                // user toggles it even while docked.
                let inset = RAIL_W * reveal;
                let rail = Rect::new(r.x0, body_top, r.x0 + inset, r.y1);
                (rail, r.x0 + inset, Rect::ZERO)
            }
            SplitLayout::Overlay => {
                // Slide the rail in from the left edge; ease-out so it
                // decelerates into place. Closed ⇒ fully off-screen.
                let slide = self.rail_flap.slide();
                let x0 = r.x0 - RAIL_W * (1.0 - slide);
                let rail = Rect::new(x0, body_top, x0 + RAIL_W, r.y1);
                let scrim = if reveal > 0.001 {
                    Rect::new(r.x0, body_top, r.x1, r.y1)
                } else {
                    Rect::ZERO
                };
                // Canvas takes the full width behind the flap.
                (rail, r.x0, scrim)
            }
        };
        let inspector = Rect::new(r.x1 - INSPECTOR_W, body_top, r.x1, r.y1);
        let canvas_area = Rect::new(canvas_left, body_top, inspector.x0, r.y1);

        // Fit the 16:9 slide inside the canvas area with padding + caption room.
        let pad = 30.0;
        let cap = 34.0;
        let avail_w = (canvas_area.width() - 2.0 * pad).max(40.0);
        let avail_h = (canvas_area.height() - 2.0 * pad - cap).max(40.0);
        let s = (avail_w / SLIDE_W).min(avail_h / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = canvas_area.x0 + (canvas_area.width() - sw) / 2.0;
        let sy = canvas_area.y0 + pad + (avail_h - sh) / 2.0;
        let slide_dest = Rect::new(sx, sy, sx + sw, sy + sh);

        let mut buttons = Vec::new();
        let mut labels = Vec::new();

        // Toolbar -----------------------------------------------------------
        // When the rail can collapse (it's an overlay flap, or docked but
        // closed) a hamburger summons it. It sits at the far left; the title
        // block then shifts right to make room.
        let rail_collapsible = layout == SplitLayout::Overlay || reveal < 0.999;
        let title_x = if rail_collapsible {
            let hy = toolbar.y0 + (TOOLBAR_H - HAMBURGER) / 2.0;
            let hb = Rect::new(toolbar.x0 + 14.0, hy, toolbar.x0 + 14.0 + HAMBURGER, hy + HAMBURGER);
            buttons.push(UiButton {
                rect: hb,
                kind: ButtonKind::Hamburger,
                hit: Hit::ToggleRail,
            });
            hb.x1 + 14.0
        } else {
            toolbar.x0 + 18.0
        };
        labels.push(Label {
            pos: Point::new(title_x, toolbar.y0 + 11.0),
            text: self.deck.title.clone(),
            font: 17.0,
            color: LabelColor::Primary,
            weight: TextWeight::Bold,
        });
        labels.push(Label {
            pos: Point::new(title_x, toolbar.y0 + 33.0),
            text: format!(
                "Slide {} of {}  ·  {}",
                self.cur_slide + 1,
                self.deck.slides.len(),
                self.deck.palette.label()
            ),
            font: 11.5,
            color: LabelColor::Dim,
            weight: TextWeight::Regular,
        });

        // Tool group, centred-ish after the title block. Anchored past the
        // title (which shifts right when the hamburger is present).
        let mut bx = (title_x + 196.0).max(toolbar.x0 + 320.0_f64.min(toolbar.width() * 0.34));
        let by = toolbar.y0 + (TOOLBAR_H - 34.0) / 2.0;
        let bh = 34.0;
        let mut add_icontext = |bx: &mut f64, icon: Icon, label: &str, accent: bool, hit: Hit| {
            let w = 18.0 + 6.0 + chrome::est_width(label, 13.0) + 22.0;
            let rect = Rect::new(*bx, by, *bx + w, by + bh);
            buttons.push(UiButton {
                rect,
                kind: ButtonKind::IconText {
                    icon,
                    label: label.to_string(),
                    accent,
                },
                hit,
            });
            *bx += w + 8.0;
        };
        add_icontext(&mut bx, Icon::Plus, "Slide", false, Hit::NewSlide);
        add_icontext(&mut bx, Icon::FileText, "Text", false, Hit::Insert(ElementKind::Text));
        let mut add_text = |bx: &mut f64, label: &str, hit: Hit| {
            let w = chrome::est_width(label, 13.0) + 26.0;
            let rect = Rect::new(*bx, by, *bx + w, by + bh);
            buttons.push(UiButton {
                rect,
                kind: ButtonKind::Text {
                    label: label.to_string(),
                    active: false,
                    danger: false,
                },
                hit,
            });
            *bx += w + 8.0;
        };
        add_text(&mut bx, "Rect", Hit::Insert(ElementKind::Rect));
        add_text(&mut bx, "Ellipse", Hit::Insert(ElementKind::Ellipse));
        add_text(&mut bx, "Line", Hit::Insert(ElementKind::Line));
        add_text(&mut bx, "Image", Hit::Insert(ElementKind::Image));

        // Right-anchored: palette cycle + Present.
        let present_w = 18.0 + 6.0 + chrome::est_width("Present", 13.0) + 22.0;
        let present_rect = Rect::new(
            toolbar.x1 - 18.0 - present_w,
            by,
            toolbar.x1 - 18.0,
            by + bh,
        );
        buttons.push(UiButton {
            rect: present_rect,
            kind: ButtonKind::IconText {
                icon: Icon::Monitor,
                label: "Present".into(),
                accent: true,
            },
            hit: Hit::Present,
        });
        let pal_w = 30.0 + chrome::est_width(self.deck.palette.label(), 12.0) + 22.0;
        let pal_rect = Rect::new(
            present_rect.x0 - 10.0 - pal_w,
            by,
            present_rect.x0 - 10.0,
            by + bh,
        );
        buttons.push(UiButton {
            rect: pal_rect,
            kind: ButtonKind::Palette {
                kind: self.deck.palette,
                selected: false,
            },
            hit: Hit::CyclePalette,
        });
        // Overview (slide-sorter) toggle, left of the palette pill.
        let grid_rect = Rect::new(pal_rect.x0 - 10.0 - bh, by, pal_rect.x0 - 10.0, by + bh);
        buttons.push(UiButton {
            rect: grid_rect,
            kind: ButtonKind::IconOnly {
                icon: Icon::LayoutGrid,
                active: self.mode == Mode::Overview,
            },
            hit: Hit::Overview,
        });

        // Thumbnail rail ----------------------------------------------------
        let tpad = 14.0;
        let tw = rail.width() - 2.0 * tpad - 22.0; // leave a number gutter
        let th = tw * SLIDE_H / SLIDE_W;
        let gap = 12.0;
        let mut ty = rail.y0 + tpad + 4.0;
        let mut thumbs = Vec::new();
        labels.push(Label {
            pos: Point::new(rail.x0 + tpad, rail.y0 + 6.0),
            text: "SLIDES".into(),
            font: 11.0,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
        });
        ty += 16.0;
        for (i, _s) in self.deck.slides.iter().enumerate() {
            let tx = rail.x0 + tpad + 20.0;
            let rect = Rect::new(tx, ty, tx + tw, ty + th);
            thumbs.push((i, rect));
            labels.push(Label {
                pos: Point::new(rail.x0 + tpad - 2.0, ty + th / 2.0 - 8.0),
                text: format!("{}", i + 1),
                font: 12.0,
                color: if i == self.cur_slide {
                    LabelColor::Accent
                } else {
                    LabelColor::Dim
                },
                weight: TextWeight::Semibold,
            });
            // Reorder chevrons on the selected thumbnail.
            if i == self.cur_slide {
                let cw = 22.0;
                let up = Rect::new(rect.x1 - cw, rect.y0 + 4.0, rect.x1 - 2.0, rect.y0 + 4.0 + cw);
                let dn = Rect::new(rect.x1 - cw, rect.y1 - 4.0 - cw, rect.x1 - 2.0, rect.y1 - 4.0);
                if i > 0 {
                    buttons.push(UiButton {
                        rect: up,
                        kind: ButtonKind::IconOnly { icon: Icon::ChevronUp, active: false },
                        hit: Hit::MoveSlideUp,
                    });
                }
                if i + 1 < self.deck.slides.len() {
                    buttons.push(UiButton {
                        rect: dn,
                        kind: ButtonKind::IconOnly { icon: Icon::ChevronDown, active: false },
                        hit: Hit::MoveSlideDown,
                    });
                }
            }
            ty += th + gap;
        }
        // Rail action buttons at the bottom.
        let rb_h = 30.0;
        let rb_y = rail.y1 - tpad - rb_h;
        let rb_w = (rail.width() - 2.0 * tpad - 2.0 * 8.0) / 3.0;
        let mk = |idx: f64, label: &str, danger: bool, hit: Hit| -> UiButton {
            let x = rail.x0 + tpad + idx * (rb_w + 8.0);
            UiButton {
                rect: Rect::new(x, rb_y, x + rb_w, rb_y + rb_h),
                kind: ButtonKind::Text {
                    label: label.to_string(),
                    active: false,
                    danger,
                },
                hit,
            }
        };
        buttons.push(mk(0.0, "Add", false, Hit::NewSlide));
        buttons.push(mk(1.0, "Dup", false, Hit::DuplicateSlide));
        buttons.push(mk(2.0, "Del", true, Hit::DeleteSlide));

        // Inspector ---------------------------------------------------------
        let notes_box = self.build_inspector(inspector, &mut buttons, &mut labels);

        Chrome {
            toolbar,
            rail,
            inspector,
            slide_dest,
            scale: s,
            thumbs,
            buttons,
            labels,
            notes_box,
            layout,
            scrim,
            reveal,
        }
    }

    /// Lay out the inspector contents; returns the speaker-notes box rect.
    fn build_inspector(
        &self,
        inspector: Rect,
        buttons: &mut Vec<UiButton>,
        labels: &mut Vec<Label>,
    ) -> Option<Rect> {
        let pad = 16.0;
        let x0 = inspector.x0 + pad;
        let x1 = inspector.x1 - pad;
        let inner_w = x1 - x0;
        let mut y = inspector.y0 + pad;

        let header = |labels: &mut Vec<Label>, y: &mut f64, t: &str| {
            labels.push(Label {
                pos: Point::new(x0, *y),
                text: t.to_string(),
                font: 11.0,
                color: LabelColor::Dim,
                weight: TextWeight::Semibold,
            });
            *y += 20.0;
        };

        if let Some(el) = self.selected_element() {
            labels.push(Label {
                pos: Point::new(x0, y),
                text: el.kind.label().to_uppercase(),
                font: 16.0,
                color: LabelColor::Primary,
                weight: TextWeight::Bold,
            });
            y += 30.0;

            // Fill swatches.
            header(labels, &mut y, "FILL");
            y = self.swatch_grid(buttons, x0, inner_w, y, &FILL_ROLES, &el.fill, true);
            y += 10.0;

            // Stroke swatches.
            header(labels, &mut y, "STROKE");
            y = self.swatch_grid(buttons, x0, inner_w, y, &STROKE_ROLES, &el.stroke, false);
            y += 10.0;

            if el.kind == ElementKind::Text {
                header(labels, &mut y, "TYPE");
                let bh = 30.0;
                let minus = Rect::new(x0, y, x0 + 40.0, y + bh);
                let plus = Rect::new(x0 + 46.0, y, x0 + 86.0, y + bh);
                buttons.push(UiButton {
                    rect: minus,
                    kind: ButtonKind::Text { label: "A−".into(), active: false, danger: false },
                    hit: Hit::FontDelta(-4.0),
                });
                buttons.push(UiButton {
                    rect: plus,
                    kind: ButtonKind::Text { label: "A+".into(), active: false, danger: false },
                    hit: Hit::FontDelta(4.0),
                });
                let bold = Rect::new(x1 - 36.0, y, x1, y + bh);
                buttons.push(UiButton {
                    rect: bold,
                    kind: ButtonKind::Text { label: "B".into(), active: el.bold, danger: false },
                    hit: Hit::ToggleBold,
                });
                let italic = Rect::new(x1 - 36.0 - 6.0 - 36.0, y, x1 - 36.0 - 6.0, y + bh);
                buttons.push(UiButton {
                    rect: italic,
                    kind: ButtonKind::Text { label: "I".into(), active: el.italic, danger: false },
                    hit: Hit::ToggleItalic,
                });
                labels.push(Label {
                    pos: Point::new(x0 + 96.0, y + 8.0),
                    text: format!("{} pt", el.font_size as i64),
                    font: 13.0,
                    color: LabelColor::Primary,
                    weight: TextWeight::Regular,
                });
                y += bh + 8.0;
                let align = Rect::new(x0, y, x1, y + bh);
                buttons.push(UiButton {
                    rect: align,
                    kind: ButtonKind::Text {
                        label: format!("Align  ·  {}", el.align.glyph()),
                        active: false,
                        danger: false,
                    },
                    hit: Hit::CycleAlign,
                });
                y += bh + 14.0;
            }

            header(labels, &mut y, "ARRANGE");
            let bh = 30.0;
            let half = (inner_w - 8.0) / 2.0;
            let front = Rect::new(x0, y, x0 + half, y + bh);
            let back = Rect::new(x0 + half + 8.0, y, x1, y + bh);
            buttons.push(UiButton {
                rect: front,
                kind: ButtonKind::Text { label: "Bring front".into(), active: false, danger: false },
                hit: Hit::BringFront,
            });
            buttons.push(UiButton {
                rect: back,
                kind: ButtonKind::Text { label: "Send back".into(), active: false, danger: false },
                hit: Hit::SendBack,
            });
            y += bh + 8.0;
            let dup = Rect::new(x0, y, x0 + half, y + bh);
            let del = Rect::new(x0 + half + 8.0, y, x1, y + bh);
            buttons.push(UiButton {
                rect: dup,
                kind: ButtonKind::Text { label: "Duplicate".into(), active: false, danger: false },
                hit: Hit::DuplicateElement,
            });
            buttons.push(UiButton {
                rect: del,
                kind: ButtonKind::Text { label: "Delete".into(), active: false, danger: true },
                hit: Hit::DeleteElement,
            });
            y += bh + 6.0;
        } else {
            labels.push(Label {
                pos: Point::new(x0, y),
                text: format!("SLIDE {}", self.cur_slide + 1),
                font: 16.0,
                color: LabelColor::Primary,
                weight: TextWeight::Bold,
            });
            y += 30.0;

            header(labels, &mut y, "LAYOUT");
            let cur_layout = self.slide().map(|s| s.layout).unwrap_or(Layout::TitleContent);
            y = self.wrap_pills(
                buttons,
                x0,
                x1,
                y,
                Layout::ALL.iter().map(|l| {
                    (l.label().to_string(), *l == cur_layout, Hit::SetLayout(*l))
                }),
            );
            y += 10.0;

            header(labels, &mut y, "BACKGROUND");
            let cur_bg = self.slide().map(|s| s.background).unwrap_or(Background::Surface);
            y = self.wrap_pills(
                buttons,
                x0,
                x1,
                y,
                Background::ALL.iter().map(|b| {
                    (b.label().to_string(), *b == cur_bg, Hit::SetBackground(*b))
                }),
            );
            y += 10.0;

            header(labels, &mut y, "DECK THEME");
            let sw = (inner_w - 4.0 * 8.0) / 5.0;
            for (i, p) in PaletteKind::ALL.iter().enumerate() {
                let x = x0 + i as f64 * (sw + 8.0);
                buttons.push(UiButton {
                    rect: Rect::new(x, y, x + sw, y + 28.0),
                    kind: ButtonKind::Palette { kind: *p, selected: *p == self.deck.palette },
                    hit: Hit::SetPalette(*p),
                });
            }
            y += 28.0 + 10.0;
        }

        // Speaker notes (always shown at the bottom).
        let notes_top = (y + 6.0).max(inspector.y1 - 188.0);
        labels.push(Label {
            pos: Point::new(x0, notes_top),
            text: if self.editing_notes { "SPEAKER NOTES  (editing — Esc)" } else { "SPEAKER NOTES" }
                .to_string(),
            font: 11.0,
            color: LabelColor::Dim,
            weight: TextWeight::Semibold,
        });
        let nb = Rect::new(x0, notes_top + 20.0, x1, inspector.y1 - pad);
        buttons.push(UiButton {
            rect: nb,
            kind: ButtonKind::Text { label: String::new(), active: false, danger: false },
            hit: Hit::EditNotes,
        });
        Some(nb)
    }

    fn swatch_grid(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        inner_w: f64,
        mut y: f64,
        roles: &[&str],
        current: &str,
        is_fill: bool,
    ) -> f64 {
        let cols = 4.0;
        let gap = 8.0;
        let sw = (inner_w - (cols - 1.0) * gap) / cols;
        let sh = 26.0;
        for (i, role) in roles.iter().enumerate() {
            let col = (i as f64) % cols;
            let row = ((i as f64) / cols).floor();
            let x = x0 + col * (sw + gap);
            let yy = y + row * (sh + gap);
            buttons.push(UiButton {
                rect: Rect::new(x, yy, x + sw, yy + sh),
                kind: ButtonKind::Swatch {
                    role: role.to_string(),
                    selected: *role == current,
                },
                hit: if is_fill {
                    Hit::SetFill(role.to_string())
                } else {
                    Hit::SetStroke(role.to_string())
                },
            });
        }
        let rows = ((roles.len() as f64) / cols).ceil();
        y += rows * (sh + gap) - gap;
        y
    }

    fn wrap_pills(
        &self,
        buttons: &mut Vec<UiButton>,
        x0: f64,
        x1: f64,
        mut y: f64,
        items: impl Iterator<Item = (String, bool, Hit)>,
    ) -> f64 {
        let bh = 28.0;
        let gap = 8.0;
        let mut x = x0;
        for (label, active, hit) in items {
            let w = chrome::est_width(&label, 12.0) + 22.0;
            if x + w > x1 {
                x = x0;
                y += bh + gap;
            }
            buttons.push(UiButton {
                rect: Rect::new(x, y, x + w, y + bh),
                kind: ButtonKind::Text { label, active, danger: false },
                hit,
            });
            x += w + gap;
        }
        y + bh
    }

    // --- mouse → slide coords ----------------------------------------------

    fn to_slide(&self, dest: Rect, s: f64, p: Point) -> Point {
        Point::new((p.x - dest.x0) / s, (p.y - dest.y0) / s)
    }

    fn handles_of(&self, dest: Rect, s: f64, el: &Element) -> Vec<(Handle, Rect)> {
        let er = render::element_dest(dest, s, el);
        let cx = er.center().x;
        let cy = er.center().y;
        let pos = |h: Handle| -> Point {
            match h {
                Handle::Tl => Point::new(er.x0, er.y0),
                Handle::T => Point::new(cx, er.y0),
                Handle::Tr => Point::new(er.x1, er.y0),
                Handle::R => Point::new(er.x1, cy),
                Handle::Br => Point::new(er.x1, er.y1),
                Handle::B => Point::new(cx, er.y1),
                Handle::Bl => Point::new(er.x0, er.y1),
                Handle::L => Point::new(er.x0, cy),
            }
        };
        Handle::ALL
            .iter()
            .map(|h| {
                let p = pos(*h);
                (
                    *h,
                    Rect::new(p.x - HANDLE, p.y - HANDLE, p.x + HANDLE, p.y + HANDLE),
                )
            })
            .collect()
    }

    // --- actions ------------------------------------------------------------

    fn apply_hit(&mut self, doc: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::NewSlide => self.new_slide(doc),
            Hit::DuplicateSlide => self.duplicate_slide(doc),
            Hit::DeleteSlide => self.delete_slide(doc),
            Hit::Present => self.enter_present(),
            Hit::Overview => {
                self.mode = if self.mode == Mode::Overview { Mode::Edit } else { Mode::Overview };
                self.editing_text = false;
                self.editing_notes = false;
            }
            Hit::OpenSlide(i) => {
                self.cur_slide = i.min(self.deck.slides.len().saturating_sub(1));
                self.sel = None;
                self.mode = Mode::Edit;
            }
            Hit::CyclePalette => {
                self.deck.palette = self.deck.palette.next();
                SlidesDoc::set_palette(doc, self.deck.palette);
                self.mark();
            }
            Hit::Insert(kind) => self.insert_element(doc, kind),
            Hit::SelectSlide(i) => {
                self.cur_slide = i;
                self.sel = None;
                self.editing_text = false;
                self.editing_notes = false;
            }
            Hit::MoveSlideUp => {
                if self.cur_slide > 0 {
                    SlidesDoc::move_slide(doc, self.cur_slide, self.cur_slide - 1);
                    self.cur_slide -= 1;
                    self.mark();
                }
            }
            Hit::MoveSlideDown => {
                if self.cur_slide + 1 < self.deck.slides.len() {
                    SlidesDoc::move_slide(doc, self.cur_slide, self.cur_slide + 1);
                    self.cur_slide += 1;
                    self.mark();
                }
            }
            Hit::SetFill(role) => self.set_sel_str(doc, "fill", &role),
            Hit::SetStroke(role) => {
                self.set_sel_str(doc, "stroke", &role);
                // Give a freshly-stroked shape a sensible default width.
                if let Some(el) = self.selected_element() {
                    if role != "none" && el.stroke_width < 1.0 {
                        self.set_sel_f64(doc, "stroke_width", 4.0);
                    }
                }
            }
            Hit::FontDelta(d) => {
                if let Some(el) = self.selected_element() {
                    let v = (el.font_size + d).clamp(8.0, 160.0);
                    self.set_sel_f64(doc, "font_size", v);
                }
            }
            Hit::ToggleBold => {
                if let Some(el) = self.selected_element() {
                    let v = !el.bold;
                    self.set_sel_bool(doc, "bold", v);
                }
            }
            Hit::ToggleItalic => {
                if let Some(el) = self.selected_element() {
                    let v = !el.italic;
                    self.set_sel_bool(doc, "italic", v);
                }
            }
            Hit::CycleAlign => {
                if let Some(el) = self.selected_element() {
                    let v = el.align.next();
                    self.set_sel_str(doc, "align", v.as_str());
                }
            }
            Hit::BringFront => self.restack(doc, true),
            Hit::SendBack => self.restack(doc, false),
            Hit::DuplicateElement => self.duplicate_element(doc),
            Hit::DeleteElement => self.delete_element(doc),
            Hit::SetLayout(l) => {
                SlidesDoc::set_slide_layout(doc, self.cur_slide, l);
                self.mark();
            }
            Hit::SetBackground(b) => {
                SlidesDoc::set_slide_background(doc, self.cur_slide, b);
                self.mark();
            }
            Hit::SetPalette(p) => {
                self.deck.palette = p;
                SlidesDoc::set_palette(doc, p);
                self.mark();
            }
            Hit::EditNotes => {
                self.editing_notes = true;
                self.editing_text = false;
                self.notes_caret = self.slide().map(|s| s.notes.chars().count()).unwrap_or(0);
            }
            Hit::ToggleRail => self.rail_flap.toggle(self.rail_layout()),
        }
        self.reload(doc);
    }

    fn set_sel_str(&mut self, doc: &mut AutoCommit, key: &str, v: &str) {
        if let Some(id) = self.sel.clone() {
            SlidesDoc::set_element_str(doc, self.cur_slide, &id, key, v);
            self.mark();
        }
    }
    fn set_sel_f64(&mut self, doc: &mut AutoCommit, key: &str, v: f64) {
        if let Some(id) = self.sel.clone() {
            SlidesDoc::set_element_f64(doc, self.cur_slide, &id, key, v);
            self.mark();
        }
    }
    fn set_sel_bool(&mut self, doc: &mut AutoCommit, key: &str, v: bool) {
        if let Some(id) = self.sel.clone() {
            SlidesDoc::set_element_bool(doc, self.cur_slide, &id, key, v);
            self.mark();
        }
    }

    fn restack(&mut self, doc: &mut AutoCommit, front: bool) {
        let Some(id) = self.sel.clone() else { return };
        let Some(slide) = self.deck.slides.get(self.cur_slide) else { return };
        let z = if front {
            slide.elements.iter().map(|e| e.z).max().unwrap_or(0) + 1
        } else {
            slide.elements.iter().map(|e| e.z).min().unwrap_or(0) - 1
        };
        SlidesDoc::set_element_z(doc, self.cur_slide, &id, z);
        self.mark();
    }

    fn new_slide(&mut self, doc: &mut AutoCommit) {
        let mut s = Slide::new(Layout::TitleContent);
        s.add(Element::text("New slide", 96.0, 90.0, 1088.0, 100.0, 54.0).with(|e| e.bold = true));
        s.add(Element::shape(ElementKind::Rect, 96.0, 188.0, 150.0, 8.0).with(|e| {
            e.fill = "accent".into();
            e.radius = 4.0;
        }));
        s.add(Element::text("Add your content…", 96.0, 230.0, 900.0, 300.0, 32.0).with(|e| {
            e.fill = "text-dim".into();
        }));
        let at = self.cur_slide + 1;
        SlidesDoc::insert_slide(doc, at, &s);
        self.cur_slide = at;
        self.sel = None;
        self.editing_text = false;
        self.mark();
    }

    fn duplicate_slide(&mut self, doc: &mut AutoCommit) {
        let Some(slide) = self.deck.slides.get(self.cur_slide).cloned() else { return };
        let mut copy = slide;
        copy.id = new_id();
        for e in &mut copy.elements {
            e.id = new_id();
        }
        let at = self.cur_slide + 1;
        SlidesDoc::insert_slide(doc, at, &copy);
        self.cur_slide = at;
        self.sel = None;
        self.mark();
    }

    fn delete_slide(&mut self, doc: &mut AutoCommit) {
        if self.deck.slides.len() <= 1 {
            return; // keep at least one slide
        }
        SlidesDoc::remove_slide(doc, self.cur_slide);
        if self.cur_slide > 0 {
            self.cur_slide -= 1;
        }
        self.sel = None;
        self.mark();
    }

    fn insert_element(&mut self, doc: &mut AutoCommit, kind: ElementKind) {
        let el = match kind {
            ElementKind::Text => Element::text("Text", 440.0, 320.0, 400.0, 90.0, 40.0),
            ElementKind::Line => Element::shape(ElementKind::Line, 440.0, 360.0, 360.0, 0.0),
            ElementKind::Image => Element::shape(ElementKind::Image, 440.0, 220.0, 400.0, 280.0),
            other => Element::shape(other, 480.0, 260.0, 320.0, 220.0),
        };
        let id = el.id.clone();
        SlidesDoc::add_element(doc, self.cur_slide, &el);
        self.sel = Some(id);
        self.editing_text = false;
        self.mark();
    }

    fn duplicate_element(&mut self, doc: &mut AutoCommit) {
        let Some(el) = self.selected_element().cloned() else { return };
        let mut copy = el;
        copy.id = new_id();
        copy.x += 24.0;
        copy.y += 24.0;
        let id = copy.id.clone();
        SlidesDoc::add_element(doc, self.cur_slide, &copy);
        self.sel = Some(id);
        self.mark();
    }

    fn delete_element(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.sel.clone() {
            SlidesDoc::remove_element(doc, self.cur_slide, &id);
            self.sel = None;
            self.editing_text = false;
            self.mark();
        }
    }

    fn nudge(&mut self, doc: &mut AutoCommit, dx: f64, dy: f64) {
        if let Some(el) = self.selected_element().cloned() {
            let nx = (el.x + dx).clamp(-el.w + 20.0, SLIDE_W - 20.0);
            let ny = (el.y + dy).clamp(-el.h + 20.0, SLIDE_H - 20.0);
            SlidesDoc::set_element_pos(doc, self.cur_slide, &el.id, nx, ny);
            self.mark();
        }
    }

    fn next_slide(&mut self, delta: isize) {
        let n = self.deck.slides.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.cur_slide as isize + delta;
        i = i.clamp(0, n - 1);
        let from = self.cur_slide;
        if i as usize != from {
            // Cross-fade only in present mode; edit/overview jumps are instant.
            if self.mode == Mode::Present {
                self.transition = Some((from, std::time::Instant::now()));
                self.blackout = false;
            }
            self.cur_slide = i as usize;
        }
        self.sel = None;
        self.editing_text = false;
    }

    /// Enter full-screen present mode, (re)starting the speaker timer.
    fn enter_present(&mut self) {
        self.mode = Mode::Present;
        self.editing_text = false;
        self.editing_notes = false;
        self.blackout = false;
        self.transition = None;
        self.present_started = Some(std::time::Instant::now());
    }

    /// Elapsed presentation time, formatted `m:ss`.
    fn present_clock(&self) -> String {
        let secs = self
            .present_started
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// Eased progress 0..1 of the active transition (or 1.0 when settled).
    fn transition_alpha(&self) -> f32 {
        match self.transition {
            Some((_, t)) => {
                let e = t.elapsed().as_millis().min(TRANSITION_MS);
                let p = e as f32 / TRANSITION_MS as f32;
                // smoothstep
                (p * p * (3.0 - 2.0 * p)).clamp(0.0, 1.0)
            }
            None => 1.0,
        }
    }

    // --- text editing -------------------------------------------------------

    fn insert_text(&mut self, doc: &mut AutoCommit, s: &str) {
        if self.editing_notes {
            SlidesDoc::splice_notes(doc, self.cur_slide, self.notes_caret, 0, s);
            self.notes_caret += s.chars().count();
            self.reload(doc);
            return;
        }
        if let Some(id) = self.sel.clone() {
            SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret, 0, s);
            self.caret += s.chars().count();
            self.reload(doc);
        }
    }

    fn backspace_text(&mut self, doc: &mut AutoCommit) {
        if self.editing_notes {
            if self.notes_caret > 0 {
                SlidesDoc::splice_notes(doc, self.cur_slide, self.notes_caret - 1, 1, "");
                self.notes_caret -= 1;
                self.mark();
                self.reload(doc);
            }
            return;
        }
        if let Some(id) = self.sel.clone() {
            if self.caret > 0 {
                SlidesDoc::splice_element_text(doc, self.cur_slide, &id, self.caret - 1, 1, "");
                self.caret -= 1;
                self.mark();
                self.reload(doc);
            }
        }
    }

    fn cur_text_len(&self) -> usize {
        if self.editing_notes {
            self.slide().map(|s| s.notes.chars().count()).unwrap_or(0)
        } else {
            self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0)
        }
    }

    // --- drawing ------------------------------------------------------------

    fn draw_editor(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let pal = self.palette(theme);
        let c = &theme.tokens.colors;
        let overlay = chrome.layout == SplitLayout::Overlay;

        // Panels.
        chrome::fill_rect(scene, t, self.rect, c.surfaces.base);
        // The rail column is painted with the body content below (so an open
        // overlay flap can slide over a scrim). Docked: it's part of the base.
        if !overlay {
            chrome::fill_rect(scene, t, chrome.rail, c.surfaces.raised);
        }
        chrome::fill_rect(scene, t, chrome.inspector, c.surfaces.raised);
        chrome::fill_rect(scene, t, chrome.toolbar, mix_panel(c.surfaces.raised, c.surfaces.base));
        // Divider lines.
        let div = c.border_standard;
        stroke_line(scene, t, chrome.toolbar.x0, chrome.toolbar.y1, chrome.toolbar.x1, chrome.toolbar.y1, div);
        if !overlay {
            stroke_line(scene, t, chrome.rail.x1, chrome.rail.y0, chrome.rail.x1, chrome.rail.y1, div);
        }
        stroke_line(scene, t, chrome.inspector.x0, chrome.inspector.y0, chrome.inspector.x0, chrome.inspector.y1, div);

        // Canvas slide.
        if let Some(slide) = self.slide() {
            // Soft drop shadow.
            let sh = chrome.slide_dest + vello::kurbo::Vec2::new(0.0, 6.0);
            chrome::fill_rrect(scene, t, sh.inflate(2.0, 2.0), 14.0, render::with_alpha(Color::BLACK, 70));
            render::draw_slide(scene, t, tr, theme, &pal, slide, chrome.slide_dest, 14.0);
            chrome::stroke_rrect(scene, t, chrome.slide_dest, 14.0, render::with_alpha(c.text_primary, 28), 1.0);

            // Selection chrome.
            if let Some(el) = self.selected_element() {
                let er = render::element_dest(chrome.slide_dest, chrome.scale, el);
                let sel_col = pal.accent;
                chrome::stroke_rrect(scene, t, er.inflate(3.0, 3.0), 4.0, sel_col, 1.5);
                if !self.editing_text {
                    for (_, hr) in self.handles_of(chrome.slide_dest, chrome.scale, el) {
                        chrome::fill_rrect(scene, t, hr, 2.5, Color::WHITE);
                        chrome::stroke_rrect(scene, t, hr, 2.5, sel_col, 1.5);
                    }
                } else {
                    self.draw_text_caret(scene, t, tr, &pal, el, er, chrome.scale);
                }
            }

            // Live alignment guides (only while a drag is snapping).
            for g in &self.active_guides {
                let gc = render::with_alpha(pal.accent, 220);
                match g {
                    Guide::Vertical(sx) => {
                        let x = chrome.slide_dest.x0 + sx * chrome.scale;
                        stroke_line(scene, t, x, chrome.slide_dest.y0, x, chrome.slide_dest.y1, gc);
                    }
                    Guide::Horizontal(sy) => {
                        let y = chrome.slide_dest.y0 + sy * chrome.scale;
                        stroke_line(scene, t, chrome.slide_dest.x0, y, chrome.slide_dest.x1, y, gc);
                    }
                }
            }

            // Caption under the slide.
            let cap = format!("{}  ·  {}", slide.layout.label(), slide.background.label());
            tr.draw_text_weighted(
                scene,
                &cap,
                12.0,
                c.text_dim,
                t * Affine::translate((chrome.slide_dest.x0, chrome.slide_dest.y1 + 10.0)),
                None,
                TextWeight::Regular,
            );
        }

        // The rail (thumbnails + its buttons/labels) is content that lives
        // inside the rail column. In overlay mode it must paint on top of the
        // dimming scrim, so the whole rail group is deferred to a final pass.
        // Membership is geometric: below the toolbar and within the rail's
        // (possibly off-screen / sliding) x-span.
        let in_rail = |center: Point| -> bool {
            center.y >= chrome.rail.y0
                && center.x >= chrome.rail.x0 - 0.5
                && center.x <= chrome.rail.x1 + 0.5
        };

        // Non-rail buttons (toolbar + inspector) in the base layer.
        for b in &chrome.buttons {
            if !in_rail(b.rect.center()) {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
        // Notes content over its box (inspector — base layer).
        if let (Some(nb), Some(slide)) = (chrome.notes_box, self.slide()) {
            self.draw_notes(scene, t, tr, theme, nb, &slide.notes);
        }
        // Non-rail labels.
        for l in &chrome.labels {
            if !in_rail(Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, &pal, l);
            }
        }

        // Scrim behind an open overlay flap, dimming the editor body.
        if overlay && chrome.scrim.width() > 0.0 {
            let alpha = (chrome.reveal * 0.5) as f32;
            chrome::fill_rect(scene, t, chrome.scrim, render::with_alpha(Color::BLACK, (alpha * 255.0) as u8));
        }

        // Rail group on top: panel fill (overlay only), thumbnails, then the
        // rail's own buttons + labels.
        if overlay && chrome.reveal > 0.001 {
            chrome::fill_rect(scene, t, chrome.rail, c.surfaces.raised);
            stroke_line(scene, t, chrome.rail.x1, chrome.rail.y0, chrome.rail.x1, chrome.rail.y1, div);
        }
        for (i, rect) in &chrome.thumbs {
            if let Some(slide) = self.deck.slides.get(*i) {
                render::draw_slide(scene, t, tr, theme, &pal, slide, *rect, 6.0);
                let ring = if *i == self.cur_slide { pal.accent } else { render::with_alpha(c.text_primary, 26) };
                let w = if *i == self.cur_slide { 2.5 } else { 1.0 };
                chrome::stroke_rrect(scene, t, *rect, 6.0, ring, w);
            }
        }
        for b in &chrome.buttons {
            if in_rail(b.rect.center()) {
                self.draw_button(scene, t, tr, theme, &pal, b);
            }
        }
        for l in &chrome.labels {
            if in_rail(Point::new(l.pos.x, l.pos.y)) {
                self.draw_label(scene, t, tr, theme, &pal, l);
            }
        }
    }

    fn draw_label(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        l: &Label,
    ) {
        let col = self.label_color(l.color, theme, pal);
        tr.draw_text_weighted(
            scene,
            &l.text,
            l.font,
            col,
            t * Affine::translate((l.pos.x, l.pos.y)),
            None,
            l.weight,
        );
    }

    fn label_color(&self, lc: LabelColor, theme: &Theme, pal: &Palette) -> Color {
        match lc {
            LabelColor::Primary => theme.tokens.colors.text_primary,
            LabelColor::Dim => theme.tokens.colors.text_dim,
            LabelColor::Accent => pal.accent,
        }
    }

    fn draw_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pal: &Palette,
        b: &UiButton,
    ) {
        let c = &theme.tokens.colors;
        match &b.kind {
            ButtonKind::Text { label, active, danger } => {
                let (bg, fg) = if *danger {
                    (render::with_alpha(c.semantic.danger, 38), c.semantic.danger)
                } else if *active {
                    (pal.accent, Color::WHITE)
                } else {
                    (c.surfaces.base, c.text_primary)
                };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 26), 1.0);
                if !label.is_empty() {
                    chrome::text_centered(scene, t, tr, label, b.rect, 13.0, fg, TextWeight::Semibold);
                }
            }
            ButtonKind::IconText { icon, label, accent } => {
                let (bg, fg) = if *accent {
                    (pal.accent, Color::WHITE)
                } else {
                    (c.surfaces.base, c.text_primary)
                };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                if !accent {
                    chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 26), 1.0);
                }
                let ir = Rect::new(b.rect.x0 + 11.0, b.rect.y0 + 8.0, b.rect.x0 + 29.0, b.rect.y1 - 8.0);
                draw_icon(scene, t, ir, *icon, fg, 1.8);
                chrome::text_vcenter(scene, t, tr, label, b.rect, ir.x1 + 5.0, 13.0, fg, TextWeight::Semibold);
            }
            ButtonKind::IconOnly { icon, active } => {
                let bg = if *active { pal.accent } else { c.surfaces.base };
                chrome::fill_rrect(scene, t, b.rect, 7.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 7.0, render::with_alpha(c.text_primary, 30), 1.0);
                let ir = b.rect.inflate(-6.0, -6.0);
                draw_icon(scene, t, ir, *icon, c.text_primary, 1.8);
            }
            ButtonKind::Swatch { role, selected } => {
                match pal.resolve_role(role) {
                    Some(col) => {
                        chrome::fill_rrect(scene, t, b.rect, 6.0, col);
                        // contrast border for light swatches
                        chrome::stroke_rrect(scene, t, b.rect, 6.0, render::with_alpha(Color::BLACK, 50), 1.0);
                    }
                    None => {
                        // "none" → hatched empty box
                        chrome::fill_rrect(scene, t, b.rect, 6.0, c.surfaces.base);
                        chrome::stroke_rrect(scene, t, b.rect, 6.0, render::with_alpha(c.text_primary, 60), 1.0);
                        stroke_line(scene, t, b.rect.x0 + 4.0, b.rect.y1 - 4.0, b.rect.x1 - 4.0, b.rect.y0 + 4.0, c.semantic.danger);
                    }
                }
                if *selected {
                    chrome::stroke_rrect(scene, t, b.rect.inflate(2.5, 2.5), 8.0, pal.accent, 2.0);
                }
            }
            ButtonKind::Palette { kind, selected } => {
                let col = Palette::resolve(*kind, theme).accent;
                chrome::fill_rrect(scene, t, b.rect, 8.0, c.surfaces.base);
                chrome::stroke_rrect(
                    scene,
                    t,
                    b.rect,
                    8.0,
                    if *selected { col } else { render::with_alpha(c.text_primary, 26) },
                    if *selected { 2.0 } else { 1.0 },
                );
                let dot = Rect::new(b.rect.x0 + 8.0, b.rect.center().y - 6.0, b.rect.x0 + 20.0, b.rect.center().y + 6.0);
                chrome::fill_rrect(scene, t, dot, 6.0, col);
                if b.rect.width() > 40.0 {
                    chrome::text_vcenter(scene, t, tr, kind.label(), b.rect, dot.x1 + 6.0, 12.0, c.text_primary, TextWeight::Semibold);
                }
            }
            ButtonKind::Hamburger => {
                let active = self.rail_open();
                let bg = if active { pal.accent } else { c.surfaces.base };
                let fg = if active { Color::WHITE } else { c.text_primary };
                chrome::fill_rrect(scene, t, b.rect, 8.0, bg);
                chrome::stroke_rrect(scene, t, b.rect, 8.0, render::with_alpha(c.text_primary, 30), 1.0);
                // Three evenly-spaced horizontal lines.
                let cx0 = b.rect.x0 + 9.0;
                let cx1 = b.rect.x1 - 9.0;
                let cy = b.rect.center().y;
                for dy in [-5.0, 0.0, 5.0] {
                    stroke_line(scene, t, cx0, cy + dy, cx1, cy + dy, fg);
                }
            }
        }
    }

    fn draw_text_caret(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        _pal: &Palette,
        el: &Element,
        er: Rect,
        s: f64,
    ) {
        // Approximate caret position: count chars up to caret on the caret's
        // line, measure that prefix, place a vertical bar.
        let text = &el.text;
        let caret = self.caret.min(text.chars().count());
        let chars: Vec<char> = text.chars().collect();
        let mut line_start = 0usize;
        let mut line_idx = 0usize;
        for i in 0..caret {
            if chars[i] == '\n' {
                line_start = i + 1;
                line_idx += 1;
            }
        }
        let prefix: String = chars[line_start..caret].iter().collect();
        let font_px = (el.font_size * s).max(6.0) as f32;
        let weight = if el.bold { TextWeight::Bold } else { TextWeight::Regular };
        let (pw, _) = tr.measure_styled(&prefix, font_px, None, weight, el.italic, false);
        let line_h = (font_px as f64) * 1.28;
        let n_lines = text.split('\n').count().max(1);
        let total_h = line_h * n_lines as f64;
        let top = if total_h < er.height() {
            er.y0 + (er.height() - total_h) / 2.0
        } else {
            er.y0
        };
        let cy = top + line_idx as f64 * line_h;
        let cx = match el.align {
            TextAlign::Left => er.x0 + pw as f64,
            _ => er.x0 + pw as f64, // simple: left-anchored caret
        };
        let bar = Line::new(Point::new(cx, cy + 2.0), Point::new(cx, cy + line_h - 2.0));
        scene.stroke(&Stroke::new(2.0), t, _pal.accent, None, &bar);
    }

    fn draw_notes(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        nb: Rect,
        notes: &str,
    ) {
        let c = &theme.tokens.colors;
        let bg = if self.editing_notes {
            render::with_alpha(self.palette(theme).accent, 28)
        } else {
            c.surfaces.base
        };
        chrome::fill_rrect(scene, t, nb, 8.0, bg);
        chrome::stroke_rrect(
            scene,
            t,
            nb,
            8.0,
            if self.editing_notes { self.palette(theme).accent } else { render::with_alpha(c.text_primary, 26) },
            1.0,
        );
        let display = if notes.is_empty() {
            "Click to add speaker notes…".to_string()
        } else {
            notes.to_string()
        };
        let col = if notes.is_empty() { c.text_dim } else { c.text_primary };
        tr.draw_text_weighted(
            scene,
            &display,
            12.5,
            col,
            t * Affine::translate((nb.x0 + 10.0, nb.y0 + 9.0)),
            Some((nb.width() - 20.0) as f32),
            TextWeight::Regular,
        );
    }

    fn draw_present(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pal = self.palette(theme);
        // Black backdrop.
        chrome::fill_rect(scene, t, self.rect, Color::BLACK);

        // Black-out screen: nothing but a faint resume hint.
        if self.blackout {
            tr.draw_text_weighted(
                scene,
                "Screen blacked out  ·  press B to resume",
                14.0,
                render::with_alpha(Color::WHITE, 120),
                t * Affine::translate((self.rect.x0 + 40.0, self.rect.y1 - 40.0)),
                None,
                TextWeight::Regular,
            );
            return;
        }

        // Letterbox the current slide.
        let (area, notes_panel) = if self.show_notes {
            let split = self.rect.y0 + self.rect.height() * 0.66;
            (
                Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, split),
                Some(Rect::new(self.rect.x0, split, self.rect.x1, self.rect.y1)),
            )
        } else {
            (self.rect, None)
        };
        let pad = 24.0;
        let aw = area.width() - 2.0 * pad;
        let ah = area.height() - 2.0 * pad;
        let s = (aw / SLIDE_W).min(ah / SLIDE_H);
        let sw = SLIDE_W * s;
        let sh = SLIDE_H * s;
        let sx = area.x0 + (area.width() - sw) / 2.0;
        let sy = area.y0 + (area.height() - sh) / 2.0;
        let dest = Rect::new(sx, sy, sx + sw, sy + sh);
        // Cross-fade: paint the outgoing slide first, then the incoming
        // one inside an alpha layer that ramps in.
        let alpha = self.transition_alpha();
        if let Some((from, _)) = self.transition {
            if alpha < 1.0 {
                if let Some(prev) = self.deck.slides.get(from) {
                    render::draw_slide(scene, t, tr, theme, &pal, prev, dest, 0.0);
                }
            }
        }
        if let Some(slide) = self.slide() {
            if alpha < 1.0 {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    t,
                    &dest,
                );
                render::draw_slide(scene, t, tr, theme, &pal, slide, dest, 0.0);
                scene.pop_layer();
            } else {
                render::draw_slide(scene, t, tr, theme, &pal, slide, dest, 0.0);
            }
        }

        // Progress bar pinned to the very bottom edge of the screen.
        let n = self.deck.slides.len().max(1);
        let frac = (self.cur_slide + 1) as f64 / n as f64;
        let bar_h = 4.0;
        let track = Rect::new(self.rect.x0, self.rect.y1 - bar_h, self.rect.x1, self.rect.y1);
        chrome::fill_rect(scene, t, track, render::with_alpha(Color::WHITE, 30));
        let fill = Rect::new(track.x0, track.y0, track.x0 + track.width() * frac, track.y1);
        chrome::fill_rect(scene, t, fill, pal.accent);

        // Elapsed-time pill, top-right.
        let clock = self.present_clock();
        let tw = chrome::est_width(&clock, 13.0) + 26.0;
        let tr_rect = Rect::new(self.rect.x1 - 20.0 - tw, self.rect.y0 + 16.0, self.rect.x1 - 20.0, self.rect.y0 + 16.0 + 28.0);
        chrome::fill_rrect(scene, t, tr_rect, 14.0, render::with_alpha(Color::WHITE, 24));
        chrome::text_centered(scene, t, tr, &clock, tr_rect, 13.0, Color::WHITE, TextWeight::Semibold);

        // Slide counter pill + hint, in the gutter below the slide — only
        // when there's clear room (suppressed when the notes panel is up).
        if dest.y1 + 40.0 < area.y1 {
            let counter = format!("{} / {}", self.cur_slide + 1, self.deck.slides.len());
            let cw = chrome::est_width(&counter, 13.0) + 24.0;
            let cr = Rect::new(dest.x1 - cw, dest.y1 + 8.0, dest.x1, dest.y1 + 8.0 + 26.0);
            chrome::fill_rrect(scene, t, cr, 13.0, render::with_alpha(Color::WHITE, 26));
            chrome::text_centered(scene, t, tr, &counter, cr, 13.0, Color::WHITE, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                "← →  navigate    S  notes    B  black    Esc  exit",
                12.5,
                render::with_alpha(Color::WHITE, 150),
                t * Affine::translate((dest.x0, dest.y1 + 14.0)),
                None,
                TextWeight::Regular,
            );
        }

        // Speaker-notes panel.
        if let Some(np) = notes_panel {
            chrome::fill_rect(scene, t, np, theme.tokens.colors.surfaces.raised);
            stroke_line(scene, t, np.x0, np.y0, np.x1, np.y0, theme.tokens.colors.border_standard);
            let nx = np.x0 + 40.0;
            tr.draw_text_weighted(
                scene,
                "SPEAKER NOTES",
                12.0,
                pal.accent,
                t * Affine::translate((nx, np.y0 + 20.0)),
                None,
                TextWeight::Semibold,
            );
            // The NEXT preview occupies the right third; reserve its left edge.
            let pw = np.width() * 0.26;
            let preview_x0 = np.x1 - 40.0 - pw;
            // Big elapsed clock, right-aligned just left of the preview.
            let clock = self.present_clock();
            let (clock_w, _) = tr.measure_weighted(&clock, 30.0, None, TextWeight::Bold);
            tr.draw_text_weighted(
                scene,
                &clock,
                30.0,
                theme.tokens.colors.text_primary,
                t * Affine::translate((preview_x0 - 36.0 - clock_w as f64, np.y0 + 16.0)),
                None,
                TextWeight::Bold,
            );
            let notes = self.slide().map(|s| s.notes.clone()).unwrap_or_default();
            let body = if notes.is_empty() { "—".to_string() } else { notes };
            tr.draw_text_weighted(
                scene,
                &body,
                18.0,
                theme.tokens.colors.text_primary,
                t * Affine::translate((nx, np.y0 + 44.0)),
                Some((np.width() * 0.6) as f32),
                TextWeight::Regular,
            );
            // Next slide preview.
            if let Some(next) = self.deck.slides.get(self.cur_slide + 1) {
                let ph = pw * SLIDE_H / SLIDE_W;
                let pr = Rect::new(preview_x0, np.y0 + 28.0, np.x1 - 40.0, np.y0 + 28.0 + ph);
                tr.draw_text_weighted(
                    scene,
                    "NEXT",
                    11.0,
                    theme.tokens.colors.text_dim,
                    t * Affine::translate((pr.x0, pr.y0 - 18.0)),
                    None,
                    TextWeight::Semibold,
                );
                render::draw_slide(scene, t, tr, theme, &pal, next, pr, 6.0);
                chrome::stroke_rrect(scene, t, pr, 6.0, render::with_alpha(Color::WHITE, 40), 1.0);
            }
        }
    }

    // --- overview (slide-sorter) -------------------------------------------

    /// Lay out the slide grid: as many columns as fit, returning each
    /// slide's index and destination rect. Shared by draw + hit-test.
    fn compute_overview(&self) -> (Rect, Vec<(usize, Rect)>) {
        let header_h = 64.0;
        let area = Rect::new(self.rect.x0, self.rect.y0 + header_h, self.rect.x1, self.rect.y1);
        let pad = 36.0;
        let gap = 26.0;
        let avail_w = (area.width() - 2.0 * pad).max(200.0);
        // Aim for a thumbnail ~250px wide, then fit an integer column count.
        let target = 260.0;
        let cols = ((avail_w + gap) / (target + gap)).floor().max(1.0) as usize;
        let tw = (avail_w - (cols as f64 - 1.0) * gap) / cols as f64;
        let th = tw * SLIDE_H / SLIDE_W;
        let mut out = Vec::new();
        for i in 0..self.deck.slides.len() {
            let col = i % cols;
            let row = i / cols;
            let x = area.x0 + pad + col as f64 * (tw + gap);
            let y = area.y0 + pad + row as f64 * (th + 30.0 + gap);
            out.push((i, Rect::new(x, y, x + tw, y + th)));
        }
        (area, out)
    }

    fn draw_overview(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let pal = self.palette(theme);
        let c = &theme.tokens.colors;
        chrome::fill_rect(scene, t, self.rect, c.surfaces.base);

        // Header row.
        tr.draw_text_weighted(
            scene,
            &self.deck.title,
            18.0,
            c.text_primary,
            t * Affine::translate((self.rect.x0 + 36.0, self.rect.y0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            &format!("{} slides  ·  click to open  ·  Esc / G to return", self.deck.slides.len()),
            12.0,
            c.text_dim,
            t * Affine::translate((self.rect.x0 + 36.0, self.rect.y0 + 38.0)),
            None,
            TextWeight::Regular,
        );
        stroke_line(scene, t, self.rect.x0, self.rect.y0 + 60.0, self.rect.x1, self.rect.y0 + 60.0, c.border_standard);

        let (_, thumbs) = self.compute_overview();
        for (i, rect) in thumbs {
            if let Some(slide) = self.deck.slides.get(i) {
                // Drop shadow + card.
                let sh = rect + vello::kurbo::Vec2::new(0.0, 4.0);
                chrome::fill_rrect(scene, t, sh.inflate(1.0, 1.0), 8.0, render::with_alpha(Color::BLACK, 60));
                render::draw_slide(scene, t, tr, theme, &pal, slide, rect, 8.0);
                let (ring, w) = if i == self.cur_slide {
                    (pal.accent, 2.5)
                } else {
                    (render::with_alpha(c.text_primary, 30), 1.0)
                };
                chrome::stroke_rrect(scene, t, rect, 8.0, ring, w);
                // Number badge.
                let badge = Rect::new(rect.x0 + 6.0, rect.y0 + 6.0, rect.x0 + 30.0, rect.y0 + 28.0);
                chrome::fill_rrect(scene, t, badge, 6.0, render::with_alpha(Color::BLACK, 120));
                chrome::text_centered(scene, t, tr, &format!("{}", i + 1), badge, 12.0, Color::WHITE, TextWeight::Semibold);
                // Layout caption beneath.
                tr.draw_text_weighted(
                    scene,
                    slide.layout.label(),
                    11.5,
                    c.text_dim,
                    t * Affine::translate((rect.x0 + 2.0, rect.y1 + 7.0)),
                    None,
                    TextWeight::Regular,
                );
            }
        }
    }

    // --- keyboard-shortcuts overlay ----------------------------------------

    fn draw_help(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let pal = self.palette(theme);
        // Scrim.
        chrome::fill_rect(scene, t, self.rect, render::with_alpha(Color::BLACK, 150));
        // Centered card.
        let cw = 560.0_f64.min(self.rect.width() - 80.0);
        let ch = 520.0_f64.min(self.rect.height() - 80.0);
        let cx = self.rect.x0 + (self.rect.width() - cw) / 2.0;
        let cy = self.rect.y0 + (self.rect.height() - ch) / 2.0;
        let card = Rect::new(cx, cy, cx + cw, cy + ch);
        chrome::fill_rrect(scene, t, card, 16.0, c.surfaces.raised);
        chrome::stroke_rrect(scene, t, card, 16.0, render::with_alpha(c.text_primary, 30), 1.0);

        let px = card.x0 + 32.0;
        tr.draw_text_weighted(
            scene,
            "Keyboard shortcuts",
            20.0,
            c.text_primary,
            t * Affine::translate((px, card.y0 + 24.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text_weighted(
            scene,
            "Press ?  or Esc to close",
            12.0,
            c.text_dim,
            t * Affine::translate((px, card.y0 + 50.0)),
            None,
            TextWeight::Regular,
        );

        let rows: [(&str, &str); 16] = [
            ("N", "New slide"),
            ("P", "Present from current slide"),
            ("G", "Slide-sorter overview"),
            ("T / R / O / L", "Insert text · rect · ellipse · line"),
            ("Tab", "Cycle selection on the slide"),
            ("Enter", "Edit selected text box"),
            ("Drag", "Move · resize (snaps to guides)"),
            ("Arrows", "Nudge selection (Shift = 1px)"),
            ("[  /  ]", "Send back · bring to front"),
            ("Ctrl-D", "Duplicate element"),
            ("Del / Backspace", "Delete element"),
            ("Ctrl-Z / Ctrl-Y", "Undo · redo"),
            ("← →", "Present: previous · next slide"),
            ("S", "Present: toggle speaker notes"),
            ("B", "Present: black out the screen"),
            ("?", "Toggle this help"),
        ];
        let mut y = card.y0 + 86.0;
        let key_w = 168.0;
        for (k, d) in rows {
            tr.draw_text_weighted(
                scene,
                k,
                13.0,
                pal.accent,
                t * Affine::translate((px, y)),
                Some(key_w as f32),
                TextWeight::Semibold,
            );
            tr.draw_text_weighted(
                scene,
                d,
                13.0,
                c.text_primary,
                t * Affine::translate((px + key_w, y)),
                Some((cw - key_w - 64.0) as f32),
                TextWeight::Regular,
            );
            y += 26.0;
        }
    }
}

fn mix_panel(a: Color, b: Color) -> Color {
    Color::from_rgba8(
        ((a.r() as u16 + b.r() as u16) / 2) as u8,
        ((a.g() as u16 + b.g() as u16) / 2) as u8,
        ((a.b() as u16 + b.b() as u16) / 2) as u8,
        255,
    )
}

fn stroke_line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, color: Color) {
    let line = Line::new(Point::new(x0, y0), Point::new(x1, y1));
    scene.stroke(&Stroke::new(1.0), t, color, None, &line);
}

impl Widget for SlidesApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if SlidesDoc::is_empty(state) {
                SlidesDoc::seed(state, &self.deck);
            }
            self.seeded = true;
        }
        self.reload(state);
        // One-time view bootstrap: open on the second slide with its title
        // selected so the editor demonstrates selection + the element
        // inspector immediately (also what `--shots` captures).
        if !self.bootstrapped && self.deck.slides.len() > 1 {
            self.cur_slide = 1;
            if let Some(slide) = self.deck.slides.get(1) {
                self.sel = slide
                    .elements
                    .iter()
                    .find(|e| e.kind == ElementKind::Text)
                    .map(|e| e.id.clone());
            }
            self.bootstrapped = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() < 2.0 {
            return;
        }
        // Expire a settled cross-fade so `needs_animation_frame` can rest.
        if let Some((_, t)) = self.transition {
            if t.elapsed().as_millis() >= TRANSITION_MS {
                self.transition = None;
            }
        }
        // Reconcile the rail flap with the current width (so resizing across
        // the breakpoint returns to the layout default) and advance its eased
        // reveal using real elapsed time, frame-rate independent.
        let now = std::time::Instant::now();
        let dt = (now - self.last_flap_tick).as_secs_f64().min(0.1);
        self.last_flap_tick = now;
        self.sync_rail_flap(dt);
        match self.mode {
            Mode::Edit => {
                let chrome = self.compute_chrome();
                self.draw_editor(scene, transform, text_renderer, theme, &chrome);
            }
            Mode::Overview => self.draw_overview(scene, transform, text_renderer, theme),
            Mode::Present => self.draw_present(scene, transform, text_renderer, theme),
        }
        if self.show_help && self.mode != Mode::Present {
            self.draw_help(scene, transform, text_renderer, theme);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames pumping while the speaker timer is live, a slide
        // cross-fade is mid-flight, or the rail flap is sliding open/closed.
        self.mode == Mode::Present
            || self.transition.is_some()
            || self.rail_flap.is_animating(self.rail_layout())
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                if self.mode == Mode::Edit && self.drag.active() {
                    self.drag_move(p);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: btn_state, button: MouseButton::Left, .. } => {
                match btn_state {
                    ElementState::Pressed => self.on_press(state, self.last_mouse, handled),
                    ElementState::Released => self.on_release(state, handled),
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
                ..
            } => {
                self.on_key(state, logical_key, text.as_ref().map(|s| s.as_str()), modifiers, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SlidesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_commit(&self) -> bool {
        self.needs_commit
    }

    fn on_commit(&mut self) {
        self.needs_commit = false;
    }
}

impl SlidesApp {
    /// Snap a moving element to the slide's center/edge guides and to the
    /// anchors (left/center/right · top/middle/bottom) of its siblings.
    /// Returns the adjusted top-left plus the guide lines that fired.
    fn snapped_move(
        &self,
        id: &str,
        nx: f64,
        ny: f64,
        w: f64,
        h: f64,
        thr: f64,
    ) -> (f64, f64, Vec<Guide>) {
        // Candidate target lines in slide space.
        let mut vx: Vec<f64> = vec![0.0, SLIDE_W / 2.0, SLIDE_W, 96.0, SLIDE_W - 96.0];
        let mut hy: Vec<f64> = vec![0.0, SLIDE_H / 2.0, SLIDE_H, 70.0, SLIDE_H - 60.0];
        if let Some(slide) = self.slide() {
            for e in &slide.elements {
                if e.id == id {
                    continue;
                }
                vx.push(e.x);
                vx.push(e.x + e.w / 2.0);
                vx.push(e.x + e.w);
                hy.push(e.y);
                hy.push(e.y + e.h / 2.0);
                hy.push(e.y + e.h);
            }
        }
        let mut guides = Vec::new();

        // X axis: try left, center, right anchors; keep the closest snap.
        let anchors_x = [(nx, 0.0), (nx + w / 2.0, w / 2.0), (nx + w, w)];
        let mut best_x: Option<(f64, f64, f64)> = None; // (dist, new_left, guide_line)
        for (anchor, off) in anchors_x {
            for &target in &vx {
                let d = (anchor - target).abs();
                if d <= thr && best_x.map(|b| d < b.0).unwrap_or(true) {
                    best_x = Some((d, target - off, target));
                }
            }
        }
        let out_x = if let Some((_, left, line)) = best_x {
            guides.push(Guide::Vertical(line));
            left
        } else {
            nx
        };

        let anchors_y = [(ny, 0.0), (ny + h / 2.0, h / 2.0), (ny + h, h)];
        let mut best_y: Option<(f64, f64, f64)> = None;
        for (anchor, off) in anchors_y {
            for &target in &hy {
                let d = (anchor - target).abs();
                if d <= thr && best_y.map(|b| d < b.0).unwrap_or(true) {
                    best_y = Some((d, target - off, target));
                }
            }
        }
        let out_y = if let Some((_, top, line)) = best_y {
            guides.push(Guide::Horizontal(line));
            top
        } else {
            ny
        };

        (out_x, out_y, guides)
    }

    fn drag_move(&mut self, p: Point) {
        let chrome = self.compute_chrome();
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        let sp = self.to_slide(dest, s, p);
        match self.drag.clone() {
            Drag::Move { id, start, orig_x, orig_y } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (w, h) = self
                    .selected_element()
                    .map(|e| (e.w, e.h))
                    .unwrap_or((0.0, 0.0));
                let thr = SNAP_PX / s.max(0.0001);
                let (nx, ny, guides) =
                    self.snapped_move(&id, orig_x + dx, orig_y + dy, w, h, thr);
                self.active_guides = guides;
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide) {
                    if let Some(el) = slide.element_mut(&id) {
                        el.x = nx;
                        el.y = ny;
                    }
                }
            }
            Drag::Resize { id, handle, start, orig } => {
                let dx = sp.x - start.x;
                let dy = sp.y - start.y;
                let (mut x, mut y, mut w, mut h) = orig;
                let left = matches!(handle, Handle::Tl | Handle::L | Handle::Bl);
                let right = matches!(handle, Handle::Tr | Handle::R | Handle::Br);
                let top = matches!(handle, Handle::Tl | Handle::T | Handle::Tr);
                let bottom = matches!(handle, Handle::Bl | Handle::B | Handle::Br);
                if left {
                    x = orig.0 + dx;
                    w = orig.2 - dx;
                }
                if right {
                    w = orig.2 + dx;
                }
                if top {
                    y = orig.1 + dy;
                    h = orig.3 - dy;
                }
                if bottom {
                    h = orig.3 + dy;
                }
                let min = 16.0;
                if w < min {
                    if left {
                        x = orig.0 + orig.2 - min;
                    }
                    w = min;
                }
                if h < min {
                    if top {
                        y = orig.1 + orig.3 - min;
                    }
                    h = min;
                }
                if let Some(slide) = self.deck.slides.get_mut(self.cur_slide) {
                    if let Some(el) = slide.element_mut(&id) {
                        el.x = x;
                        el.y = y;
                        el.w = w;
                        el.h = h;
                    }
                }
            }
            Drag::None => {}
        }
    }

    fn on_press(&mut self, doc: &mut AutoCommit, p: Point, handled: &mut bool) {
        // The help overlay swallows the next click (to dismiss).
        if self.show_help {
            self.show_help = false;
            *handled = true;
            return;
        }
        if self.mode == Mode::Present {
            if self.blackout {
                self.blackout = false;
            } else {
                self.next_slide(1);
            }
            *handled = true;
            return;
        }
        if self.mode == Mode::Overview {
            let (_, thumbs) = self.compute_overview();
            for (i, rect) in thumbs {
                if rect.contains(p) {
                    self.apply_hit(doc, Hit::OpenSlide(i));
                    *handled = true;
                    return;
                }
            }
            *handled = true;
            return;
        }
        let chrome = self.compute_chrome();

        // Buttons first (the hamburger toggle lives here, so it wins over the
        // scrim dismissal below).
        for b in &chrome.buttons {
            if b.rect.contains(p) {
                let hit = b.hit.clone();
                self.apply_hit(doc, hit);
                *handled = true;
                return;
            }
        }
        // Scrim dismissal: with the rail open as an overlay flap, a click in
        // the dimmed body (outside the rail itself) closes it — like tapping
        // the scrim in Adwaita's flap.
        if chrome.layout == SplitLayout::Overlay
            && self.rail_open()
            && chrome.scrim.contains(p)
            && !chrome.rail.contains(p)
        {
            self.rail_flap.close();
            *handled = true;
            return;
        }
        // Thumbnails. Selecting one on a narrow window also dismisses the
        // overlay flap so the chosen slide is revealed.
        for (i, rect) in &chrome.thumbs {
            if rect.contains(p) {
                self.apply_hit(doc, Hit::SelectSlide(*i));
                if chrome.layout == SplitLayout::Overlay {
                    self.rail_flap.close();
                }
                *handled = true;
                return;
            }
        }
        // Canvas: handle selection / drag.
        if chrome.slide_dest.inflate(12.0, 12.0).contains(p) {
            self.press_canvas(doc, p, &chrome, handled);
            return;
        }
        // Clicked empty chrome → drop selection / exit text edit.
        if self.sel.is_some() || self.editing_text || self.editing_notes {
            self.sel = None;
            self.editing_text = false;
            self.editing_notes = false;
            *handled = true;
        }
    }

    fn press_canvas(&mut self, doc: &mut AutoCommit, p: Point, chrome: &Chrome, handled: &mut bool) {
        let dest = chrome.slide_dest;
        let s = chrome.scale;
        // Resize handle of the current selection?
        if let Some(el) = self.selected_element().cloned() {
            if !self.editing_text {
                for (h, hr) in self.handles_of(dest, s, &el) {
                    if hr.contains(p) {
                        self.drag = Drag::Resize {
                            id: el.id.clone(),
                            handle: h,
                            start: self.to_slide(dest, s, p),
                            orig: (el.x, el.y, el.w, el.h),
                        };
                        *handled = true;
                        return;
                    }
                }
            }
        }
        // Hit-test elements front-to-back.
        let mut picked: Option<Id> = None;
        if let Some(slide) = self.slide() {
            let mut z: Vec<&Element> = slide.elements.iter().collect();
            z.sort_by_key(|e| -e.z);
            for el in z {
                let er = render::element_dest(dest, s, el);
                if er.inflate(2.0, 2.0).contains(p) {
                    picked = Some(el.id.clone());
                    break;
                }
            }
        }
        match picked {
            Some(id) => {
                let was_selected = self.sel.as_deref() == Some(id.as_str());
                self.sel = Some(id.clone());
                self.editing_notes = false;
                let is_text = self
                    .selected_element()
                    .map(|e| e.kind == ElementKind::Text)
                    .unwrap_or(false);
                if was_selected && is_text {
                    // Second click on a selected text box → edit.
                    self.editing_text = true;
                    self.caret = self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0);
                } else {
                    self.editing_text = false;
                    let el = self.selected_element().cloned().unwrap();
                    self.drag = Drag::Move {
                        id,
                        start: self.to_slide(dest, s, p),
                        orig_x: el.x,
                        orig_y: el.y,
                    };
                }
                *handled = true;
            }
            None => {
                self.sel = None;
                self.editing_text = false;
                *handled = true;
            }
        }
        let _ = doc;
    }

    fn on_release(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
        if self.mode != Mode::Edit {
            return;
        }
        self.active_guides.clear();
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        let id = match drag {
            Drag::Move { id, .. } => Some(id),
            Drag::Resize { id, .. } => Some(id),
            Drag::None => None,
        };
        if let Some(id) = id {
            if let Some(el) = self.deck.slides.get(self.cur_slide).and_then(|s| s.element(&id)).cloned() {
                SlidesDoc::set_element_rect(doc, self.cur_slide, &id, el.x, el.y, el.w, el.h);
                self.mark();
                *handled = true;
            }
            self.reload(doc);
        }
    }

    fn on_key(
        &mut self,
        doc: &mut AutoCommit,
        key: &Key,
        text: Option<&str>,
        modifiers: &Modifiers,
        handled: &mut bool,
    ) {
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();

        // Help overlay owns all keys while open.
        if self.show_help {
            match key {
                Key::Named(NamedKey::Escape) => self.show_help = false,
                Key::Character(c) if c.as_str() == "?" => self.show_help = false,
                _ => {}
            }
            *handled = true;
            return;
        }
        // `?` opens help from any non-text context.
        if !self.editing_text && !self.editing_notes {
            if let Key::Character(c) = key {
                if c.as_str() == "?" {
                    self.show_help = true;
                    *handled = true;
                    return;
                }
            }
        }

        // Esc dismisses an open overlay flap first (when not editing text).
        if self.mode == Mode::Edit
            && !self.editing_text
            && !self.editing_notes
            && matches!(key, Key::Named(NamedKey::Escape))
            && self.rail_layout() == SplitLayout::Overlay
            && self.rail_open()
        {
            self.rail_flap.close();
            *handled = true;
            return;
        }

        // Overview (slide-sorter) navigation.
        if self.mode == Mode::Overview {
            match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    self.mode = Mode::Edit;
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("g") => {
                    self.mode = Mode::Edit;
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::ArrowDown) => {
                    if self.cur_slide + 1 < self.deck.slides.len() {
                        self.cur_slide += 1;
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowUp) => {
                    self.cur_slide = self.cur_slide.saturating_sub(1);
                    *handled = true;
                }
                _ => {}
            }
            return;
        }

        // Present-mode keys.
        if self.mode == Mode::Present {
            match key {
                Key::Named(NamedKey::ArrowRight)
                | Key::Named(NamedKey::ArrowDown)
                | Key::Named(NamedKey::Space)
                | Key::Named(NamedKey::Enter)
                | Key::Named(NamedKey::PageDown) => {
                    self.next_slide(1);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowUp)
                | Key::Named(NamedKey::PageUp)
                | Key::Named(NamedKey::Backspace) => {
                    self.next_slide(-1);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.mode = Mode::Edit;
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("s") => {
                    self.show_notes = !self.show_notes;
                    *handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("b") => {
                    self.blackout = !self.blackout;
                    *handled = true;
                }
                _ => {}
            }
            return;
        }

        // Text / notes editing intercepts most keys.
        if self.editing_text || self.editing_notes {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.editing_text = false;
                    self.editing_notes = false;
                    self.mark();
                    *handled = true;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.backspace_text(doc);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.insert_text(doc, "\n");
                    *handled = true;
                }
                Key::Named(NamedKey::Space) => {
                    self.insert_text(doc, " ");
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    if self.editing_notes {
                        self.notes_caret = self.notes_caret.saturating_sub(1);
                    } else {
                        self.caret = self.caret.saturating_sub(1);
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    let len = self.cur_text_len();
                    if self.editing_notes {
                        self.notes_caret = (self.notes_caret + 1).min(len);
                    } else {
                        self.caret = (self.caret + 1).min(len);
                    }
                    *handled = true;
                }
                Key::Character(c) if !ctrl => {
                    self.insert_text(doc, c.as_str());
                    *handled = true;
                }
                _ => {
                    if let Some(txt) = text {
                        if !txt.is_empty() && !ctrl {
                            self.insert_text(doc, txt);
                            *handled = true;
                        }
                    }
                }
            }
            return;
        }

        // Editor command keys.
        match key {
            Key::Named(NamedKey::Escape) => {
                if self.sel.is_some() {
                    self.sel = None;
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if self.sel.is_some() {
                    self.delete_element(doc);
                    self.reload(doc);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Enter) => {
                if self.selected_element().map(|e| e.kind == ElementKind::Text).unwrap_or(false) {
                    self.editing_text = true;
                    self.caret = self.selected_element().map(|e| e.text.chars().count()).unwrap_or(0);
                    *handled = true;
                }
            }
            Key::Named(NamedKey::Tab) => {
                self.cycle_selection();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.nudge(doc, if mods.shift_key() { -1.0 } else { -12.0 }, 0.0);
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.nudge(doc, if mods.shift_key() { 1.0 } else { 12.0 }, 0.0);
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.nudge(doc, 0.0, if mods.shift_key() { -1.0 } else { -12.0 });
                self.reload(doc);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.nudge(doc, 0.0, if mods.shift_key() { 1.0 } else { 12.0 });
                self.reload(doc);
                *handled = true;
            }
            Key::Character(c) => {
                let lc = c.to_lowercase();
                if ctrl && lc == "d" {
                    self.duplicate_element(doc);
                    self.reload(doc);
                    *handled = true;
                    return;
                }
                match lc.as_str() {
                    "n" => {
                        self.new_slide(doc);
                        self.reload(doc);
                        *handled = true;
                    }
                    "p" => {
                        self.enter_present();
                        *handled = true;
                    }
                    "g" => {
                        self.mode = Mode::Overview;
                        self.sel = None;
                        self.editing_text = false;
                        *handled = true;
                    }
                    "t" => {
                        self.insert_element(doc, ElementKind::Text);
                        self.reload(doc);
                        *handled = true;
                    }
                    "r" => {
                        self.insert_element(doc, ElementKind::Rect);
                        self.reload(doc);
                        *handled = true;
                    }
                    "o" => {
                        self.insert_element(doc, ElementKind::Ellipse);
                        self.reload(doc);
                        *handled = true;
                    }
                    "l" => {
                        self.insert_element(doc, ElementKind::Line);
                        self.reload(doc);
                        *handled = true;
                    }
                    "d" => {
                        self.duplicate_element(doc);
                        self.reload(doc);
                        *handled = true;
                    }
                    "]" => {
                        self.restack(doc, true);
                        self.reload(doc);
                        *handled = true;
                    }
                    "[" => {
                        self.restack(doc, false);
                        self.reload(doc);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn cycle_selection(&mut self) {
        let Some(slide) = self.slide() else { return };
        if slide.elements.is_empty() {
            return;
        }
        let mut z: Vec<&Element> = slide.elements.iter().collect();
        z.sort_by_key(|e| e.z);
        let ids: Vec<Id> = z.iter().map(|e| e.id.clone()).collect();
        let next = match &self.sel {
            Some(cur) => {
                let i = ids.iter().position(|x| x == cur).unwrap_or(usize::MAX);
                ids[(i.wrapping_add(1)) % ids.len()].clone()
            }
            None => ids[0].clone(),
        };
        self.sel = Some(next);
        self.editing_text = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Deck, ElementKind, Layout, PaletteKind, Slide};

    fn app_with(slide: Slide) -> SlidesApp {
        let mut a = SlidesApp::new();
        a.deck = Deck { title: "t".into(), palette: PaletteKind::Indigo, slides: vec![slide] };
        a.cur_slide = 0;
        a.bootstrapped = true;
        a
    }

    #[test]
    fn snaps_element_center_to_slide_center() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 200.0, 100.0));
        let app = app_with(s);
        let (w, h) = (200.0, 100.0);
        // Drag so the element's center-x lands 4u shy of the slide center.
        let nx = SLIDE_W / 2.0 - w / 2.0 + 4.0;
        let (sx, _sy, guides) = app.snapped_move(&id, nx, 100.0, w, h, 8.0);
        assert!(
            (sx - (SLIDE_W / 2.0 - w / 2.0)).abs() < 1e-6,
            "center should snap to slide center, got left={sx}"
        );
        assert!(guides.iter().any(|g| matches!(g, Guide::Vertical(_))));
    }

    #[test]
    fn no_snap_when_outside_threshold() {
        let mut s = Slide::new(Layout::Blank);
        let id = s.add(Element::shape(ElementKind::Rect, 0.0, 0.0, 50.0, 50.0));
        let app = app_with(s);
        let (sx, sy, guides) = app.snapped_move(&id, 300.0, 300.0, 50.0, 50.0, 4.0);
        assert_eq!((sx, sy), (300.0, 300.0));
        assert!(guides.is_empty());
    }

    #[test]
    fn present_clock_formats_minutes_seconds() {
        let app = SlidesApp::new();
        // No presentation started → 0:00.
        assert_eq!(app.present_clock(), "0:00");
    }

    // --- responsive slide rail ---------------------------------------------

    /// Build an app at a fixed allocated width and run one frame's flap sync
    /// (the real per-frame path) so `compute_chrome` resolves the rail layout
    /// and eased reveal exactly as it would live.
    fn app_sized(w: f64) -> SlidesApp {
        let mut s = Slide::new(Layout::TitleContent);
        s.add(Element::text("hello", 96.0, 90.0, 1088.0, 100.0, 54.0));
        let mut a = app_with(s);
        a.rect = Rect::new(0.0, 0.0, w, 800.0);
        a.sync_rail_flap(0.0);
        a
    }

    #[test]
    fn wide_window_docks_the_rail_beside_the_canvas() {
        let app = app_sized(NARROW_BREAKPOINT + 200.0);
        assert_eq!(app.rail_layout(), SplitLayout::Docked);
        assert!(app.rail_open(), "docked rail is open by default");
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Docked);
        // The rail occupies the full left column and the canvas starts past it.
        assert!((chrome.rail.x0 - app.rect.x0).abs() < 1e-6);
        assert!(chrome.rail.width() > RAIL_W - 1.0, "docked rail is full width");
        assert!(
            chrome.slide_dest.x0 > chrome.rail.x1 - 1.0,
            "canvas sits to the right of the docked rail"
        );
        assert_eq!(chrome.scrim.width(), 0.0, "no scrim while docked");
        // No hamburger when the rail is docked + fully open.
        assert!(
            !chrome.buttons.iter().any(|b| matches!(b.kind, ButtonKind::Hamburger)),
            "no hamburger needed when the rail is docked open"
        );
    }

    #[test]
    fn narrow_window_collapses_rail_to_a_closed_overlay() {
        let app = app_sized(NARROW_BREAKPOINT - 200.0);
        assert_eq!(app.rail_layout(), SplitLayout::Overlay);
        assert!(!app.rail_open(), "overlay rail is closed by default");
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Overlay);
        // Canvas spans the full width (rail no longer insets it).
        assert!(
            (chrome.slide_dest.x0 - app.rect.x0) < (RAIL_W * 0.5),
            "narrow canvas reclaims the rail's column"
        );
        // The closed flap is fully off the left edge, and no scrim shows.
        assert!(chrome.rail.x1 <= app.rect.x0 + 1.0, "closed flap is off-screen left");
        assert_eq!(chrome.scrim.width(), 0.0, "no scrim while the flap is closed");
        // A hamburger is offered to summon it.
        assert!(
            chrome.buttons.iter().any(|b| matches!(b.kind, ButtonKind::Hamburger)),
            "narrow layout offers a hamburger to summon the rail"
        );
    }

    #[test]
    fn opening_the_overlay_flap_reveals_the_rail_over_a_scrim() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        // Summon via the production toggle path, then settle the animation.
        app.rail_flap.toggle(app.rail_layout());
        app.rail_flap.snap(true);
        assert!(app.rail_open());
        let chrome = app.compute_chrome();
        assert_eq!(chrome.layout, SplitLayout::Overlay);
        // Fully revealed: the flap is flush with the left edge over a scrim.
        assert!((chrome.rail.x0 - app.rect.x0).abs() < 1.0, "open flap is flush left");
        assert!(chrome.scrim.width() > 0.0, "an open overlay flap dims the body");
    }

    #[test]
    fn hamburger_hit_toggles_the_overlay_flap() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        let mut doc = AutoCommit::new();
        assert!(!app.rail_open());
        // Drive the real action dispatcher, not a private setter.
        app.apply_hit(&mut doc, Hit::ToggleRail);
        assert!(app.rail_open(), "ToggleRail opens the collapsed flap");
        app.apply_hit(&mut doc, Hit::ToggleRail);
        assert!(!app.rail_open(), "ToggleRail closes it again");
    }

    #[test]
    fn resizing_across_the_breakpoint_returns_to_the_layout_default() {
        let mut app = app_sized(NARROW_BREAKPOINT - 200.0);
        // User opens the flap while narrow.
        app.rail_flap.reconcile(app.rail_layout());
        app.apply_hit(&mut AutoCommit::new(), Hit::ToggleRail);
        assert!(app.rail_open());
        // Widen past the breakpoint: the stale preference drops and the rail
        // adopts the docked default (open). Then narrow again → closed default.
        app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT + 200.0, 800.0);
        app.rail_flap.reconcile(app.rail_layout());
        assert_eq!(app.rail_layout(), SplitLayout::Docked);
        assert!(app.rail_open(), "docked default is open");
        app.rect = Rect::new(0.0, 0.0, NARROW_BREAKPOINT - 200.0, 800.0);
        app.rail_flap.reconcile(app.rail_layout());
        assert!(!app.rail_open(), "overlay default is closed after re-narrowing");
    }
}
