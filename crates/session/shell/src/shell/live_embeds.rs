//! The windows the shell runs for live views of shared resources
//! ([`ui::window_share`]).
//!
//! A message that shares a window shows the resource as it is now, drawn by
//! its own app from this computer's copy of the record. Every app here runs
//! in a process of its own, so the shell starts the resource's app the way
//! it starts any app on a record, as a viewer of it: the workspace's grants
//! without authorship, so it reads what the person's copy holds and writes
//! nothing ([`crate::shell::launcher::queue_embed_launch`]). The launch's note
//! names the view, and the window it maps is never placed as a window: the
//! compositor configures it at the size a view is laid out at
//! ([`ui::window_share::host::VIEW`]), never focuses it or hands it input, and
//! keeps its frame clock running; the window that embeds the view draws that
//! window's latest buffer, scaled into the place it gave the view and
//! clipped to what of it may show ([`paint_over`]). It is the app itself
//! drawing, frame by frame, as the record syncs: not a picture taken once,
//! and not a video. Opening the resource is the embedding app's own action.
//!
//! When nothing embeds a view any more ([`NativeView`] dropped), its window
//! is asked to close, and the app ends as any app whose last window closed.
//!
//! The compositor and the chrome share one thread, which keeps this
//! registry.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ui::window_share::host::{LiveView, VIEW};
use ui::window_share::{EmbedState, ResourceView};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

/// How long a view's app has to show its window before the view says it
/// did not open.
const OPEN_DEADLINE: Duration = Duration::from_secs(20);

/// One view drawn over a window: the view's window, its app, and where it
/// goes in the embedding window's own logical coordinates, with what of it
/// may show.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Over {
    pub surface: u64,
    pub app: String,
    pub rect: Rect,
    pub clip: Rect,
}

#[derive(Default)]
struct Registry {
    next: u64,
    /// The views running, by key.
    wanted: HashSet<u64>,
    /// The window each view's app showed, by the view's key.
    bound: HashMap<u64, u64>,
    /// Why a view whose app went away shows nothing.
    ended: HashMap<u64, String>,
    /// Windows of views no longer wanted, until they are gone.
    closing: Vec<u64>,
    /// Of those, the ones the compositor has not asked to close yet.
    to_close: Vec<u64>,
    /// What each embedding window draws over itself, by its surface.
    over: HashMap<u64, Vec<Over>>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

/// One live view the shell runs: the launch of its app, and the window it
/// showed. Dropping it closes the window.
pub(crate) struct NativeView {
    key: u64,
    started: Instant,
}

impl NativeView {
    /// Start `view`'s app in `workspace` as a viewer of its record. Why it
    /// cannot start: its app is not installed here, or the record is not one
    /// of an app.
    pub(crate) fn start(
        view: &ResourceView,
        workspace: &crate::shell::state::Workspace,
    ) -> Result<Self, String> {
        let app = crate::shell::launcher::installed_apps_without_waiting()
            .into_iter()
            .find(|app| app.has_gui && app.package_id == view.app)
            .ok_or_else(|| crate::shell::i18n::t("live-view-not-installed"))?;
        let record = ui::record::RecordId::from_route(&view.record)
            .ok_or_else(|| crate::shell::i18n::t("live-view-gone"))?;
        // The app's own record of the workspace is the one it opens on its
        // own.
        let uri = (!record.is_package_primary()).then(|| record.uri());
        let key = REGISTRY.with_borrow_mut(|registry| {
            registry.next = registry.next.wrapping_add(1);
            registry.wanted.insert(registry.next);
            registry.next
        });
        let view = Self {
            key,
            started: Instant::now(),
        };
        crate::shell::launcher::queue_embed_launch(app, workspace, uri, key)
            .map_err(|error| error.user_message())?;
        Ok(view)
    }

