//! The keys of an on-screen keyboard: rows of key caps, laid out by their widths and docked at
//! the bottom of the box it is given, with what a finger does to them reported as data.
//!
//! The app owns which keys there are (its layouts, their words and the purpose a field asks), the
//! words around them (the toolbar, the suggestions, the emoji pane) and what a press means: it is
//! drawn from what it is shown, and this pad draws the keys and says what happened. So a view is
//! rebuilt without the pad ever holding a copy of the field.
//!
//! What a finger may do, and what the pad reports for it:
//!
//! - **Tap.** A key types when the finger lifts, not when it lands, so a finger that slides off
//!   types nothing. With a mouse, a press and release on the same key is a tap.
//! - **Both thumbs.** The keys claim raw touch, so every finger is tracked on its own: a finger
//!   that lands while another holds a key types the held one at once, so typing in press order
//!   drops no key ([`Widget::claims_raw_touch`]). Only a finger that stayed alone may swipe.
//! - **Hold.** A key with forms offers them after [`LONG_PRESS`]: a row of them opens above the
//!   key, sliding picks one and lifting types it; released without sliding, the plain key types
//!   once. A key that repeats (Backspace) acts at once after [`REPEAT_DELAY`], then every
//!   [`REPEAT_INTERVAL`]. A secret field offers no forms.
//! - **Swipe.** A solo finger that crossed the keys and lifted has its path reported, in fractions
//!   of the pad's box and with the box's width in its heights, so the app can ask what word it
//!   traced on the keys it drew.
//! - **Caret.** Dragging along the space bar reports caret steps, one per quarter key, after half
//!   a key of travel.
//! - **Preview.** A pressed character shows its glyph in a bubble above the finger, so the thumb
//!   does not hide what it types; never in a secret field.
//!
//! Presses, holds, swipes and caret steps are [`KeyPadProps`]' events. Assistive technology reads
//! each key as a button where it stands, and activating one is the tap it stands for.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::implicit_hasher,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_as_ptr,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::useless_conversion,
    reason = "lint debt these widgets carried in the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard, and tests may wait on the real clock"
    )
)]

use tilo_ui::time::{Duration, Instant, now};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use tilo_ui::widget::Interaction;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{
    a11y::AccessNode, a11y::AccessState, a11y::Action as AccessibilityAction, a11y::Role,
};
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle, text_render::TextWeight};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

#[path = "key_pad_component.rs"]
mod component;
pub use component::{KeyPadProps, key_pad_description};

tilo_ui_base::choice!(PadSize, "PadSize", {
    Compact => "compact",
    Standard => "standard",
    Large => "large",
});

tilo_ui_base::choice!(PadTone, "PadTone", {
    Plain => "plain",
    Modifier => "modifier",
    Active => "active",
    Primary => "primary",
});

/// How tall a key's rows are, as a multiple of the theme's row height: a keyboard that keeps its
/// rows whatever the desktop's density, compact or large by the person's choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PadSize {
    Compact,
    #[default]
    Standard,
    Large,
}

impl PadSize {
    #[must_use]
    pub const fn scale(self) -> f32 {
        match self {
            Self::Compact => 0.85,
            Self::Standard => 1.0,
            Self::Large => 1.25,
        }
    }
}

/// How a key stands on the pad: a letter (a faint lift off the ground), a modifier (on the ground
/// until touched, so the letters lead), or the key that commits, in the accent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PadTone {
    #[default]
    Plain,
    Modifier,
    /// Latched (Shift with Caps Lock on): the accent, filled.
    Active,
    Primary,
}

/// One key of the pad.
#[derive(Clone, Debug, PartialEq)]
pub struct PadKey {
    /// What it shows: a word, or one character.
    pub label: String,
    /// What assistive technology calls it, where that says more than the label.
    pub name: String,
    /// A glyph instead of the label, by its icon name (`delete`); empty for none.
    pub icon: String,
    pub tone: PadTone,
    /// Its width in letter keys.
    pub width: f64,
    pub row: i64,
    pub column: i64,
    /// What a long press offers, the plain key first; empty for none.
    pub holds: Vec<String>,
    /// Holding it acts again and again.
    pub repeat: bool,
    /// Dragging along it moves the caret.
    pub caret: bool,
}

impl Default for PadKey {
    fn default() -> Self {
        Self {
            label: String::new(),
            name: String::new(),
            icon: String::new(),
            tone: PadTone::Plain,
            width: 1.0,
            row: 0,
            column: 0,
            holds: Vec::new(),
            repeat: false,
            caret: false,
        }
    }
}

/// A glyph key's character, as a fraction of its cell height: keys are a content surface, so the
/// glyph follows the key it sits on, not the text scale.
const KEY_GLYPH_RATIO: f64 = 0.46;
const PREVIEW_GLYPH_RATIO: f64 = 0.5;
const OPTION_GLYPH_RATIO: f64 = 0.5;
/// How much of a row's height the gap between keys takes, each side.
const KEY_GAP_RATIO: f64 = 0.06;
/// How long a key with forms waits before offering them, how long a repeating key waits before it
/// repeats, and how often it repeats after.
const LONG_PRESS: Duration = Duration::from_millis(350);
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_INTERVAL: Duration = Duration::from_millis(50);
/// How far along the space bar a drag travels before it steers the caret, and how far between
/// steps, in key sizes.
const CURSOR_START: f64 = 0.5;
const CURSOR_STEP: f64 = 0.25;
/// The most points a gesture keeps: sampling is bounded, and the latest endpoint is kept.
const MAX_GESTURE_POINTS: usize = 256;
/// Marks that are a dot or a tick in any face: drawn larger and heavier than a letter.
const SMALL_MARKS: &str = ".,:;'\"-_*~^`";
const SMALL_MARK_SCALE: f32 = 1.35;

