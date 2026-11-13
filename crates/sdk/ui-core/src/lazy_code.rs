//! A surface whose code loads on first use. A program built to fetch some of its code later (the
//! browser desktop, whose start module is what every visitor waits for) keeps surfaces such as History,
//! the palettes or Feedback out of the code it starts with. **The wait**, drawn with [`draw_loading`]:
//! a quiet panel where the surface will stand, so a person who asked for it never sees nothing.

pub use tilo_lock::lazy::LazyCode;

/// A surface built from code that may still be on its way: built at once when the code is in, otherwise
/// its arguments kept until it is.
pub enum LazySurface<T, A> {
    /// Waiting on the code, with what the surface is to be built from.
    Loading(A),
    /// Built.
    Ready(T),
}

impl<T, A> LazySurface<T, A> {
    /// The surface `make` builds from `args` with `code`, or `args` kept while `code` is still to come.
    pub fn new<F: Copy>(code: &LazyCode<F>, args: A, make: impl FnOnce(F, A) -> T) -> Self {
        match code.get() {
            Some(code) => Self::Ready(make(code, args)),
            None => Self::Loading(args),
        }
    }

    /// Build the surface once `code` is in.
    pub fn poll<F: Copy>(
        surface: &mut Option<Self>,
        code: &LazyCode<F>,
        make: impl FnOnce(F, A) -> T,
    ) -> bool {
        if !matches!(surface, Some(Self::Loading(_))) {
            return false;
        }
        let Some(code) = code.get() else {
            return false;
        };
        let Some(Self::Loading(args)) = surface.take() else {
            return false;
        };
        *surface = Some(Self::Ready(make(code, args)));
        true
    }

    /// The surface, once built.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(surface) => Some(surface),
            Self::Loading(_) => None,
        }
    }

    /// The surface, once built.
    pub fn ready_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Ready(surface) => Some(surface),
            Self::Loading(_) => None,
        }
    }

    /// Whether the surface waits on its code.
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading(_))
    }
}

/// Draw the quiet panel that stands in `rect` for a surface whose code is on its way: the raised
/// surface colour, and `label` (a toolkit message key) in the body text, inset as a panel's content is.
pub fn draw_loading(
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text: &mut crate::text_render::TextRenderer,
    theme: &crate::theme::Theme,
    rect: vello::kurbo::Rect,
    label: &str,
) {
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.raised,
        None,
        &rect,
    );
    let inset = f64::from(theme.padding_standard);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a panel's width is far inside f32"
    )]
    let room = (rect.width() - 2.0 * inset).max(0.0) as f32;
    text.draw_style(
        scene,
        &crate::i18n::widget_format(label, &[]),
        theme.text_dim,
        transform * vello::kurbo::Affine::translate((rect.x0 + inset, rect.y0 + inset)),
        Some(room),
        &theme.text(crate::tokens::TextRole::Body),
    );
}

#[cfg(test)]
mod tests {
    use super::{LazyCode, LazySurface};

    fn three() -> u32 {
        3
    }

    static LINKED: LazyCode<u32> = LazyCode::linked(three);
    static FETCHED: LazyCode<u32> = LazyCode::fetched();

    fn load() {}

    #[test]
    fn linked_code_builds_at_once_and_fetched_code_once_it_is_in() {
        let linked = LazySurface::new(&LINKED, 2, |code, args| code * args);
        assert_eq!(linked.ready(), Some(&6));

        FETCHED.defer(load);
        let mut fetched = Some(LazySurface::new(&FETCHED, 5, |code, args| code * args));
        assert!(fetched.as_ref().is_some_and(LazySurface::is_loading));
        assert!(!LazySurface::poll(
            &mut fetched,
            &FETCHED,
            |code, args| code * args
        ));
        FETCHED.install(4);
        assert!(LazySurface::poll(&mut fetched, &FETCHED, |code, args| code * args));
        assert_eq!(fetched.as_ref().and_then(LazySurface::ready), Some(&20));
        assert!(
            !LazySurface::poll(&mut fetched, &FETCHED, |code, args| code * args),
            "built once"
        );
    }
}