    /// The window its app showed, once it did.
    pub(crate) fn surface(&self) -> Option<u64> {
        surface_of(self.key)
    }
}

impl LiveView for NativeView {
    fn state(&self) -> EmbedState {
        if let Some(reason) =
            REGISTRY.with_borrow(|registry| registry.ended.get(&self.key).cloned())
        {
            return EmbedState::Unavailable(reason);
        }
        match self.surface() {
            Some(surface) if crate::shell::window_previews::client_has_frame(surface) => {
                EmbedState::Showing
            }
            None if self.started.elapsed() >= OPEN_DEADLINE => {
                EmbedState::Unavailable(crate::shell::i18n::t("live-view-did-not-open"))
            }
            _ => EmbedState::Loading,
        }
    }

    /// A view is started once: a process is too dear to start again every
    /// few seconds, and one that would not open or closed would not again.
    fn retries(&self) -> bool {
        false
    }
}

impl Drop for NativeView {
    fn drop(&mut self) {
        let _ = REGISTRY.try_with(|registry| {
            let mut registry = registry.borrow_mut();
            registry.wanted.remove(&self.key);
            registry.ended.remove(&self.key);
            if let Some(surface) = registry.bound.remove(&self.key) {
                registry.closing.push(surface);
                registry.to_close.push(surface);
            }
        });
    }
}

/// `surface` is the window the launch of view `key` showed: it is the
/// view's from now on. A view no longer wanted gets its window closed at
/// once. Either way the window is never placed as one.
pub(crate) fn bind(key: u64, surface: u64) {
    REGISTRY.with_borrow_mut(|registry| {
        if registry.wanted.contains(&key) {
            registry.bound.insert(key, surface);
        } else {
            registry.closing.push(surface);
            registry.to_close.push(surface);
        }
    });
}

/// Whether `surface` is a view's window (or one closing): never a window of
/// its own, and kept drawing.
pub(crate) fn is_embed_surface(surface: u64) -> bool {
    REGISTRY
        .try_with(|registry| {
            let registry = registry.borrow();
            registry.bound.values().any(|bound| *bound == surface)
                || registry.closing.contains(&surface)
        })
        .unwrap_or(false)
}

/// `surface` went away: a view it showed says its app closed.
pub(crate) fn surface_gone(surface: u64) {
    let _ = REGISTRY.try_with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.closing.retain(|closing| *closing != surface);
        registry.to_close.retain(|closing| *closing != surface);
        let keys: Vec<u64> = registry
            .bound
            .iter()
            .filter(|(_, bound)| **bound == surface)
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            registry.bound.remove(&key);
            registry
                .ended
                .insert(key, crate::shell::i18n::t("live-view-closed"));
        }
    });
}

/// The windows of views no longer wanted, for the compositor to ask to
/// close.
pub(crate) fn take_closing() -> Vec<u64> {
    // They stay views' windows until they are gone, so nothing places
    // them in between.
    REGISTRY.with_borrow_mut(|registry| std::mem::take(&mut registry.to_close))
}

/// The window view `key`'s app showed.
fn surface_of(key: u64) -> Option<u64> {
    REGISTRY.with_borrow(|registry| registry.bound.get(&key).copied())
}

/// Draw exactly `over` from now on: what each embedding window (by its
/// surface) draws over itself. Returns whether that changed.
pub(crate) fn set_over(over: HashMap<u64, Vec<Over>>) -> bool {
    REGISTRY.with_borrow_mut(|registry| {
        let changed = registry.over != over;
        registry.over = over;
        changed
    })
}

/// A number that changes whenever a view drawn over `window` shows a new
/// frame, or the views over it change: its drawing is stale.
pub(crate) fn frames_over(window: u64) -> u64 {
    REGISTRY
        .try_with(|registry| {
            registry.borrow().over.get(&window).map_or(0, |over| {
                over.iter().fold(over.len() as u64, |sum, view| {
                    sum.wrapping_mul(31).wrapping_add(
                        crate::shell::window_previews::client_frame_seq(view.surface),
                    )
                })
            })
        })
        .unwrap_or(0)
}