/// One finger on the pad, in press order.
#[derive(Clone, Debug)]
struct Finger {
    id: u64,
    key: Option<usize>,
    at: Point,
    path: Vec<Point>,
    /// It is the only finger on the keys, so its drag may be a swipe.
    solo: bool,
    /// Its key already typed because another finger landed; lifting it types nothing more.
    spent: bool,
}

/// A key being held: its forms about to open, or its repeat about to act.
#[derive(Clone, Copy, Debug)]
struct Hold {
    key: usize,
    /// The finger holding it; none for the mouse.
    finger: Option<u64>,
    due: Instant,
    kind: HoldKind,
    fired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldKind {
    Alternates,
    Repeat,
}

/// The row of a long press's forms: above the key, one cell each, the plain key under the finger.
#[derive(Clone, Debug, PartialEq)]
struct Alternates {
    key: usize,
    finger: Option<u64>,
    options: Vec<String>,
    selected: usize,
    cells: Vec<Rect>,
}

impl Alternates {
    fn new(key: usize, key_rect: Rect, options: Vec<String>, bounds: Rect) -> Self {
        let cells = alternates_cells(key_rect, options.len(), bounds);
        Self {
            key,
            finger: None,
            options,
            selected: 0,
            cells,
        }
    }

    /// Select the option nearest `x`: only the horizontal position counts, so a finger drifting up
    /// or down keeps its choice.
    fn track(&mut self, x: f64) {
        let nearest = self
            .cells
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let da = (a.1.center().x - x).abs();
                let db = (b.1.center().x - x).abs();
                da.total_cmp(&db)
            })
            .map(|(at, _)| at);
        if let Some(at) = nearest {
            self.selected = at;
        }
    }

    fn chosen(&self) -> Option<&str> {
        self.options.get(self.selected).map(String::as_str)
    }
}

/// A drag along the space bar that steers the caret.
#[derive(Clone, Copy, Debug)]
struct CursorDrag {
    finger: Option<u64>,
    anchor: f64,
    active: bool,
}

/// The rows of keys an on-screen keyboard draws, at the bottom of the box it is given.
pub struct KeyPad {
    keys: Vec<PadKey>,
    pub(crate) size: PadSize,
    pub(crate) caps: bool,
    pub(crate) secret: bool,
    pub(crate) enabled: bool,
    pub(crate) label: Option<String>,
    pub(crate) description: Option<String>,
    rect: Rect,
    /// Every key's cell, by its place in `keys`, and the box its keys fill.
    cells: Vec<Rect>,
    band: Rect,
    row_height: f64,
    /// A row's height as the theme wants it, read when the pad is laid out: layout places the
    /// keys with it, so where each key stands is known before the pad is first drawn, to an
    /// assistive technology and to a touch alike.
    wanted_row: f64,
    /// Whether `cells` stand for the keys and the box the pad holds now.
    placed: bool,
    hovered: Option<usize>,
    pressed: Option<usize>,
    fingers: Vec<Finger>,
    /// The geometry at the contact's start: a release in geometry that moved acts on nothing.
    contact: Option<Vec<Rect>>,
    hold: Option<Hold>,
    alternates: Option<Alternates>,
    cursor: Option<CursorDrag>,
    pointer: Option<Point>,
    /// The mouse's captured path while its button is down.
    gesture: Option<Vec<Point>>,
    pub(crate) on_press: Option<Box<dyn FnMut(i64, (f64, f64))>>,
    pub(crate) on_hold: Option<Box<dyn FnMut(i64, String)>>,
    pub(crate) on_swipe: Option<Box<dyn FnMut(Vec<f64>, f64)>>,
    pub(crate) on_caret: Option<Box<dyn FnMut(i64)>>,
    dirty: bool,
}

impl Default for KeyPad {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            size: PadSize::Standard,
            caps: false,
            secret: false,
            enabled: true,
            label: None,
            description: None,
            rect: Rect::ZERO,
            cells: Vec::new(),
            band: Rect::ZERO,
            row_height: 0.0,
            wanted_row: 0.0,
            placed: false,
            hovered: None,
            pressed: None,
            fingers: Vec::new(),
            contact: None,
            hold: None,
            alternates: None,
            cursor: None,
            pointer: None,
            gesture: None,
            on_press: None,
            on_hold: None,
            on_swipe: None,
            on_caret: None,
            dirty: true,
        }
    }
}

impl KeyPad {
    /// The keys as the pad holds them.
    #[must_use]
    pub fn keys(&self) -> &[PadKey] {
        &self.keys
    }

    /// Take `keys`, dropping what belonged to the keys before: a contact in geometry that changed
    /// must not type on the keys that took its place.
    pub fn set_keys(&mut self, keys: Vec<PadKey>) {
        if keys != self.keys {
            self.cancel_contacts();
            let rows = self.rows();
            self.keys = keys;
            self.dirty = true;
            self.replace(rows == self.rows());
        }
    }

