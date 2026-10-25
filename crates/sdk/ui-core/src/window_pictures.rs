//! Live pictures of this device's own windows, drawn by their apps.
//!
//! Some chrome shows a window small somewhere else: a peek at what another
//! person has open shows the windows of theirs this device also holds, as
//! this device's own replica draws them. A thumbnail captured from the
//! screen would be a second way of showing a window; this is the same
//! drawing the window just made. A host says which windows it shows small
//! ([`want`]); while it does, the strip keeps a copy of each one's drawing
//! every time it draws or replays it, and the host paints that copy where
//! it wants ([`paint_fitted`]).
//!
//! A drawing is kept only when it stands on its own and shows nothing
//! private: one that drew pixels living outside the scene (a client's GPU
//! buffer, valid for one frame) or showed a secret (a password field) is
//! never kept, and the host shows the window's app mark instead. A blurred
//! material the window carried is left out of its picture: its backdrop is
//! the screen behind the window, which a miniature does not have.
//! Nothing is kept, and nothing costs more than one look at an empty list,
//! while no window is wanted.

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

/// One window's latest drawing.
#[derive(Clone)]
pub struct WindowPicture {
    /// The drawing, in the coordinates of the scene it was drawn into.
    pub scene: Scene,
    /// The window's rect in the coordinates of [`Self::scene`].
    pub rect: Rect,
    /// The strip showed the window when it last drew: the picture follows
    /// it. `false` once a draw left it off screen, where nothing draws it:
    /// the picture is how it last looked, and a host shows it as such.
    pub current: bool,
}

#[derive(Default)]
struct Pictures {
    /// Window group ids whose drawings are kept.
    wanted: Vec<u64>,
    held: Vec<(u64, WindowPicture)>,
    /// Newly wanted groups with no picture yet: the strip draws them on
    /// its next frame even when nothing in them changed, since a strip at
    /// rest is not drawn again and would never make one.
    owed: Vec<u64>,
}

thread_local! {
    static PICTURES: RefCell<Pictures> = RefCell::new(Pictures::default());
}

/// Keep the drawings of exactly `groups` (window group ids) from now on,
/// dropping every picture of a window no longer among them. Empty keeps
/// nothing.
pub fn want(groups: &[u64]) {
    PICTURES.with(|pictures| {
        let mut pictures = pictures.borrow_mut();
        let Pictures { wanted, held, owed } = &mut *pictures;
        owed.retain(|group| groups.contains(group));
        owed.extend(groups.iter().filter(|group| {
            !wanted.contains(group) && !held.iter().any(|(held, _)| held == *group)
        }));
        wanted.clear();
        wanted.extend_from_slice(groups);
        held.retain(|(group, _)| wanted.contains(group));
    });
}

/// Whether window group `group` was just wanted and has no picture yet:
/// its strip draws again for one.
pub(crate) fn owed(group: u64) -> bool {
    PICTURES.with(|pictures| pictures.borrow().owed.contains(&group))
}

/// Whether any newly wanted window still waits for its first picture: a
/// host showing pictures asks for one more frame while it does.
#[must_use]
pub fn owes_any() -> bool {
    PICTURES.with(|pictures| !pictures.borrow().owed.is_empty())
}

/// A strip drew, or left off screen, window groups `groups`: none of them
/// is owed a draw any more, pictured or not (one off screen, or showing a
/// secret, gets none, and asking again would draw it every frame).
pub(crate) fn settled<'a>(groups: impl IntoIterator<Item = &'a u64>) {
    PICTURES.with(|pictures| {
        let mut pictures = pictures.borrow_mut();
        if pictures.owed.is_empty() {
            return;
        }
        for group in groups {
            pictures.owed.retain(|owed| owed != group);
        }
    });
}

/// Whether a picture of window group `group` is wanted.
pub(crate) fn wanted(group: u64) -> bool {
    PICTURES.with(|pictures| {
        let pictures = pictures.borrow();
        !pictures.wanted.is_empty() && pictures.wanted.contains(&group)
    })
}

/// Keep `scene` as the picture of window group `group`, whose rect in the
/// scene's coordinates is `rect`, when it is wanted.
pub(crate) fn keep(group: u64, scene: &Scene, rect: Rect) {
    if !wanted(group) {
        return;
    }
    PICTURES.with(|pictures| {
        let mut pictures = pictures.borrow_mut();
        let picture = WindowPicture {
            scene: scene.clone(),
            rect,
            current: true,
        };
        match pictures.held.iter_mut().find(|(held, _)| *held == group) {
            Some((_, held)) => *held = picture,
            None => pictures.held.push((group, picture)),
        }
    });
}

/// Drop the picture of window group `group`: its latest drawing may not be
/// shown small, so an older one must not be either.
pub(crate) fn forget(group: u64) {
    PICTURES.with(|pictures| {
        pictures
            .borrow_mut()
            .held
            .retain(|(held, _)| *held != group);
    });
}