/// Draw the views over `window` (a surface), through `transform`, with
/// `place` mapping the window's own logical coordinates to where its
/// picture is drawn: each laid out at [`VIEW`]'s size and scaled into its
/// place, clipped to what of it may show. A view withheld from captures is
/// left out of one.
pub(crate) fn paint_over(
    window: u64,
    scene: &mut Scene,
    transform: Affine,
    place: impl Fn(Rect) -> Rect,
) {
    let over = REGISTRY
        .try_with(|registry| registry.borrow().over.get(&window).cloned())
        .ok()
        .flatten()
        .unwrap_or_default();
    for view in over {
        if crate::shell::capture_protection::painting_for_capture()
            && crate::shell::foreign::store().is_capture_protected(view.surface, &view.app)
        {
            continue;
        }
        let rect = place(view.rect);
        let clip = place(view.clip).intersect(rect);
        if rect.is_zero_area() || clip.is_zero_area() {
            continue;
        }
        // The view keeps its own proportions from its place's corner, as
        // every desktop draws it.
        let scale = (rect.width() / VIEW.0).min(rect.height() / VIEW.1);
        let drawn = Rect::new(
            rect.x0,
            rect.y0,
            rect.x0 + VIEW.0 * scale,
            rect.y0 + VIEW.1 * scale,
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        let _ = crate::shell::window_previews::paint_client_frame(
            view.surface,
            scene,
            transform,
            drawn,
        );
        scene.pop_layer();
    }
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    REGISTRY.with_borrow_mut(|registry| *registry = Registry::default());
}

#[cfg(test)]
pub(crate) fn view_for_test() -> NativeView {
    let key = REGISTRY.with_borrow_mut(|registry| {
        registry.next = registry.next.wrapping_add(1);
        registry.wanted.insert(registry.next);
        registry.next
    });
    NativeView {
        key,
        started: Instant::now(),
    }
}

#[cfg(test)]
impl NativeView {
    pub(crate) fn key(&self) -> u64 {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_views_window_is_never_placed_and_closes_when_the_view_ends() {
        reset_for_test();
        let view = view_for_test();
        assert_eq!(view.state(), EmbedState::Loading);
        bind(view.key(), 41);
        assert!(is_embed_surface(41));
        assert_eq!(view.surface(), Some(41));
        assert_eq!(
            view.state(),
            EmbedState::Loading,
            "the window shows once it drew a frame"
        );
        drop(view);
        assert_eq!(take_closing(), vec![41], "an unwanted view's window closes");
        assert!(is_embed_surface(41), "and is never placed while it closes");
        assert!(take_closing().is_empty(), "it is asked once");
        surface_gone(41);
        assert!(!is_embed_surface(41));
    }

    #[test]
    fn a_window_for_a_view_no_longer_wanted_closes_at_once() {
        reset_for_test();
        let view = view_for_test();
        let key = view.key();
        drop(view);
        bind(key, 7);
        assert!(is_embed_surface(7));
        assert_eq!(take_closing(), vec![7]);
    }

    #[test]
    fn a_view_whose_app_closed_says_so() {
        reset_for_test();
        let view = view_for_test();
        bind(view.key(), 9);
        surface_gone(9);
        assert_eq!(
            view.state(),
            EmbedState::Unavailable(crate::shell::i18n::t("live-view-closed"))
        );
        assert!(!view.retries());
    }

    #[test]
    fn the_views_over_a_window_change_its_drawing() {
        reset_for_test();
        let before = frames_over(3);
        let over = Over {
            surface: 11,
            app: "so.tilo.notes".into(),
            rect: Rect::new(10.0, 10.0, 330.0, 210.0),
            clip: Rect::new(0.0, 0.0, 400.0, 400.0),
        };
        assert!(set_over(HashMap::from([(3, vec![over.clone()])])));
        assert!(!set_over(HashMap::from([(3, vec![over])])));
        assert_ne!(frames_over(3), before);
        let mut scene = Scene::new();
        paint_over(3, &mut scene, Affine::IDENTITY, |rect| rect);
    }
}