    /// Place the keys again in the box the pad holds, so the keys a press brought (Shift's
    /// capitals, the symbols, a modifier's row) are where a screen reader and the next touch
    /// look for them at once. Unless `same_height`, the pad asks for a layout too: its height
    /// follows its rows.
    fn replace(&mut self, same_height: bool) {
        if self.rect.area() > 0.0 && self.wanted_row > 0.0 {
            self.place_rows();
        }
        self.placed = same_height && self.rect.area() > 0.0 && self.wanted_row > 0.0;
    }

    /// Take `size`: the keys are placed again at its row height.
    pub(crate) fn set_size(&mut self, size: PadSize) {
        if size != self.size {
            self.size = size;
            self.dirty = true;
            self.replace(false);
        }
    }

    /// Where the keys stand, by their place in `keys`.
    #[must_use]
    pub fn key_geometry(&self) -> &[Rect] {
        &self.cells
    }

    fn rows(&self) -> usize {
        self.keys
            .iter()
            .map(|key| usize::try_from(key.row.max(0)).unwrap_or(0))
            .max()
            .map_or(0, |row| row.saturating_add(1))
    }

    /// Place every key: rows split the band's height, and a row's width by the keys' weights, left
    /// to right in their columns.
    fn place(&mut self, theme: &Theme) {
        self.wanted_row = f64::from(theme.tokens.metrics.header_height);
        self.place_rows();
    }

