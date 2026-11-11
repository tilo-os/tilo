//! Widget keys in the scene, for a host that keeps what it shows from frame
//! to frame by identity: a page that builds the scene as HTML elements.
//!
//! A [`part`](super::part) splits a section for damage within one frame's
//! recording. A widget key is the same idea carried *inside* the encoding
//! (`vello_encoding::Mark`): what a widget draws between its begin and end
//! marks belongs to it wherever the scene lands, so the key survives subtree
//! caches, `Scene::append` and transforms like the draws do. A host that
//! reconciles elements by key reuses one element per widget: a widget that
//! moved is one position write, and an inserted sibling shifts nothing.
//!
//! Keys are only written while a host asks for them ([`key_widgets`]); the
//! GPU path never reads them and draws exactly as before. The code that
//! writes them is reached only through the [`Keyer`] the host installs
//! ([`keyer`]), so a program that splits its code keeps it out of the start
//! module until a page asks for keys: the draw paths carry only the check.
//!
//! A key names a widget by its place: its parent's key, its index among its
//! parent's children and its type (or its test id, which names it alone).
//! The same tree drawn twice gives the same keys, and a widget rebuilt in
//! the same place keeps its key.

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
        picture: picture_keys,
    }
}

/// Have every widget this thread draws through the toolkit's draw paths
/// mark its draws with its key ([`begin_widget`]), with `keyer`'s code;
/// `None` stops it. Off by default.
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

/// One key open on the stack of widgets being drawn.
struct Open {
    key: u64,
    /// How many children have been keyed under it so far, for a child drawn
    /// without an index of its own.
    children: usize,
}

