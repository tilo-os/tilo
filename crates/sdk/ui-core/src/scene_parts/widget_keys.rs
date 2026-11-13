//! Widget keys in the scene, for a host that keeps what it shows from frame to frame by
//! identity: a page that builds the scene as HTML elements. A [`part`](super::part)
//! splits a section for damage within one frame's recording. Keys are only written while
//! a host asks for them ([`key_widgets`]); the GPU path never reads them and draws
//! exactly as before.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello_encoding::{MarkKind, Transform};

use crate::widget::Widget;

/// The code that writes widget keys, handed to [`key_widgets`].
#[derive(Clone, Copy)]
pub struct Keyer {
    begin_frame: fn(),
    begin_widget: fn(&mut Scene, &Described<'_>, Option<usize>, Affine),
    begin_item: fn(&mut Scene, u64, Affine),
    item_key: fn(&'static str, u64) -> u64,
    end: fn(&mut Scene),
    skip: fn(),
    painted: fn((usize, &'static str)) -> Option<u64>,
    field: fn(&mut crate::text_render::TextRenderer, &FieldText<'_>),
    picture: fn(),
}

/// The keying code: the one way to it.
#[must_use]
pub fn keyer() -> Keyer {
    Keyer {
        begin_frame: begin_frame_keys,
        begin_widget: begin_described,
        begin_item: begin_item_keys,
        item_key: item_key_of,
        end: end_keys,
        skip: skip_keys,
        painted: painted_of,
        field: field_keys,
        picture: picture_keys,
    }
}

/// Have every widget this thread draws through the toolkit's draw paths mark its draws
/// with its key ([`begin_widget`]), with `keyer`'s code; `None` stops it.
pub fn key_widgets(keyer: Option<Keyer>) {
    KEYER.with(|slot| slot.set(keyer));
}

/// Whether widgets drawn on this thread mark their draws with their keys.
#[must_use]
pub fn keys_widgets() -> bool {
    KEYER.with(std::cell::Cell::get).is_some()
}

fn installed() -> Option<Keyer> {
    KEYER.with(std::cell::Cell::get)
}

struct Open {
    key: u64,
    children: usize,
    used_from: usize,
}

type SlotEntry = (usize, u64);

/// A scroll region a keyed widget drew this frame ([`Widget::primary_vertical_scroll`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnScroll {
    pub key: u64,
    /// How far the content is scrolled, in the widget's own pixels.
    pub offset: f64,
    pub content: f64,
    pub viewport: f64,
    pub rect: Rect,
    /// How many scene pixels one of the widget's own pixels spans.
    pub scale: f64,
    /// Who drew it, compared by address alone ([`scroll_widget`]): never dereferenced.
    widget: (usize, &'static str),
    ancestors: Vec<u64>,
}

impl DrawnScroll {
    fn same_region(&self, other: &Self) -> bool {
        self.offset.to_bits() == other.offset.to_bits()
            && self.content.to_bits() == other.content.to_bits()
            && self.viewport.to_bits() == other.viewport.to_bits()
    }
}

thread_local! {
    static KEYER: std::cell::Cell<Option<Keyer>> = const { std::cell::Cell::new(None) };
    static OPEN: RefCell<Vec<Open>> = const { RefCell::new(Vec::new()) };
    static USED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static ROOTS: RefCell<HashMap<(usize, &'static str), usize>> = RefCell::new(HashMap::new());
    static SCROLLS: RefCell<HashMap<u64, DrawnScroll>> = RefCell::new(HashMap::new());
    static DRAWN: RefCell<HashMap<(usize, &'static str), u64>> = RefCell::new(HashMap::new());
    static SLOTS: RefCell<HashMap<(u64, (usize, &'static str)), SlotEntry>> =
        RefCell::new(HashMap::new());
    static FIELDS: RefCell<HashMap<u64, DrawnField>> = RefCell::new(HashMap::new());
    static PICTURES: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

/// Say that the widget being drawn is a picture: a chart, a drawing, a map, a video
/// frame.
pub fn picture() {
    if let Some(keyer) = installed() {
        (keyer.picture)();
    }
}

fn picture_keys() {
    if let Some(key) = OPEN.with_borrow(|open| open.last().map(|open| open.key)) {
        PICTURES.with_borrow_mut(|pictures| pictures.insert(key));
    }
}

/// The keys of the widgets the last frame showed that drew themselves as pictures
/// ([`picture`], [`retain_shown`]).
#[must_use]
pub fn drawn_pictures() -> HashSet<u64> {
    PICTURES.with_borrow(Clone::clone)
}

/// The line of a single-line text field, as the field would set it ([`native_field`]).
pub struct FieldText<'a> {
    pub style: &'a crate::text_render::TextStyle,
    pub origin: Affine,
    pub area: Rect,
    pub color: vello::peniko::Color,
}

/// A single-line text field a keyed widget left to the host this frame: the host shows
/// the field itself (a page's `<input>`) where the widget would have drawn its text,
/// caret and selection.
#[derive(Clone, Debug)]
pub struct DrawnField {
    pub key: u64,
    pub area: Rect,
    pub baseline: vello::kurbo::Point,
    /// The face, its size in scene pixels and its variation coordinates.
    pub font: vello::peniko::FontData,
    pub size: f32,
    pub coords: Vec<vello::NormalizedCoord>,
    pub color: vello::peniko::Color,
}

/// Whether the host shows single-line text fields itself, as a page shows its own
/// `<input>`: then the field the innermost keyed widget is draws its chrome and
/// placeholder but not its text, caret or selection, and the host is told how the text
/// would have been set.
pub fn native_field(text: &mut crate::text_render::TextRenderer, field: &FieldText<'_>) -> bool {
    let Some(keyer) = installed() else {
        return false;
    };
    (keyer.field)(text, field);
    true
}

fn field_keys(text: &mut crate::text_render::TextRenderer, field: &FieldText<'_>) {
    let Some(key) = OPEN.with_borrow(|open| open.last().map(|open| open.key)) else {
        return;
    };
    let Some(face) = text.line_face(field.style) else {
        return;
    };
    let scale = field.origin.determinant().abs().sqrt();
    let drawn = DrawnField {
        key,
        area: field.area,
        baseline: field.origin * vello::kurbo::Point::new(0.0, f64::from(face.baseline)),
        font: face.font,
        size: (f64::from(face.size) * scale) as f32,
        coords: face.coords,
        color: field.color,
    };
    FIELDS.with_borrow_mut(|fields| fields.insert(key, drawn));
}

/// The fields the widgets the last frame showed left to the host ([`native_field`],
/// [`retain_shown`]).
#[must_use]
pub fn drawn_fields() -> Vec<DrawnField> {
    FIELDS.with_borrow(|fields| fields.values().cloned().collect())
}

/// Start keying a frame: root widgets count from the first again.
pub fn begin_frame() {
    if let Some(keyer) = installed() {
        (keyer.begin_frame)();
    }
}

fn begin_frame_keys() {
    ROOTS.with_borrow_mut(HashMap::clear);
    OPEN.with_borrow_mut(Vec::clear);
    USED.with_borrow_mut(Vec::clear);
}

/// Forget the scroll regions and widgets whose keys `scene` (a whole frame) no longer
/// shows: what a cached subtree replayed into it keeps.
pub fn retain_shown(scene: &Scene) {
    let shown: HashSet<u64> = scene
        .encoding()
        .resources
        .marks
        .iter()
        .filter_map(|mark| match mark.kind {
            MarkKind::Begin { key, .. } => Some(key),
            MarkKind::End => None,
        })
        .collect();
    SCROLLS.with_borrow_mut(|scrolls| scrolls.retain(|key, _| shown.contains(key)));
    DRAWN.with_borrow_mut(|drawn| drawn.retain(|_, key| shown.contains(key)));
    SLOTS.with_borrow_mut(|slots| slots.retain(|_, (_, key)| shown.contains(key)));
    FIELDS.with_borrow_mut(|fields| fields.retain(|key, _| shown.contains(key)));
    PICTURES.with_borrow_mut(|pictures| pictures.retain(|key| shown.contains(key)));
}

/// The key `widget`'s draws carried in the frame keyed last, as the page names it (16 hex
/// digits); `None` when it was not drawn keyed.
#[must_use]
pub fn painted_key(widget: &(impl Widget + ?Sized)) -> Option<String> {
    let keyer = installed()?;
    (keyer.painted)(identity(widget)).map(|key| format!("{key:016x}"))
}

fn painted_of(identity: (usize, &'static str)) -> Option<u64> {
    DRAWN.with_borrow(|drawn| drawn.get(&identity).copied())
}

/// The scroll regions of the widgets the last frame showed ([`retain_shown`]).
#[must_use]
pub fn drawn_scrolls() -> Vec<DrawnScroll> {
    SCROLLS.with_borrow(|scrolls| scrolls.values().cloned().collect())
}

/// The key a draw of something under the innermost open key gets: `kind` names the draw
/// site and `id` tells its draws apart.
#[must_use]
pub fn item_key(kind: &'static str, id: &impl Hash) -> u64 {
    installed().map_or(0, |keyer| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        id.hash(&mut hasher);
        (keyer.item_key)(kind, hasher.finish())
    })
}

fn item_key_of(kind: &'static str, id: u64) -> u64 {
    let parent = OPEN.with_borrow(|open| open.last().map_or(0, |open| open.key));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut hasher);
    kind.hash(&mut hasher);
    id.hash(&mut hasher);
    hasher.finish()
}

/// Whether a key was begun, for [`end_widget`].
#[must_use]
pub struct Begun(bool);

/// Begin `widget`'s draws: `index` is its place among its parent's children, when the
/// caller knows it (a container that skips children it does not draw must say it, or the
/// children after them would take their keys).
pub fn begin_widget(
    scene: &mut Scene,
    widget: &(impl Widget + ?Sized),
    index: Option<usize>,
    transform: Affine,
) -> Begun {
    let Some(keyer) = installed() else {
        return Begun(false);
    };
    (keyer.begin_widget)(
        scene,
        &Described {
            identity: identity(widget),
            test_id: widget.test_id(),
            rect: widget.get_rect(),
            region: widget.primary_vertical_scroll(),
        },
        index,
        transform,
    );
    Begun(true)
}

struct Described<'a> {
    identity: (usize, &'static str),
    test_id: Option<&'a str>,
    rect: Rect,
    region: Option<crate::widget::PrimaryScrollRegion>,
}

/// Whether `slot` is one of `used`, which is nearly always ascending: a container passes
/// its children's places in order, so a slot past the last is free without looking.
fn taken(used: &[usize], slot: usize) -> bool {
    used.last().is_some_and(|&last| slot <= last) && used.contains(&slot)
}

fn claim_slot(
    parent: u64,
    index: Option<usize>,
    identity: (usize, &'static str),
    used: &mut Vec<usize>,
    from: usize,
    next: &mut usize,
) -> usize {
    let remembered = index.or_else(|| {
        SLOTS
            .with_borrow(|slots| slots.get(&(parent, identity)).map(|entry| entry.0))
            .filter(|slot| !taken(&used[from..], *slot))
    });
    let slot = remembered.unwrap_or_else(|| {
        let mut slot = *next;
        while taken(&used[from..], slot) {
            slot = slot.saturating_add(1);
        }
        slot
    });
    *next = (*next).max(slot.saturating_add(1));
    used.push(slot);
    slot
}

fn begin_described(
    scene: &mut Scene,
    widget: &Described<'_>,
    index: Option<usize>,
    transform: Affine,
) {
    let scene_id = std::ptr::from_ref(&*scene) as usize;
    // A scene that holds no key yet starts its roots' count again: one drawn into, reset
    // and drawn into again in a frame (a layout that settled on the second pass) numbers
    // its roots as once.
    let fresh = scene.encoding().resources.marks.is_empty();
    let (key, ancestors) = OPEN.with_borrow_mut(|open| {
        let (parent, slot) = USED.with_borrow_mut(|used| {
            if let Some(parent) = open.last_mut() {
                let from = parent.used_from;
                let slot = claim_slot(
                    parent.key,
                    index,
                    widget.identity,
                    used,
                    from,
                    &mut parent.children,
                );
                (parent.key, slot)
            } else {
                ROOTS.with_borrow_mut(|roots| {
                    if fresh {
                        roots.retain(|(drawn_into, _), _| *drawn_into != scene_id);
                    }
                    let count = roots.entry((scene_id, widget.identity.1)).or_default();
                    let slot = claim_slot(0, index, widget.identity, used, 0, count);
                    (0, slot)
                })
            }
        });
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        parent.hash(&mut hasher);
        if let Some(id) = widget.test_id {
            id.hash(&mut hasher);
        } else {
            slot.hash(&mut hasher);
            widget.identity.1.hash(&mut hasher);
        }
        let key = hasher.finish();
        SLOTS.with_borrow_mut(|slots| {
            slots.insert((parent, widget.identity), (slot, key));
        });
        let ancestors: Vec<u64> = open.iter().map(|open| open.key).collect();
        let used_from = USED.with_borrow(Vec::len);
        open.push(Open {
            key,
            children: 0,
            used_from,
        });
        (key, ancestors)
    });
    DRAWN.with_borrow_mut(|drawn| {
        drawn.insert(widget.identity, key);
    });
    FIELDS.with_borrow_mut(|fields| fields.remove(&key));
    PICTURES.with_borrow_mut(|pictures| pictures.remove(&key));
    let rect = widget.rect;
    let origin = transform * Affine::translate((rect.x0, rect.y0));
    if let Some(region) = &widget.region {
        let scale = transform.determinant().abs().sqrt();
        let drawn = DrawnScroll {
            key,
            offset: region.metrics.offset,
            content: region.metrics.content,
            viewport: region.metrics.viewport,
            rect: transform.transform_rect_bbox(rect),
            scale,
            widget: widget.identity,
            ancestors,
        };
        SCROLLS.with_borrow_mut(|scrolls| {
            let inner = scrolls
                .values()
                .any(|other| other.ancestors.contains(&key) && other.same_region(&drawn));
            scrolls.retain(|outer, other| {
                !(drawn.ancestors.contains(outer) && other.same_region(&drawn))
            });
            if inner {
                scrolls.remove(&key);
            } else {
                scrolls.insert(key, drawn);
            }
        });
    } else {
        SCROLLS.with_borrow_mut(|scrolls| scrolls.remove(&key));
    }
    mark(scene, key, origin);
}

/// Count a child left undrawn, so the siblings drawn after it without an index keep their
/// keys.
pub fn skip_widget() {
    if let Some(keyer) = installed() {
        (keyer.skip)();
    }
}

fn skip_keys() {
    OPEN.with_borrow_mut(|open| {
        if let Some(parent) = open.last_mut() {
            parent.children = parent.children.saturating_add(1);
        }
    });
}

/// End the draws [`begin_widget`] began.
pub fn end_widget(scene: &mut Scene, begun: Begun) {
    if begun.0
        && let Some(keyer) = installed()
    {
        (keyer.end)(scene);
    }
}

fn end_keys(scene: &mut Scene) {
    OPEN.with_borrow_mut(|open| {
        if let Some(closed) = open.pop() {
            USED.with_borrow_mut(|used| used.truncate(closed.used_from));
        }
    });
    scene.encoding_mut().encode_mark(MarkKind::End);
}

/// Draw one item of a widget under `key` ([`item_key`]), standing at `origin`.
pub fn keyed<R>(
    scene: &mut Scene,
    key: u64,
    origin: Affine,
    draw: impl FnOnce(&mut Scene) -> R,
) -> R {
    let begun = begin_item(scene, key, origin);
    let result = draw(scene);
    end_widget(scene, begun);
    result
}

/// Begin one item of a widget under `key` ([`item_key`]), standing at `origin`, for draws
/// that cannot be one closure; [`end_widget`] ends it.
pub fn begin_item(scene: &mut Scene, key: u64, origin: Affine) -> Begun {
    let Some(keyer) = installed() else {
        return Begun(false);
    };
    (keyer.begin_item)(scene, key, origin);
    Begun(true)
}

fn begin_item_keys(scene: &mut Scene, key: u64, origin: Affine) {
    let used_from = USED.with_borrow(Vec::len);
    OPEN.with_borrow_mut(|open| {
        open.push(Open {
            key,
            children: 0,
            used_from,
        });
    });
    mark(scene, key, origin);
}

fn mark(scene: &mut Scene, key: u64, origin: Affine) {
    let c = origin.as_coeffs();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the encoding keeps transforms as f32, as it does every draw's"
    )]
    let origin = Transform {
        matrix: [c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32],
        translation: [c[4] as f32, c[5] as f32],
    };
    scene
        .encoding_mut()
        .encode_mark(MarkKind::Begin { key, origin });
}

fn identity(widget: &(impl Widget + ?Sized)) -> (usize, &'static str) {
    (
        std::ptr::from_ref(widget).cast::<()>() as usize,
        widget.concrete_type_name(),
    )
}

/// Scroll the region `scroll` names to `offset`, in the widget's own pixels, when the
/// widget that drew it is still in `root`'s tree. Finds it by address and type, never
/// through the address kept.
pub fn scroll_widget(root: &mut dyn Widget, scroll: &DrawnScroll, offset: f64) -> bool {
    fn visit(widget: &mut dyn Widget, wanted: (usize, &'static str), offset: f64) -> bool {
        if identity(&*widget) == wanted {
            let _ = widget.apply_primary_vertical_scroll(
                crate::widget::PrimaryScrollAction::Absolute(offset),
            );
            return true;
        }
        widget
            .children_mut()
            .into_iter()
            .any(|child| visit(child, wanted, offset))
    }
    visit(root, scroll.widget, offset)
}

#[cfg(test)]
mod tests;