    /// Place every key at the row height the theme last asked for.
    fn place_rows(&mut self) {
        self.placed = true;
        let rows = self.rows();
        if rows == 0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            self.cells = Vec::new();
            self.band = Rect::ZERO;
            self.row_height = 0.0;
            return;
        }
        // The keys keep the theme's row height, by the person's size, and dock at the bottom: a
        // panel taller than the keys is spare room above them, never taller key wells.
        let wanted = self.wanted_row * f64::from(self.size.scale());
        let height = (wanted * rows as f64).min(self.rect.height());
        let row_height = height / rows as f64;
        let band = Rect::new(
            self.rect.x0,
            self.rect.y1 - height,
            self.rect.x1,
            self.rect.y1,
        );
        self.row_height = row_height;
        self.band = band;
        let mut cells = vec![Rect::ZERO; self.keys.len()];
        for row in 0..rows {
            let mut keys: Vec<usize> = self
                .keys
                .iter()
                .enumerate()
                .filter(|(_, key)| usize::try_from(key.row.max(0)).unwrap_or(0) == row)
                .map(|(at, _)| at)
                .collect();
            keys.sort_by_key(|at| self.keys.get(*at).map_or(0, |key| key.column));
            let total: f64 = keys
                .iter()
                .filter_map(|at| self.keys.get(*at))
                .map(|key| key.width.max(0.0))
                .sum();
            if total <= 0.0 {
                continue;
            }
            let unit = band.width() / total;
            let y0 = band.y0 + row as f64 * row_height;
            let mut x = band.x0;
            for at in keys {
                let Some(key) = self.keys.get(at) else {
                    continue;
                };
                let width = unit * key.width.max(0.0);
                cells[at] = Rect::new(x, y0, x + width, y0 + row_height);
                x += width;
            }
        }
        self.cells = cells;
    }

    fn key_at(&self, point: Point) -> Option<usize> {
        self.cells
            .iter()
            .position(|rect| shown(*rect) && rect.contains(point))
    }

    /// The point as the events carry it: fractions of the pad's box.
    fn fraction(&self, point: Point) -> (f64, f64) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return (0.0, 0.0);
        }
        (
            (point.x - self.rect.x0) / self.rect.width(),
            (point.y - self.rect.y0) / self.rect.height(),
        )
    }

    /// The box's width in its heights: what keeps a traced path's shape when it is handed on as
    /// fractions.
    fn aspect(&self) -> f64 {
        if self.rect.height() <= 0.0 {
            1.0
        } else {
            self.rect.width() / self.rect.height()
        }
    }

    fn press(&mut self, at: usize, point: Point) {
        let fraction = self.fraction(point);
        if let Some(on) = self.on_press.as_mut() {
            on(i64::try_from(at).unwrap_or(i64::MAX), fraction);
        }
    }

    fn hold_form(&mut self, at: usize, form: String) {
        if let Some(on) = self.on_hold.as_mut() {
            on(i64::try_from(at).unwrap_or(i64::MAX), form);
        }
    }

    fn caret_steps(&mut self, steps: i64) {
        if let Some(on) = self.on_caret.as_mut() {
            on(steps);
        }
    }

    fn swipe(&mut self, path: &[Point]) {
        let flat: Vec<f64> = path
            .iter()
            .flat_map(|point| {
                let (x, y) = self.fraction(*point);
                [x, y]
            })
            .collect();
        let aspect = self.aspect();
        if let Some(on) = self.on_swipe.as_mut() {
            on(flat, aspect);
        }
    }

    /// The size a key is measured by for the swipe test: a letter key's average side.
    fn key_scale(&self) -> f64 {
        let letters: Vec<Rect> = self
            .cells
            .iter()
            .zip(&self.keys)
            .filter(|(_, key)| is_letter(key))
            .map(|(rect, _)| *rect)
            .collect();
        if letters.is_empty() {
            return self.row_height.max(1.0);
        }
        let sum: f64 = letters
            .iter()
            .map(|rect| (rect.width() + rect.height()) * 0.5)
            .sum();
        sum / letters.len() as f64
    }

    /// Whether a path crossed the keys far enough to be a swipe rather than a tap that wobbled.
    fn is_swipe(&self, path: &[Point]) -> bool {
        let (Some(first), Some(last)) = (path.first(), path.last()) else {
            return false;
        };
        let travel: f64 = path
            .windows(2)
            .map(|pair| {
                pair.first()
                    .zip(pair.last())
                    .map_or(0.0, |(a, b)| (*b - *a).hypot())
            })
            .sum();
        let span = (*last - *first).hypot();
        let scale = self.key_scale();
        travel > scale * 1.5 && span > scale * 0.75
    }

    fn begin_hold(&mut self, key: usize, finger: Option<u64>, now: Instant) {
        let Some(at) = self.keys.get(key) else {
            return;
        };
        let (kind, delay) = if at.repeat {
            (HoldKind::Repeat, REPEAT_DELAY)
        } else if !self.secret && at.holds.len() > 1 {
            (HoldKind::Alternates, LONG_PRESS)
        } else {
            return;
        };
        self.hold = Some(Hold {
            key,
            finger,
            due: now + delay,
            kind,
            fired: false,
        });
    }

    fn poll_hold(&mut self, now: Instant) -> bool {
        let Some(hold) = self.hold else {
            return false;
        };
        if now < hold.due {
            return false;
        }
        self.cancel_stale();
        let Some(hold) = self.hold.as_mut() else {
            return false;
        };
        if now < hold.due {
            return false;
        }
        let (key, finger, kind) = (hold.key, hold.finger, hold.kind);
        match kind {
            HoldKind::Repeat => {
                hold.fired = true;
                hold.due = now + REPEAT_INTERVAL;
                self.spend(finger);
                let centre = self
                    .cells
                    .get(key)
                    .map_or(Point::ZERO, |rect| rect.center());
                self.press(key, centre);
            }
            HoldKind::Alternates => {
                self.hold = None;
                let options = self
                    .keys
                    .get(key)
                    .map(|at| at.holds.clone())
                    .unwrap_or_default();
                let Some(rect) = self.cells.get(key).copied() else {
                    return false;
                };
                let mut alternates = Alternates::new(key, rect, options, self.band);
                alternates.finger = finger;
                self.alternates = Some(alternates);
                self.spend(finger);
            }
        }
        self.dirty = true;
        true
    }

    fn spend(&mut self, finger: Option<u64>) {
        if let Some(at) = finger.and_then(|id| self.fingers.iter_mut().find(|f| f.id == id)) {
            at.spent = true;
        }
    }

    /// Drop what belonged to a contact in geometry that has moved: a release must not type on the
    /// keys that took the old one's place.
    fn cancel_stale(&mut self) {
        if self
            .contact
            .as_ref()
            .is_some_and(|geometry| *geometry != self.cells)
        {
            self.cancel_contacts();
        }
    }

    fn cancel_contacts(&mut self) {
        self.fingers.clear();
        self.contact = None;
        self.hold = None;
        self.alternates = None;
        self.cursor = None;
        self.gesture = None;
        self.pointer = None;
        self.pressed = None;
        self.hovered = None;
    }

    fn begin_cursor(&mut self, key: usize, finger: Option<u64>, x: f64) {
        if self.keys.get(key).is_some_and(|at| at.caret) {
            self.cursor = Some(CursorDrag {
                finger,
                anchor: x,
                active: false,
            });
        }
    }

    fn drag_cursor(&mut self, finger: Option<u64>, x: f64) -> bool {
        let scale = self.key_scale();
        let Some(drag) = self.cursor.as_mut().filter(|drag| drag.finger == finger) else {
            return false;
        };
        let started = !drag.active;
        if started {
            if (x - drag.anchor).abs() < scale * CURSOR_START {
                return false;
            }
            drag.active = true;
            drag.anchor = x;
        }
        let step = scale * CURSOR_STEP;
        let steps = ((x - drag.anchor) / step).trunc();
        drag.anchor += steps * step;
        if started {
            self.spend(finger);
            self.hold = None;
        }
        let count = i64::try_from(steps.abs() as usize).unwrap_or(i64::MAX);
        if count == 0 {
            return true;
        }
        let direction = if steps < 0.0 { -1 } else { 1 };
        self.caret_steps(direction * count);
        self.dirty = true;
        true
    }

    fn cursor_active(&self) -> bool {
        self.cursor.as_ref().is_some_and(|drag| drag.active)
    }

    fn track_alternates(&mut self, finger: Option<u64>, x: f64) -> bool {
        if let Some(alternates) = self.alternates.as_mut().filter(|row| row.finger == finger) {
            alternates.track(x);
            self.dirty = true;
            return true;
        }
        false
    }

    fn commit_alternate(&mut self) {
        let Some(row) = self.alternates.take() else {
            return;
        };
        if let Some(form) = row.chosen() {
            let form = form.to_owned();
            self.hold_form(row.key, form);
        }
    }

    fn type_finger(&mut self, finger: &Finger) {
        if finger.solo && self.is_swipe(&finger.path) {
            self.swipe(&finger.path);
            return;
        }
        if let Some(at) = self.key_at(finger.at) {
            self.press(at, finger.at);
        }
    }

    /// One finger on the pad: it landed, moved, lifted or was cancelled at `point`. Whether the
    /// pad took it.
    fn finger(&mut self, id: u64, phase: TouchPhase, point: Point) -> bool {
        self.cancel_stale();
        match phase {
            TouchPhase::Started => {
                if !self.claims_raw_touch(point) {
                    return false;
                }
                self.contact = Some(self.cells.clone());
                // A second finger ends any hold, forms row or caret drag of the first: the keys
                // keep typing in press order.
                self.hold = None;
                self.alternates = None;
                self.cursor = None;
                let held: Vec<Finger> = self
                    .fingers
                    .iter_mut()
                    .filter(|finger| !finger.spent)
                    .map(|finger| {
                        finger.spent = true;
                        finger.clone()
                    })
                    .collect();
                for finger in &mut self.fingers {
                    finger.solo = false;
                }
                for finger in held {
                    self.type_finger(&finger);
                }
                self.fingers.retain(|finger| finger.id != id);
                let key = self.key_at(point);
                self.fingers.push(Finger {
                    id,
                    key,
                    at: point,
                    path: vec![point],
                    solo: self.fingers.is_empty(),
                    spent: false,
                });
                if let Some(key) = key {
                    self.pressed = Some(key);
                    self.begin_hold(key, Some(id), now());
                    self.begin_cursor(key, Some(id), point.x);
                }
                self.dirty = true;
                true
            }
            TouchPhase::Moved => {
                let key = self.key_at(point);
                let Some(finger) = self.fingers.iter_mut().find(|finger| finger.id == id) else {
                    return false;
                };
                finger.at = point;
                finger.key = key;
                record(&mut finger.path, point);
                let cursor = self.drag_cursor(Some(id), point.x);
                let alternates = self.track_alternates(Some(id), point.x);
                if !cursor
                    && !alternates
                    && self
                        .hold
                        .is_some_and(|hold| hold.key != key.unwrap_or(usize::MAX))
                {
                    // Sliding off the key before the hold is due is a swipe, not a hold.
                    self.hold = None;
                }
                self.dirty = true;
                true
            }
            TouchPhase::Ended => {
                let Some(position) = self.fingers.iter().position(|finger| finger.id == id) else {
                    return false;
                };
                let finger_id = Some(id);
                let mut finger = self.fingers.remove(position);
                finger.at = point;
                record(&mut finger.path, point);
                if self
                    .alternates
                    .as_ref()
                    .is_some_and(|row| row.finger == finger_id)
                {
                    self.commit_alternate();
                } else if !finger.spent {
                    self.type_finger(&finger);
                }
                if self.hold.is_some_and(|hold| hold.finger == finger_id) {
                    self.hold = None;
                }
                if self.cursor.is_some_and(|drag| drag.finger == finger_id) {
                    self.cursor = None;
                }
                self.dirty = true;
                true
            }
            TouchPhase::Cancelled => {
                let finger = Some(id);
                if self
                    .alternates
                    .as_ref()
                    .is_some_and(|row| row.finger == finger)
                {
                    self.alternates = None;
                }
                if self.hold.is_some_and(|hold| hold.finger == finger) {
                    self.hold = None;
                }
                if self.cursor.is_some_and(|drag| drag.finger == finger) {
                    self.cursor = None;
                }
                let before = self.fingers.len();
                self.fingers.retain(|held| held.id != id);
                self.dirty = true;
                self.fingers.len() != before
            }
        }
    }

    /// The mouse button went down at `point`: the key there is pressed until it lifts. Whether the
    /// pad took it.
    fn pointer_pressed(&mut self, point: Point) -> bool {
        if !self.rect.contains(point) {
            return false;
        }
        self.contact = Some(self.cells.clone());
        if let Some(key) = self.key_at(point) {
            self.pressed = Some(key);
            self.hovered = Some(key);
            let centre = self.cells[key].center();
            self.gesture = Some(vec![centre, point]);
            self.begin_hold(key, None, now());
            self.begin_cursor(key, None, point.x);
            self.dirty = true;
            return true;
        }
        false
    }

    /// The mouse moved to `point` while its button is down, or not.
    fn pointer_moved(&mut self, point: Point) -> bool {
        self.pointer = Some(point);
        if let Some(path) = self.gesture.as_mut() {
            record(path, point);
        }
        let cursor = self.drag_cursor(None, point.x);
        let alternates = self.track_alternates(None, point.x);
        let key = self.key_at(point);
        if !cursor && !alternates {
            if self.pressed.is_some() && self.hold.is_some_and(|hold| Some(hold.key) != key) {
                self.hold = None;
            }
            if key != self.hovered {
                self.hovered = key;
                self.dirty = true;
            }
        }
        cursor || alternates || key.is_some()
    }

    /// The mouse button came up: the key it went down on types, or the path it crossed swipes.
    /// Whether the pad took it.
    fn pointer_lifted(&mut self) -> bool {
        self.cancel_stale();
        let path = self.gesture.take();
        let held = self.hold.take().filter(|hold| hold.finger.is_none());
        let steering = self
            .cursor
            .is_some_and(|drag| drag.finger.is_none() && drag.active);
        if self.cursor.is_some_and(|drag| drag.finger.is_none()) {
            self.cursor = None;
        }
        if self
            .alternates
            .as_ref()
            .is_some_and(|row| row.finger.is_none())
        {
            self.pressed = None;
            self.commit_alternate();
            return true;
        }
        if steering || held.is_some_and(|hold| hold.fired) {
            self.pressed = None;
            return true;
        }
        let Some(key) = self.pressed.take() else {
            return false;
        };
        let swiped = path
            .as_ref()
            .filter(|path| self.is_swipe(path))
            .map(|path| {
                self.swipe(path);
                true
            })
            .unwrap_or(false);
        if !swiped && self.hovered == Some(key) {
            let point = self.pointer.unwrap_or_else(|| self.cells[key].center());
            self.press(key, point);
        }
        true
    }

    fn preview(&self) -> Option<usize> {
        if self.secret || self.alternates.is_some() || self.cursor_active() {
            return None;
        }
        let key = self.pressed.or_else(|| {
            self.fingers
                .iter()
                .filter(|finger| !finger.spent)
                .filter_map(|finger| finger.key)
                .next_back()
        })?;
        let at = self.keys.get(key)?;
        (at.icon.is_empty() && at.label.chars().count() == 1).then_some(key)
    }

    /// The path a trail shows: a solo finger's, when it has crossed enough to be a swipe.
    fn trail(&self) -> &[Point] {
        if self.secret
            || self.alternates.is_some()
            || self.cursor_active()
            || self
                .contact
                .as_ref()
                .is_some_and(|geometry| *geometry != self.cells)
        {
            return &[];
        }
        let path = self
            .fingers
            .iter()
            .find(|finger| finger.solo && !finger.spent)
            .map(|finger| finger.path.as_slice())
            .or(self.gesture.as_deref())
            .unwrap_or_default();
        if self.is_swipe(path) { path } else { &[] }
    }

    fn activate(&mut self, at: usize) {
        let point = self.cells.get(at).map_or(Point::ZERO, |rect| rect.center());
        self.press(at, point);
        self.dirty = true;
    }
}