/// A scroll region a keyed widget drew this frame ([`Widget::primary_vertical_scroll`]).
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnScroll {
    /// The key of the widget whose region it is.
    pub key: u64,
    /// How far the content is scrolled, in the widget's own pixels.
    pub offset: f64,
    /// How tall the content is, and the part of it shown.
    pub content: f64,
    pub viewport: f64,
    /// The widget's rect, in scene coordinates.
    pub rect: Rect,
    /// How many scene pixels one of the widget's own pixels spans.
    pub scale: f64,
    /// Who drew it, compared by address alone ([`scroll_widget`]): never
    /// dereferenced.
    widget: (usize, &'static str),
    /// The keys of the widgets it was drawn inside.
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
    /// The widgets at the root of the frame keyed so far.
    /// By the scene they draw into and their type: a widget drawn at the
    /// root of another scene (a measuring pass, a drag's picture), or one
    /// of another type, does not renumber the frame's own.
    static ROOTS: RefCell<HashMap<(usize, &'static str), usize>> = RefCell::new(HashMap::new());
    /// The scroll regions keyed widgets drew, by key: kept across frames,
    /// since a frame that replays a cached subtree draws no widget in it
    /// again ([`retain_shown`]).
    static SCROLLS: RefCell<HashMap<u64, DrawnScroll>> = RefCell::new(HashMap::new());
    /// The key each widget drew under, by address and type, kept the same
    /// way.
    static DRAWN: RefCell<HashMap<(usize, &'static str), u64>> = RefCell::new(HashMap::new());
    /// The keys of the widgets that drew themselves as pictures, kept the
    /// same way.
    static PICTURES: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
}

/// Say that the widget being drawn is a picture: a chart, a drawing, a map,
/// a video frame. A host that shows the scene as elements shows it as one
/// image rather than an element per shape; its text stays text over it.
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

/// The keys of the widgets the last frame showed that drew themselves as
/// pictures ([`picture`], [`retain_shown`]).
#[must_use]
pub fn drawn_pictures() -> HashSet<u64> {
    PICTURES.with_borrow(Clone::clone)
}

/// Start keying a frame: root widgets count from the first again. A host
/// that keys widgets calls it before every frame it draws.
pub fn begin_frame() {
    if let Some(keyer) = installed() {
        (keyer.begin_frame)();
    }
}

fn begin_frame_keys() {
    ROOTS.with_borrow_mut(HashMap::clear);
    OPEN.with_borrow_mut(Vec::clear);
}

/// Forget the scroll regions and widgets whose keys `scene` (a whole frame)
/// no longer shows: what a cached subtree replayed into it keeps.
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
    PICTURES.with_borrow_mut(|pictures| pictures.retain(|key| shown.contains(key)));
}

/// The key `widget`'s draws carried in the frame keyed last, as the page
/// names it (16 hex digits); `None` when it was not drawn keyed. The widget
/// is matched by address and type, and nothing kept is dereferenced.
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

/// The key a draw of something under the innermost open key gets: `kind`
/// names the draw site and `id` tells its draws apart. For a widget that
/// draws many items itself (an editor's blocks, a list's rows), so each item
/// keeps an element of its own.
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

/// Begin `widget`'s draws: `index` is its place among its parent's children,
/// when the caller knows it (a container that skips children it does not
/// draw must say it, or the children after them would take their keys).
/// `transform` is the one it draws with. Writes nothing unless widgets are
/// keyed.
pub fn begin_widget(
    scene: &mut Scene,
    widget: &(impl Widget + ?Sized),
    index: Option<usize>,
    transform: Affine,
) -> Begun {
    let Some(keyer) = installed() else {
        return Begun(false);
    };
    // What the key needs of the widget, read here; the rest is one function
    // for every widget type rather than a copy per type.
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

/// What [`begin_widget`] reads of a widget.
struct Described<'a> {
    identity: (usize, &'static str),
    test_id: Option<&'a str>,
    rect: Rect,
    region: Option<crate::widget::PrimaryScrollRegion>,
}

fn begin_described(
    scene: &mut Scene,
    widget: &Described<'_>,
    index: Option<usize>,
    transform: Affine,
) {
    let scene_id = std::ptr::from_ref(&*scene) as usize;
    // A scene that holds no key yet starts its roots' count again: one
    // drawn into, reset and drawn into again in a frame (a layout that
    // settled on the second pass) numbers its roots as once.
    let fresh = scene.encoding().resources.marks.is_empty();
    let (key, ancestors) = OPEN.with_borrow_mut(|open| {
        let (parent, slot) = match open.last_mut() {
            Some(parent) => {
                let slot = index.unwrap_or(parent.children);
                parent.children = slot.saturating_add(1);
                (parent.key, slot)
            }
            None => ROOTS.with_borrow_mut(|roots| {
                if fresh {
                    roots.retain(|(drawn_into, _), _| *drawn_into != scene_id);
                }
                let count = roots.entry((scene_id, widget.identity.1)).or_default();
                let slot = index.unwrap_or(*count);
                *count = slot.saturating_add(1);
                (0, slot)
            }),
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        parent.hash(&mut hasher);
        if let Some(id) = widget.test_id {
            id.hash(&mut hasher);
        } else {
            slot.hash(&mut hasher);
            widget.identity.1.hash(&mut hasher);
        }
        let key = hasher.finish();
        let ancestors: Vec<u64> = open.iter().map(|open| open.key).collect();
        open.push(Open { key, children: 0 });
        (key, ancestors)
    });
    DRAWN.with_borrow_mut(|drawn| {
        drawn.insert(widget.identity, key);
    });
    // A picture drawn again says again that it is one.
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
            // A wrapper that answers for the region of a widget inside it (a
            // window, a tile, an app's root) is not where it scrolls: the
            // innermost widget that reports the region keeps it.
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

/// Count a child left undrawn, so the siblings drawn after it without an
/// index keep their keys.
pub fn skip_widget() {
    if let Some(keyer) = installed() {
        (keyer.skip)();
    }
}

fn skip_keys() {
    // A root left undrawn renumbers nothing: roots count by type.
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
        open.pop();
    });
    scene.encoding_mut().encode_mark(MarkKind::End);
}

/// Draw one item of a widget under `key` ([`item_key`]), standing at
/// `origin`. Writes no marks unless widgets are keyed.
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

/// Begin one item of a widget under `key` ([`item_key`]), standing at
/// `origin`, for draws that cannot be one closure; [`end_widget`] ends it.
pub fn begin_item(scene: &mut Scene, key: u64, origin: Affine) -> Begun {
    let Some(keyer) = installed() else {
        return Begun(false);
    };
    (keyer.begin_item)(scene, key, origin);
    Begun(true)
}

fn begin_item_keys(scene: &mut Scene, key: u64, origin: Affine) {
    OPEN.with_borrow_mut(|open| open.push(Open { key, children: 0 }));
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

/// Scroll the region `scroll` names to `offset`, in the widget's own
/// pixels, when the widget that drew it is still in `root`'s tree. Finds it
/// by address and type, never through the address kept. Returns whether it
/// was found.
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