/// The window groups of `groups` were left off screen by a draw: their
/// pictures stay, as they last looked ([`WindowPicture::current`]), until
/// the strip draws them again.
pub(crate) fn unseen<'a>(groups: impl IntoIterator<Item = &'a u64>) {
    PICTURES.with(|pictures| {
        let mut pictures = pictures.borrow_mut();
        if pictures.held.is_empty() {
            return;
        }
        for group in groups {
            if let Some((_, picture)) = pictures.held.iter_mut().find(|(held, _)| held == group) {
                picture.current = false;
            }
        }
    });
}

/// `f` of the latest picture of window group `group`, when one is kept.
pub fn with_picture<R>(group: u64, f: impl FnOnce(&WindowPicture) -> R) -> Option<R> {
    PICTURES.with(|pictures| {
        pictures
            .borrow()
            .held
            .iter()
            .find(|(held, _)| *held == group)
            .map(|(_, picture)| f(picture))
    })
}

/// Where `picture` goes fitted inside `into`: as large as fits, keeping
/// its shape, centred.
#[must_use]
pub fn fitted_rect(picture: &WindowPicture, into: Rect) -> Rect {
    let (w, h) = (picture.rect.width(), picture.rect.height());
    if w <= 0.0 || h <= 0.0 || into.width() <= 0.0 || into.height() <= 0.0 {
        return Rect::ZERO;
    }
    let scale = (into.width() / w).min(into.height() / h);
    let (fw, fh) = (w * scale, h * scale);
    let center = into.center();
    Rect::new(
        center.x - fw / 2.0,
        center.y - fh / 2.0,
        center.x + fw / 2.0,
        center.y + fh / 2.0,
    )
}

/// Paint `picture` fitted inside `into` ([`fitted_rect`]) through
/// `transform`, clipped to where it lands; returns that rect, empty when
/// the picture or `into` has no area.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication composes finite floating-point transforms"
)]
pub fn paint_fitted(
    picture: &WindowPicture,
    scene: &mut Scene,
    transform: Affine,
    into: Rect,
) -> Rect {
    let covered = fitted_rect(picture, into);
    if covered.area() <= 0.0 {
        return Rect::ZERO;
    }
    let scale = covered.width() / picture.rect.width();
    let placed = transform
        * Affine::translate((covered.x0, covered.y0))
        * Affine::scale(scale)
        * Affine::translate((-picture.rect.x0, -picture.rect.y0));
    scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        transform,
        &covered,
    );
    scene.append(&picture.scene, Some(placed));
    scene.pop_layer();
    covered
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::peniko::Color;

    fn drawn() -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, 10.0, 10.0),
        );
        scene
    }

    #[test]
    fn only_wanted_windows_are_kept_and_unwanting_drops_them() {
        want(&[]);
        keep(1, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        assert!(with_picture(1, |_| ()).is_none(), "nothing wanted");
        want(&[1, 2]);
        keep(1, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        keep(3, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        assert!(with_picture(1, |picture| picture.rect.width()) == Some(200.0));
        assert!(with_picture(3, |_| ()).is_none(), "3 is not wanted");
        forget(1);
        assert!(with_picture(1, |_| ()).is_none());
        keep(1, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        want(&[2]);
        assert!(with_picture(1, |_| ()).is_none(), "unwanted, dropped");
        want(&[]);
    }

    /// A window left off screen keeps its picture as it last looked, until
    /// a draw shows it again.
    #[test]
    fn a_window_off_screen_is_pictured_as_it_last_looked() {
        want(&[4]);
        assert!(owed(4) && owes_any(), "a new want is owed a draw");
        keep(4, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        settled(&[4]);
        assert!(!owes_any(), "drawn, it owes nothing");
        want(&[4]);
        assert!(!owed(4), "still wanted and pictured: nothing owed");
        assert_eq!(with_picture(4, |picture| picture.current), Some(true));
        unseen(&[4, 5]);
        assert_eq!(with_picture(4, |picture| picture.current), Some(false));
        keep(4, &drawn(), Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(with_picture(4, |picture| picture.current), Some(true));
        want(&[]);
    }

    #[test]
    fn a_picture_fits_inside_keeping_its_shape() {
        let picture = WindowPicture {
            scene: drawn(),
            rect: Rect::new(100.0, 50.0, 500.0, 250.0),
            current: true,
        };
        let into = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            fitted_rect(&picture, into),
            Rect::new(0.0, 25.0, 100.0, 75.0)
        );
        let mut scene = Scene::new();
        assert_eq!(
            paint_fitted(&picture, &mut scene, Affine::IDENTITY, into),
            Rect::new(0.0, 25.0, 100.0, 75.0)
        );
        assert!(!scene.encoding().is_empty());
        assert_eq!(
            paint_fitted(&picture, &mut Scene::new(), Affine::IDENTITY, Rect::ZERO),
            Rect::ZERO
        );
    }
}