/// Whether a cell shows anything: an empty one is a key no row asked for.
fn shown(rect: Rect) -> bool {
    rect.width() > 0.0 && rect.height() > 0.0
}

fn is_letter(key: &PadKey) -> bool {
    key.icon.is_empty()
        && key.label.chars().count() == 1
        && key.label.chars().all(char::is_alphabetic)
}

fn record(path: &mut Vec<Point>, point: Point) {
    if path.last().is_none_or(|last| last.distance(point) > 1.0) {
        if path.len() < MAX_GESTURE_POINTS {
            path.push(point);
        } else if let Some(last) = path.last_mut() {
            *last = point;
        }
    }
}

fn key_pad(rect: Rect) -> f64 {
    (rect.height() * KEY_GAP_RATIO).clamp(2.0, 6.0)
}

fn alternates_cells(key_rect: Rect, count: usize, bounds: Rect) -> Vec<Rect> {
    let width = key_rect.width();
    let height = key_rect.height();
    let y0 = (key_rect.y0 - height).max(bounds.y0);
    // The first option (the plain letter) sits right above the finger, so a press released without
    // sliding, or with a small wobble, keeps it. The rest run towards the side with room.
    let span = width * count.saturating_sub(1) as f64;
    let room_right = bounds.x1 - key_rect.x1;
    let room_left = key_rect.x0 - bounds.x0;
    let step = if room_right >= span || room_right >= room_left {
        width
    } else {
        -width
    };
    let (lo, hi) = if step > 0.0 {
        (key_rect.x0, key_rect.x0 + span + width)
    } else {
        (key_rect.x0 - span, key_rect.x1)
    };
    let shift = if hi > bounds.x1 {
        bounds.x1 - hi
    } else if lo < bounds.x0 {
        bounds.x0 - lo
    } else {
        0.0
    };
    (0..count)
        .map(|at_| {
            let x = key_rect.x0 + shift + step * at_ as f64;
            Rect::new(x, y0, x + width, y0 + height)
        })
        .collect()
}

fn preview_rect(key_rect: Rect, bounds: Rect) -> Rect {
    let pad = key_pad(key_rect);
    let cell = key_rect.inset(-pad);
    let width = (cell.width() * 1.5).max(cell.width() + 16.0);
    let height = cell.height() * 1.25;
    let x0 = (cell.center().x - width * 0.5).clamp(bounds.x0, (bounds.x1 - width).max(bounds.x0));
    let y0 = (cell.y0 - height - pad).max(bounds.y0);
    Rect::new(x0, y0, x0 + width, y0 + height)
}

/// How far above its baseline the middle of a glyph's body sits, in em.
fn body_middle(ch: char) -> f64 {
    const X_HEIGHT: f64 = 0.51;
    const CAP_HEIGHT: f64 = 0.72;
    if ch.is_uppercase() || ch.is_numeric() {
        CAP_HEIGHT * 0.5
    } else {
        X_HEIGHT * 0.5
    }
}

fn key_fill(theme: &Theme, tone: PadTone, hovered: bool, pressed: bool) -> Color {
    let colors = &theme.tokens.colors;
    let ground = colors.surfaces.content;
    match tone {
        PadTone::Active | PadTone::Primary => {
            if pressed || hovered {
                colors.intent.accent_hover
            } else {
                colors.intent.accent
            }
        }
        PadTone::Plain => {
            tilo_ui::widget::face_colors(
                theme,
                tilo_ui::widget::ButtonRole::Key,
                Interaction {
                    hovered,
                    pressed,
                    ..Interaction::default()
                },
            )
            .0
        }
        PadTone::Modifier => {
            if pressed {
                colors.lift_on(ground, 1.0)
            } else if hovered {
                colors.lift_on(ground, 0.5)
            } else {
                ground
            }
        }
    }
}

fn draw_glyph(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    glyph: &str,
    content_px: f32,
    ink: Color,
) {
    let first = glyph.chars().next().unwrap_or(' ');
    let small = glyph.chars().count() == 1 && SMALL_MARKS.contains(first);
    let (px, weight) = if small {
        (content_px * SMALL_MARK_SCALE, TextWeight::Semibold)
    } else {
        (content_px, TextWeight::Medium)
    };
    let style = TextStyle::content(px).with_weight(weight);
    let (advance, _) = text.measure_style(glyph, None, &style);
    let baseline = text.first_baseline_style(glyph, None, &style);
    let middle = body_middle(first) * f64::from(content_px);
    let origin = Point::new(
        rect.center().x - f64::from(advance) * 0.5,
        rect.center().y + middle - f64::from(baseline),
    );
    text.draw_style(
        scene,
        glyph,
        ink,
        transform * Affine::translate(origin.to_vec2()),
        None,
        &style,
    );
    let _ = theme;
}

fn draw_word(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    word: &str,
    ink: Color,
) {
    let room = (rect.width() - 2.0 * key_pad(rect)).max(0.0) as f32;
    let style = theme.text(TextRole::Label);
    let fitted = text.elide_to_width_style(word, room, &style);
    let (width, height) = text.measure_style(&fitted, None, &style);
    let origin = Point::new(
        rect.center().x - f64::from(width) * 0.5,
        rect.center().y - f64::from(height) * 0.5,
    );
    text.draw_style(
        scene,
        &fitted,
        ink,
        transform * Affine::translate(origin.to_vec2()),
        None,
        &style,
    );
}

fn draw_label(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    ratio: f64,
    ink: Color,
) {
    if label.is_empty() {
        return;
    }
    if label.chars().count() == 1 {
        let label_px = theme.text(TextRole::Label).size;
        let px = ((rect.height() * ratio).min(rect.width() * 0.85) as f32).max(label_px);
        draw_glyph(scene, transform, text, theme, rect, label, px, ink);
    } else {
        draw_word(scene, transform, text, theme, rect, label, ink);
    }
}

fn draw_float_panel(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) -> f64 {
    let radius = f64::from(theme.tokens.radii.panel);
    tilo_ui_base::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    tilo_ui_base::surface::panel(scene, transform, rect, radius, theme);
    radius
}

fn draw_swipe_trail(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    points: &[Point],
    bounds: Rect,
) {
    if points.len() < 2 {
        return;
    }
    let width = f64::from(theme.tokens.spacing.s1);
    let color = theme.tokens.colors.text_primary;
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &bounds);
    for pair in points.windows(2) {
        let (Some(start), Some(end)) = (pair.first(), pair.last()) else {
            continue;
        };
        let delta = *end - *start;
        let length = delta.hypot();
        let turned =
            transform * Affine::translate(start.to_vec2()) * Affine::rotate(delta.y.atan2(delta.x));
        scene.fill(
            Fill::NonZero,
            turned,
            color,
            None,
            &Rect::new(
                -width / 2.0,
                -width / 2.0,
                length + width / 2.0,
                width / 2.0,
            ),
        );
    }
    scene.pop_layer();
}

impl Widget for KeyPad {
    tilo_ui_base::described!();

    fn name(&self) -> &'static str {
        "KeyPad"
    }

    fn test_id(&self) -> Option<&str> {
        Some("key_pad")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let wanted_row = f64::from(theme.tokens.metrics.header_height);
        // Any change of the theme's row height places the keys again: an exact reading.
        if wanted_row.to_bits() != self.wanted_row.to_bits() {
            self.wanted_row = wanted_row;
            self.placed = false;
        }
        // The keys' rows are the pad's own height, whatever box holds it: a box that sizes to its
        // content gives it that, and one with room to spare lets it grow, docking the keys at
        // the bottom.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a few rows of keys are far from f32::MAX"
        )]
        let height = (self.wanted_row * f64::from(self.size.scale()) * self.rows() as f64) as f32;
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::length(height),
            },
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || !self.placed
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
            self.cancel_contacts();
            self.placed = false;
        }
        if !self.placed {
            self.place_rows();
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(unused_variables, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, _env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 || self.keys.is_empty() {
            return;
        }
        self.place(theme);
        tilo_ui_base::surface::region(
            scene,
            transform,
            self.rect,
            tilo_ui_base::surface::Region::Content,
            theme,
        );
        let capslock = self.caps;
        let colors = theme.tokens.colors.clone();
        let fingers: Vec<usize> = self
            .fingers
            .iter()
            .filter(|finger| !finger.spent)
            .filter_map(|finger| finger.key)
            .collect();
        let preview = self.preview();
        let trail: Vec<Point> = self.trail().to_vec();
        let cursor_active = self.cursor_active();
        for (at, rect) in self.cells.iter().copied().enumerate() {
            let Some(key) = self.keys.get(at) else {
                continue;
            };
            if !shown(rect) {
                continue;
            }
            let hovered = self.hovered == Some(at);
            let pressed = self.pressed == Some(at) || fingers.contains(&at);
            let cell = rect.inset(-key_pad(rect));
            let radius = f64::from(theme.tokens.radii.control);
            tilo_ui_base::surface::card(
                scene,
                transform,
                cell,
                radius,
                key_fill(theme, key.tone, hovered, pressed),
            );
            let ink = match key.tone {
                PadTone::Active | PadTone::Primary => colors.intent.on_accent,
                _ if cursor_active && is_letter(key) => colors.text_dim,
                _ => colors.text_primary,
            };
            if let Some(icon) = tilo_ui_base::icons::Icon::from_name(&key.icon) {
                let side = f64::from(theme.tokens.metrics.icon_md);
                let icon_rect = Rect::from_center_size(cell.center(), (side, side));
                let filled = key.tone == PadTone::Active && capslock;
                if filled {
                    icon.draw_filled(scene, transform, icon_rect, ink);
                } else {
                    tilo_ui_base::icons::draw_icon(
                        scene,
                        transform,
                        icon_rect,
                        icon,
                        ink,
                        tilo_ui_base::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            } else {
                let label = if capslock && is_letter(key) {
                    key.label.to_uppercase()
                } else {
                    key.label.clone()
                };
                draw_label(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    cell,
                    &label,
                    KEY_GLYPH_RATIO,
                    ink,
                );
            }
        }
        if !trail.is_empty() {
            draw_swipe_trail(scene, transform, theme, &trail, self.band);
        }
        if let Some(alternates) = &self.alternates {
            if let (Some(first), Some(last)) = (alternates.cells.first(), alternates.cells.last()) {
                let panel_radius = draw_float_panel(scene, transform, theme, first.union(*last));
                for (at, (cell, option)) in
                    alternates.cells.iter().zip(&alternates.options).enumerate()
                {
                    let selected = at == alternates.selected;
                    let pad = key_pad(*cell);
                    let inner = cell.inset(-pad);
                    let ink = if selected {
                        let radius = f64::from(
                            theme
                                .tokens
                                .radii
                                .concentric(panel_radius as f32, pad as f32),
                        );
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            colors.intent.accent,
                            None,
                            &vello::kurbo::RoundedRect::from_rect(inner, radius),
                        );
                        colors.intent.on_accent
                    } else {
                        colors.text_primary
                    };
                    draw_label(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        inner,
                        option,
                        OPTION_GLYPH_RATIO,
                        ink,
                    );
                }
            }
        } else if let Some(at) = preview
            && let (Some(rect), Some(key)) = (self.cells.get(at), self.keys.get(at))
        {
            let bubble = preview_rect(*rect, self.rect);
            draw_float_panel(scene, transform, theme, bubble);
            let label = if capslock && is_letter(key) {
                key.label.to_uppercase()
            } else {
                key.label.clone()
            };
            draw_label(
                scene,
                transform,
                text_renderer,
                theme,
                bubble,
                &label,
                PREVIEW_GLYPH_RATIO,
                colors.text_primary,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.cancel_stale();
        if !self.enabled {
            return;
        }
        match event {
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                let took = self.finger(touch.id, touch.phase, point);
                *ctx.handled |= took;
                if took {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                *ctx.handled |= self.pointer_moved(ctx.logical(position));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.pointer {
                    *ctx.handled |= self.pointer_pressed(point);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                *ctx.handled |= self.pointer_lifted();
            }
            WindowEvent::Focused(false) => self.cancel_contacts(),
            _ => {}
        }
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.enabled && self.rect.contains(point) && self.key_at(point).is_some()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.hold.map(|hold| hold.due)
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        self.poll_hold(now)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if !self.enabled {
            return None;
        }
        let mut children = Vec::with_capacity(self.keys.len());
        for (at, rect) in self.cells.iter().enumerate() {
            let Some(key) = self.keys.get(at) else {
                continue;
            };
            if !shown(*rect) {
                continue;
            }
            let name = if key.name.is_empty() {
                key.label.clone()
            } else {
                key.name.clone()
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(*rect)
                    .with_state(AccessState::new().focusable(true))
                    .with_hashed_key(format!("key:{at}"))
                    .with_action_target(format!("keypad:key:{at}")),
            );
        }
        let node = AccessNode::new(Role::Group).with_children(children);
        let node = match &self.label {
            Some(label) => node.with_name(label.clone()),
            None => node,
        };
        Some(match &self.description {
            Some(why) => node.with_description(why.clone()),
            None => node,
        })
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessibilityAction,
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        if !matches!(action, AccessibilityAction::Activate) {
            return false;
        }
        let Some(rest) = target.strip_prefix("keypad:key:") else {
            return false;
        };
        let Ok(at) = rest.parse::<usize>() else {
            return false;
        };
        if at >= self.keys.len() {
            return false;
        }
        self.activate(at);
        true
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Default)
    }
}

/// The geometry helpers the tests and the drawing share.
#[cfg(test)]
mod tests;

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The on-screen keyboard's key pad, which a window builds by name beside the toolkit's
/// (`tilo_ui::component_catalogue::TOOLKIT`).
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: key_pad_enums,
};

fn key_pad_enums() -> Vec<tilo_ui_base::component::EnumDescription> {
    vec![key_pad_description()]
}
